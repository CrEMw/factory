# Document sync API

Repositories send snapshots of their design documents to the hub. The hub stores the texts, derives role and title,
links documents to tickets and to each other, and keeps the history. Code: `services/hub/src/documents.rs` (route),
`services/hub/src/callers.rs` (caller check), the SQL function `public.sync_repository_documents` (the snapshot
rules, in `services/hub/baseline/1000_baseline.sql`), and the client
`tools/doc-sync-client.mjs`.

## Setup

A repository can sync only when it is in the repository registry and has its own hub caller.

1. Registry: add the repository to `repositories.toml` in the seed directory (`HUB_SEED_DIR`). hub imports it at
   startup; the import adds and updates, never deletes.

   ```toml
   [[repository]]
   repo = "acme/widgets"     # owner/name
   project = "widgets"       # project of the documents
   caller = "docs-widgets"   # docs-<name>, unique
   ```

2. Caller: in the chart, add the name to `hub.callers` and its secret store key to
   `secrets.remoteKeys.callers` (see [configuration.md](configuration.md)). hub then accepts the secret from the file
   `<HUB_CALLERS_DIR>/docs-widgets`.

3. Client: run `tools/doc-sync-client.mjs` on every push to the main branch of the repository, with the same secret.

## POST /api/documents/sync

Headers:

```
Content-Type: application/json
X-Hub-Caller: docs-<name>
Authorization: Bearer <secret>
```

Callers whose name starts with `docs-` may call only this route, and only they may call it. Any other caller gets 403
`caller_not_allowed`; a `docs-*` caller on another route gets the same. A caller writes only the repository the registry
assigns to it.

Body, up to 32 MiB (larger: 413):

```json
{
  "repo": "acme/widgets",
  "revision": "<40 hex>",
  "ancestors": ["<40 hex>", "..."],
  "documents": [
    {"path": "docs/superpowers/specs/2026-01-01-search.md", "body": "# Search\n..."}
  ]
}
```

| Field | Rule |
|---|---|
| `repo` | `owner/name`, a repository from the registry. |
| `revision` | Full commit SHA, 40 lowercase hex. |
| `ancestors` | `git rev-list <revision>`: full SHAs, must contain `revision`. |
| `documents` | At most 5000 objects with `path` and `body`; paths unique. |
| `path` | `docs/superpowers/(intents\|specs\|plans)/.../<name>.md`, no `README.md`, no whitespace, control characters, backslashes, `.` or `..` segments. |
| `body` | Document text, up to 1 MiB in UTF-8. |

The hub derives the rest: role (`intent` for `intents/` and any `intent.md`, `spec` for `specs/`, otherwise `plan`),
title (the first `# ` heading, otherwise the file name), hash and links.

### Responses

| Status | Body | When |
|---|---|---|
| 200 | result object, below | The snapshot was applied. Sending the stored revision again is accepted and changes nothing when the texts are the same. |
| 400 | `{"error": "invalid_json"}` | The body is not a JSON object with a string `repo`. |
| 401 | text | Unknown caller or wrong secret. |
| 403 | `{"error": "caller_not_allowed"}` | The caller may not call this route. |
| 403 | `{"error": "repo_not_allowed"}` | `repo` is not the caller's repository (this includes a repository missing from the registry). |
| 409 | `{"error": "stale"}` | The revision already written is not in `ancestors`: a newer or unrelated revision is stored. |
| 413 | | Body over 32 MiB. |
| 422 | `{"error": "invalid_snapshot"}` | Bad revision or ancestors, more than 5000 documents, a document that is not an object, a duplicate path. |
| 422 | `{"error": "unknown_repo"}` | `repo` is not in the registry; reached only in open mode, without caller secrets. |
| 422 | `{"error": "invalid_json"}` | JSON that Postgres cannot store, for example text with `\u0000`. |

JSON error bodies, except `caller_not_allowed`, also carry `detail` with a message.

Result:

```json
{
  "repo": "acme/widgets",
  "revision": "<40 hex>",
  "documents": 12,
  "updated": 1,
  "retired": 0,
  "restored": 0,
  "links": {"opened": 1, "closed": 0},
  "edges": {"opened": 0, "closed": 0, "proposed": 0},
  "problems": [{"path": "...", "kind": "too_large", "detail": "..."}]
}
```

A problem in one document does not fail the snapshot: that document keeps its previous state and is listed in
`problems` (kinds include `bad_path`, `too_large`, `bad_body`, `unknown_code`; at most 200 entries). A `dangling_link`
problem is reported, but the document is still written.

### Semantics

- A snapshot is the full set of documents at `revision`. A document missing from it is retired; its ticket links
  are closed, not deleted, and come back if the file returns.
- Ticket links come from labelled lines with ticket codes in backquotes, using the words of `HUB_DOCUMENT_LINK_LABELS`
  (default: `Ticket`, `Tickets` for `describes`, `Closes` for `closes`). A `describes` label counts only on one of
  the first 20 lines of the text; a `closes` line (`Closes:`) counts anywhere.
- Explicit links to other documents in the registry become document edges.
- Everything runs in one transaction under the actor `service:<caller>`.

## GET /api/documents/{path}

Reads one document with its project, links to other documents and tickets. `?repo=owner/name` selects the
repository. Without it, hub uses `HUB_DEFAULT_DOCS_REPO`; when that is unset, the answer is 400 `repo_required`.

## tools/doc-sync-client.mjs

A Node.js script with no dependencies besides `git`. Repositories that sync carry a byte for byte copy of it; the
copy in this repository is canonical, with the test `tools/doc-sync-client.test.mjs`.

It collects `docs/superpowers/{intents,specs,plans}/**/*.md` (except `README.md`) at `REVISION` with
`git ls-tree` and `git cat-file`, skips files with a NUL byte (logged), takes `git rev-list REVISION` as ancestors and
posts the snapshot.

Environment:

| Variable | Default | Meaning |
|---|---|---|
| `HUB_URL` | none, required | Hub address, for example `http://hub-service:8000`. A trailing `/` is removed. |
| `HUB_CALLER` | none, required | Caller name, `docs-<name>`. |
| `HUB_CALLER_SECRET_FILE` | none, required | File with the caller secret. |
| `SOURCE_REPO` | none, required | `owner/name` of the repository. |
| `REVISION` | none, required | Full 40-hex commit SHA to send. |
| `REPO_DIR` | `.` | Clone with history up to `REVISION`. |
| `DOC_SYNC_DELAY_MS` | `20000` | Pause between attempts, in milliseconds. |

The document roots are fixed in the script; it reads no other variables.

Behavior:

- Waits for the secret file up to 3 times (about a minute with the default pause), then fails.
- Sends the request with a 60 second timeout, at most 7 attempts.
- 2xx: success. 409 with `{"error": "stale"}`: success, the hub already has a newer revision.
- No connection, 502, 503, or 404 without a JSON `error` field (the route is not rolled out yet): retry.
- Anything else (400, 401, 403, 422, 500, ...): exit with status 1 at once.

Example:

```sh
HUB_URL=http://hub-service:8000 \
HUB_CALLER=docs-widgets \
HUB_CALLER_SECRET_FILE=/var/run/secrets/hub-caller/secret \
SOURCE_REPO=acme/widgets \
REVISION="$(git rev-parse HEAD)" \
node tools/doc-sync-client.mjs
```
