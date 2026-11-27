#!/usr/bin/env node
// Sync of a repo's documents into the hub graph. Canonical copy: tools/doc-sync-client.mjs of the hub repo;
// repos that sync their documents carry a byte-for-byte copy: change it in the hub repo and carry it over unchanged, with the test
// tools/doc-sync-client.test.mjs.
// Sends to POST /api/documents/sync a snapshot of docs/superpowers/{intents,specs,plans}/**/*.md (except README.md)
// at revision REVISION: paths and texts, the revision and its ancestors. The hub derives role, title, hash and links.
// Environment: HUB_URL, HUB_CALLER (docs-<repo>), HUB_CALLER_SECRET_FILE, SOURCE_REPO (<owner>/<repo>),
// REVISION (40 hex), REPO_DIR (a clone with history up to REVISION, the current directory by default),
// DOC_SYNC_DELAY_MS (pause between attempts, 20000 by default; the test sets less).
//
// Contract with the hub (responses to errors of the function public.sync_repository_documents):
//   2xx                               the snapshot was written or had been written already: success;
//   409 and JSON {"error": "stale"}   DSSTL, a newer revision is written: success, nothing to do;
//   404 without a JSON error field    no route yet (the hub is rolling out): retry;
//   502, 503, reset, timeout          the hub is restarting: retry;
//   anything else (400/422 DSINV, 403 DSCAL, 422 DSREP, 401, 500): fail at once.
// The wait is short (secret up to 1 minute, hub up to 2 minutes): the run shares the Repository queue
// (concurrency_limit: 1) with release, and the next push sends a full snapshot anyway.
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';

const env = (name, fallback) => {
  const v = process.env[name] ?? fallback;
  if (!v) throw new Error(`missing variable ${name}`);
  return v;
};
const hub = env('HUB_URL').replace(/\/$/, '');
const caller = env('HUB_CALLER');
const secretFile = env('HUB_CALLER_SECRET_FILE');
const repo = env('SOURCE_REPO');
const revision = env('REVISION');
const dir = env('REPO_DIR', '.');
const delay = Number(env('DOC_SYNC_DELAY_MS', '20000'));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
if (!/^[0-9a-f]{40}$/.test(revision)) throw new Error(`REVISION is not a full SHA: ${revision}`);

const git = (...args) => execFileSync('git', ['-C', dir, ...args], { encoding: 'utf8', maxBuffer: 1 << 28 });
const roots = ['intents', 'specs', 'plans'].map((r) => `docs/superpowers/${r}`);
const paths = git('ls-tree', '-r', '-z', '--name-only', revision, '--', ...roots)
  .split('\0')
  .filter((p) => p.endsWith('.md') && !p.endsWith('/README.md'));
// PostgreSQL jsonb cannot store NUL in text (22P05): such a document would fail the whole snapshot (422 invalid_json),
// so it is skipped with a log line.
const documents = [];
for (const path of paths) {
  const body = git('cat-file', 'blob', `${revision}:${path}`);
  if (body.includes('\0')) console.log(`skipped ${path}: NUL in text`);
  else documents.push({ path, body });
}
const ancestors = git('rev-list', revision).split('\n').filter(Boolean);
const body = JSON.stringify({ repo, revision, ancestors, documents });
console.log(`${repo}@${revision}: ${documents.length} documents, ${ancestors.length} ancestors, ${body.length} bytes`);

// ESO may create the secret after the run starts: wait for the file 3 attempts (about a minute), then fail.
for (let i = 0; !existsSync(secretFile) || !readFileSync(secretFile, 'utf8').trim(); i++) {
  if (i >= 3) {
    console.error(`no caller secret ${secretFile}`);
    process.exit(1);
  }
  console.log(`waiting for secret ${secretFile}`);
  await sleep(delay);
}
const secret = readFileSync(secretFile, 'utf8').trim();

const jsonError = (text) => {
  try {
    const v = JSON.parse(text);
    return v && typeof v === 'object' && typeof v.error === 'string' ? v.error : undefined;
  } catch {
    return undefined;
  }
};

// At most 7 attempts (about 2 minutes with a 20 s pause).
for (let attempt = 1; ; attempt++) {
  let status, text;
  try {
    const res = await fetch(`${hub}/api/documents/sync`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'x-hub-caller': caller, authorization: `Bearer ${secret}` },
      body,
      signal: AbortSignal.timeout(60_000),
    });
    status = res.status;
    text = await res.text();
  } catch (err) {
    status = 0;
    text = String(err?.cause ?? err);
  }
  console.log(`attempt ${attempt}: ${status || 'no connection'} ${text}`);
  if (status >= 200 && status < 300) break;
  const error = jsonError(text);
  if (status === 409 && error === 'stale') {
    console.log('the hub already has a newer revision than this one: nothing to do');
    break;
  }
  const retry = status === 0 || status === 502 || status === 503 || (status === 404 && error === undefined);
  if (!retry || attempt >= 7) process.exit(1);
  await sleep(delay);
}
