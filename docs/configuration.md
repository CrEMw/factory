# Configuration

Reference of the environment variables of `hub` (Rust service) and `hub-web` (SvelteKit), and of the Helm values of
the chart in `deploy/chart`. Sources: `services/hub/src/config.rs` and the other `services/hub/src/*.rs` that read
env, `hub-web/src/lib/server`, `deploy/overlay/values.yaml` (the neutral chart defaults) and
`deploy/overlay/templates`.

An empty value counts as unset everywhere below unless noted.

## hub

### Installation settings

Read by both `hub migrate` and the `hub` service (`services/hub/src/config.rs`). An invalid value fails the command
before the first database query. `hub migrate` writes them into the `public.instance` row; the service checks env
against that row at startup.

| Variable | Default | Meaning |
|---|---|---|
| `HUB_ACCOUNT` | none, required | Account name, `^[a-z][a-z0-9-]{0,39}$`. Written to the database once; the service refuses to start if env and database disagree. |
| `HUB_TIMEZONE` | none, required | IANA time zone name such as `UTC`. Postgres must know it (`pg_timezone_names`). A zone different from the database logs a warning; the next `hub migrate` updates it. |
| `HUB_MAILBOX_PENDING_LIMIT` | `20` | Pending agent mailbox keys per owner, a positive integer. |
| `HUB_MAILBOX_PENDING_TTL` | unset: no limit | Lifetime of a pending mailbox key, a Postgres interval such as `7 days`. |
| `HUB_DOCUMENT_ROOTS` | `docs/superpowers` | Document roots in repositories, comma separated relative paths (no leading or trailing `/`, no `.` or `..`). Stored in `public.instance`; the document sync path check does not read it yet and accepts only `docs/superpowers/(intents\|specs\|plans)/...`. |
| `HUB_DOCUMENT_LINK_LABELS` | `{"describes":["Ticket","Tickets"],"closes":["Closes"],"and":["and"]}` | Words that introduce ticket links in documents. JSON object with exactly the keys `describes`, `closes` and `and`; each a list of 1 to 20 labels of 1 to 40 characters, without backquotes, control characters or spaces at the ends. |

### Service and migrations

| Variable | Default | Read by | Meaning |
|---|---|---|---|
| `HUBDB_URI` | none, required for the service | `hub`, `hub migrate` | Postgres connection URI. In the chart: the `uri` key of the secret `hub-db-hub` (role `hub`). |
| `HUBDB_MIGRATE_URI` | falls back to `HUBDB_URI` | `hub migrate` | Connection URI of the migration owner. In the chart: the `uri` key of the CNPG secret `hub-db-app`. |
| `PORT` | none, required | `hub` | HTTP port. The chart sets `8000` (`config.hub.PORT`). |
| `HUB_MIGRATIONS` | `chain` where the build has the chain, else `baseline` | `hub`, `hub migrate` | `chain` or `baseline`, see [migrations.md](migrations.md). Any other value is refused; `chain` too in a build without the chain. |
| `HUB_ADOPT_CHECKSUMS` | none | `hub migrate --adopt` | Expected journal of the migration chain, one `<version> <sha384 hex>` per line, see [migrations.md](migrations.md). |
| `HUB_CALLERS_DIR` | unset | `hub` | Directory of caller secrets: one file per caller, the file name is the caller name, the content its secret. Reread every 30 seconds. The chart mounts it at `/var/run/secrets/hub-callers`. |
| `HUB_CALLERS_REQUIRED` | `false` | `hub` | `true` or `1`: refuse to start without caller secrets. Without secrets and without this flag, `/api` is open to anyone who reaches the port (local runs only). The chart sets `true`. |
| `HUB_SEED_DIR` | unset | `hub` | Directory with seed files (`people.toml`, `components.toml`, `scopes.toml`, `repositories.toml`, `links/*.jsonl`), see `examples/seed`. |
| `HUB_SEED_REQUIRED` | `false` | `hub` | `true` or `1`: fail the start when the seed directory is missing or empty. |
| `HUB_DEFAULT_DOCS_REPO` | unset | `hub` | `owner/name` used by `GET /api/documents/<path>` without `?repo=`. Unset: such a request answers 400 `repo_required`. A bad shape fails the start. |

### Telemetry

| Variable | Default | Meaning |
|---|---|---|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | unset: no export | OTLP endpoint for traces, metrics and logs. Only the `http/protobuf` protocol is built in. |
| `OTEL_SDK_DISABLED` | unset | `true` turns export off even with an endpoint. |
| `OTEL_SERVICE_NAME` | `hub` | Service name in telemetry. `OTEL_RESOURCE_ATTRIBUTES` is read by the SDK as usual. |

## hub-web

| Variable | Default | Meaning |
|---|---|---|
| `ORIGIN` | none, required | External address of hub-web: the adapter-node origin, the Better Auth base URL and the MCP resource address (`<ORIGIN>/mcp`). Chart: values `origin`. |
| `BETTER_AUTH_SECRET` | none, required | Signing key of sessions and tokens. Chart: key `password` of the secret `hub-web-auth`. |
| `HUBDB_URI` | none, required | Postgres URI for the Better Auth tables (schema `auth`). Chart: key `uri` of the secret `hub-db-hub-web` (role `hub_web`). |
| `HUB_HTTP` | none, required | Address of the hub service. Chart: `http://hub-service:8000`. |
| `HUB_CALLER_SECRET_FILE` | unset | File with the caller secret hub-web sends to hub (caller name `hub-web`). Reread every 30 seconds. Unset: requests go without a secret (works only against a hub in open mode). Chart: `/var/run/secrets/hub-caller/secret`. |
| `PORT` | adapter-node default | HTTP port. The chart sets `8001`; `/mcp` also uses it to fetch its own JWKS from `127.0.0.1`. |
| `HUB_LOCALE` | `en` | Interface language: `en` or `ru` (Russian). Any other value stops startup. |
| `PUBLIC_HUB_NAME` | `factory hub` | Hub name in the page header and titles. |
| `HUB_APP_NAME` | value of `PUBLIC_HUB_NAME` | Better Auth application name. |
| `MCP_SERVER_NAME` | `factory-hub` | Name of the MCP server reported to clients. |
| `MCP_OPEN_CLIENT_REGISTRATION` | `true` | Dynamic MCP client registration without sign-in. `true`/`1` or `false`/`0`; anything else stops startup. A token is still issued only after sign-in and consent. |
| `TRUSTED_PROXY_CIDRS` | `10.42.0.0/16,10.43.0.0/16` | Networks of proxies in front of hub-web, comma separated CIDRs. Used to find the client address in `X-Forwarded-For` for rate limiting. A bad entry stops startup. |
| `MAILBOX_PATH_TOKENS` | unset: mailbox closed | Path tokens of the agent mailbox (`/api/mailbox/<token>`), comma separated; two during a rotation. Empty: every mailbox path answers 404. |
| `NODE_ENV` | | `production` in the chart. With `production`, `AUTH_DEV_LOGIN` is refused. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | unset: no export | As for hub. `OTEL_SDK_DISABLED=true` turns export off. |
| `OTEL_METRIC_EXPORT_INTERVAL` | `60000` | Metric export interval in milliseconds. |

Sign-in variables (`AUTH_*`) are described in [identity-providers.md](identity-providers.md).

## Helm values

Defaults from `deploy/overlay/values.yaml`, merged by `tools/chart.sh` into `deploy/chart/values.yaml`. An install
passes its own values file over them. Empty and `null` values are either not rendered or rendered as empty strings;
both count as unset, so the service default applies.

### General

| Key | Default | Meaning |
|---|---|---|
| `nameOverride` | `""` | Chart name in labels (`app.kubernetes.io/name`); empty: the chart name `factory`. Part of immutable Deployment selectors: an install that already runs under another name keeps it here. |
| `nodeSelector` | `{}` | Node selector of all pods and of the database. |
| `origin` | `""` | `ORIGIN` of hub-web. Required in practice: hub-web does not start without it. |
| `otel.endpoint` | `""` | `OTEL_EXPORTER_OTLP_ENDPOINT` of hub and hub-web; empty: no telemetry. |

### Generated by Aspire

These keys come from the AppHost (`apphost/apphost.mts`), not from the overlay.

| Key | Default | Meaning |
|---|---|---|
| `parameters.hub.hub_image` | `hub:latest` | Image of hub and of the Job `hub-db-migrate`. |
| `parameters.hub_web.hub_web_image` | `hub-web:latest` | Image of hub-web. |
| `config.hub.PORT` | `"8000"` | Port of hub. |
| `config.hub_web.*` | `NODE_ENV` (`production`), `PORT` (`"8001"`), `HUB_HTTP`, `services__hub__http__0`, `HOST`, `HOSTNAME` | Env of hub-web. |

### Secrets

The chart reads secrets through External Secrets Operator.

| Key | Default | Meaning |
|---|---|---|
| `secrets.store.kind` | `SecretStore` | Kind of the secret store referenced by every ExternalSecret. |
| `secrets.store.name` | `hub-secrets` | Name of the secret store. |
| `secrets.remoteKeys.dbHub` | `hub-db-hub` | Password of the database role `hub`. |
| `secrets.remoteKeys.dbHubWeb` | `hub-db-hub-web` | Password of the database role `hub_web`. |
| `secrets.remoteKeys.backupStorageKey` | `hub-backup-storage-key` | Storage account key for backups. |
| `secrets.remoteKeys.webAuthSecret` | `hub-web-auth-secret` | `BETTER_AUTH_SECRET` of hub-web. |
| `secrets.remoteKeys.accessTeamDomain` | `hub-web-access-team-domain` | Cloudflare Access team domain. |
| `secrets.remoteKeys.accessAud` | `hub-web-access-aud` | Cloudflare Access application audience. |
| `secrets.remoteKeys.cloudflaredToken` | `hub-cloudflared-token` | Run token of the Cloudflare tunnel. |
| `secrets.remoteKeys.mailboxPathToken` | `null` | Mailbox path tokens (comma separated). `null`: no ExternalSecret, the mailbox answers 404. |
| `secrets.remoteKeys.oidcClientSecret` | `null` | Client secret of the OIDC entrance, mounted as the file `AUTH_OIDC_CLIENT_SECRET_FILE` (ExternalSecret `hub-web-oidc`). `null`: none. |
| `secrets.remoteKeys.callers` | `{hub-web: hub-caller-hub-web}` | Secret store key per hub caller. `hub-web` is required; every name in `hub.callers` needs an entry. |

Changing a key makes ESO lose the secret.

### Database

CloudNativePG `Cluster` named `hub-db`.

| Key | Default | Meaning |
|---|---|---|
| `db.instances` | `1` | Number of Postgres instances. |
| `db.storage.size` | `5Gi` | Volume size. |
| `db.storage.class` | `standard` | Storage class. Cannot change on a live Cluster. |
| `db.resources.requests.cpu` | `100m` | |
| `db.resources.requests.memory` | `256Mi` | |
| `db.resources.limits.cpu` | `"1"` | |
| `db.resources.limits.memory` | `512Mi` | |
| `db.roleComments.hub` | `null` | `COMMENT ON ROLE` of `hub`; `null`: a neutral text. |
| `db.roleComments.hubWeb` | `null` | `COMMENT ON ROLE` of `hub_web`; `null`: a neutral text. |

### Backups

barman-cloud backups to Azure Blob Storage through the CNPG barman-cloud plugin. The `ObjectStore`,
`ScheduledBackup`, storage ExternalSecret and WAL archiver are rendered only when enabled.

| Key | Default | Meaning |
|---|---|---|
| `backup.enabled` | `false` | Turn backups on. |
| `backup.azure.account` | `""` | Storage account name. |
| `backup.destinationPath` | `""` | Destination path. Keep it byte for byte: a different path is a new archive, and the WAL chain breaks. |
| `backup.retention` | `14d` | Retention policy of backups and WAL. |
| `backup.schedule` | `"0 30 3 * * *"` | Base backup schedule (six-field cron). The first backup runs on creation. |

### Ingress

Two optional entrances to hub-web.

| Key | Default | Meaning |
|---|---|---|
| `ingress.tailscale.enabled` | `false` | Render the Tailscale Ingress. |
| `ingress.tailscale.className` | `tailscale` | Ingress class. |
| `ingress.tailscale.proxyGroup` | `""` | Tailscale proxy group (the `proxy-group` annotation of the Ingress). |
| `ingress.tailscale.host` | `""` | Host in `tls.hosts`. Keep it equal to `auth.providers.tailscale.host`. |
| `ingress.cloudflared.enabled` | `false` | Render the cloudflared connector (Deployment, Service, ExternalSecret). |
| `ingress.cloudflared.image` | the upstream `cloudflare/cloudflared` image, pinned by tag and digest | Connector image. |
| `ingress.cloudflared.tokenSecret` | `hub-cloudflared` | Kubernetes secret with the tunnel token (`TUNNEL_TOKEN`). |
| `ingress.cloudflared.protocol` | `http2` | Tunnel protocol (`--protocol`). |

### Sign-in

| Key | Default | Meaning |
|---|---|---|
| `auth.providers.cloudflareAccess.enabled` | `false` | Render the ExternalSecret `hub-web-access` and set `AUTH_CF_ACCESS_TEAM_DOMAIN` and `AUTH_CF_ACCESS_AUD` on hub-web. |
| `auth.providers.tailscale.host` | `""` | `AUTH_TAILSCALE_HOSTS` of hub-web. |
| `auth.extraOrigins` | `""` | `AUTH_EXTRA_ORIGINS` of hub-web. |

### hub and hub-web

| Key | Default | Meaning |
|---|---|---|
| `hub.callers` | `[]` | Callers of hub besides hub-web, for example `docs-<repo>` (a repository's document run). Each gets an ExternalSecret `hub-caller-<name>` and a file in the callers volume. |
| `hub.instance.account` | `null`, required | `HUB_ACCOUNT`. Rendering fails without it. |
| `hub.instance.timezone` | `null`, required | `HUB_TIMEZONE`. Rendering fails without it. |
| `hub.instance.mailboxPendingLimit` | `null` | `HUB_MAILBOX_PENDING_LIMIT`. |
| `hub.instance.mailboxPendingTtl` | `null` | `HUB_MAILBOX_PENDING_TTL`. |
| `hub.instance.documentRoots` | `[]` | `HUB_DOCUMENT_ROOTS`, joined with commas. |
| `hub.instance.documentLinkLabels` | `null` | `HUB_DOCUMENT_LINK_LABELS`, as a YAML map rendered to JSON. |
| `hub.migrations.source` | `baseline` | `HUB_MIGRATIONS` of the Job `hub-db-migrate` and of `hub`: `baseline` (one file, version 1000) or `chain` (the earlier migration chain, for a database that has not adopted the baseline yet). |
| `hub.migrations.adoptChecksums` | `null` | Expected journal of the chain, one `<version> <sha384 hex>` line each. Set: the Job runs `hub migrate --adopt` (needs `source: baseline`), see `docs/migrations.md`. |
| `hub.env` | `{}` | More hub env, for example `HUB_SEED_DIR`, `HUB_SEED_REQUIRED`, `HUB_DEFAULT_DOCS_REPO`. Not for secrets: a name with `SECRET`, `PASSWORD`, `TOKEN` or `PRIVATE_KEY` fails the render. |
| `web.env` | `{}` | More hub-web env, for example `HUB_LOCALE`, `PUBLIC_HUB_NAME`. Not for secrets, as `hub.env`. |

The `hub.instance` block renders the same env into the Job `hub-db-migrate` and the Deployment `hub`.

### Example

```yaml
origin: https://hub.example.com
hub:
  instance:
    account: acme
    timezone: UTC
  callers: [docs-widgets]
  env:
    HUB_DEFAULT_DOCS_REPO: acme/widgets
secrets:
  remoteKeys:
    callers:
      hub-web: hub-caller-hub-web
      docs-widgets: hub-caller-docs-widgets
web:
  env:
    HUB_LOCALE: en
```
