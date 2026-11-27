#!/usr/bin/env node
// Test of tools/doc-sync-client.mjs: a temporary git repo and a hub stub on 127.0.0.1.
// Run: node tools/doc-sync-client.test.mjs (tools/check.sh calls it: directly in repos with the copy, in the hub repo via
// tools/test-doc-sync-client.mjs). Copied byte for byte together with the client. The temporary directory is removed.
import { execFile, execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const client = join(dirname(fileURLToPath(import.meta.url)), 'doc-sync-client.mjs');
const tmp = mkdtempSync(join(tmpdir(), 'doc-sync-test-'));
let server;
try {
  const repo = join(tmp, 'repo');
  const git = (...a) =>
    execFileSync('git', ['-C', repo, '-c', 'user.name=t', '-c', 'user.email=t@t', ...a], { encoding: 'utf8' }).trim();
  const put = (path, text) => {
    mkdirSync(dirname(join(repo, path)), { recursive: true });
    writeFileSync(join(repo, path), text);
  };
  mkdirSync(repo);
  git('init', '-q');
  put('docs/superpowers/specs/a.md', '# A\n');
  git('add', '.');
  git('commit', '-qm', 'one');
  put('docs/superpowers/intents/b.md', '# B 161\n');
  put('docs/superpowers/plans/deep/c.md', '# C\n');
  put('docs/superpowers/plans/README.md', 'index\n');
  put('docs/superpowers/plans/notes.txt', 'not md\n');
  put('docs/other/d.md', '# D\n');
  put('docs/superpowers/specs/nul.md', '# N\0\n');
  git('add', '.');
  git('commit', '-qm', 'two');
  const revision = git('rev-parse', 'HEAD');
  const secretFile = join(tmp, 'secret');
  writeFileSync(secretFile, 's3cret\n');

  // Stub responses in order: [status, body] or 'drop' (connection reset); the last one repeats.
  let script = [];
  let requests = [];
  server = createServer((req, res) => {
    let data = '';
    req.on('data', (c) => (data += c));
    req.on('end', () => {
      requests.push({ url: req.url, method: req.method, headers: req.headers, body: data });
      const step = script.length > 1 ? script.shift() : script[0];
      if (step === 'drop') return req.socket.destroy();
      res.writeHead(step[0], step[1].startsWith('{') ? { 'content-type': 'application/json' } : {});
      res.end(step[1]);
    });
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const hub = `http://127.0.0.1:${server.address().port}`;

  const run = (responses, extraEnv = {}) => {
    script = responses;
    requests = [];
    return new Promise((resolve) => {
      execFile(
        process.execPath,
        [client],
        {
          env: {
            PATH: process.env.PATH,
            HUB_URL: `${hub}/`,
            HUB_CALLER: 'docs-widgets',
            HUB_CALLER_SECRET_FILE: secretFile,
            SOURCE_REPO: 'acme/widgets',
            REVISION: revision,
            REPO_DIR: repo,
            DOC_SYNC_DELAY_MS: '5',
            ...extraEnv,
          },
        },
        (err, stdout, stderr) => resolve({ code: err ? err.code : 0, out: stdout + stderr }),
      );
    });
  };
  const cases = [];
  const check = async (name, fn) => {
    try {
      await fn();
      cases.push(name);
    } catch (e) {
      console.error(`FAIL ${name}: ${e.message}`);
      process.exitCode = 1;
    }
  };

  await check('404 without JSON, then 200: retry and snapshot', async () => {
    const r = await run([[404, ''], [200, '{"documents":3}']]);
    assert.equal(r.code, 0, r.out);
    assert.equal(requests.length, 2);
    const q = requests[1];
    assert.equal(q.method, 'POST');
    assert.equal(q.url, '/api/documents/sync');
    assert.equal(q.headers['x-hub-caller'], 'docs-widgets');
    assert.equal(q.headers.authorization, 'Bearer s3cret');
    const b = JSON.parse(q.body);
    assert.equal(b.repo, 'acme/widgets');
    assert.equal(b.revision, revision);
    assert.equal(b.ancestors.length, 2);
    assert.equal(b.ancestors[0], revision);
    assert.deepEqual(
      b.documents.map((d) => d.path).sort(),
      ['docs/superpowers/intents/b.md', 'docs/superpowers/plans/deep/c.md', 'docs/superpowers/specs/a.md'],
    );
    assert.equal(b.documents.find((d) => d.path.endsWith('b.md')).body, '# B 161\n');
    assert.match(r.out, /skipped docs\/superpowers\/specs\/nul\.md: NUL/);
  });
  await check('409 stale: success without retry', async () => {
    const r = await run([[409, '{"error":"stale"}']]);
    assert.equal(r.code, 0, r.out);
    assert.equal(requests.length, 1);
  });
  await check('409 with another error: failure', async () => {
    const r = await run([[409, '{"error":"conflict"}']]);
    assert.equal(r.code, 1, r.out);
    assert.equal(requests.length, 1);
  });
  await check('404 with JSON error (repo not in the registry): failure without retry', async () => {
    const r = await run([[404, '{"error":"unsupported repository"}']]);
    assert.equal(r.code, 1, r.out);
    assert.equal(requests.length, 1);
  });
  await check('422: failure without retry', async () => {
    const r = await run([[422, '{"error":"invalid snapshot"}']]);
    assert.equal(r.code, 1, r.out);
    assert.equal(requests.length, 1);
  });
  await check('reset, then 200: retry', async () => {
    const r = await run(['drop', [200, '{}']]);
    assert.equal(r.code, 0, r.out);
    assert.equal(requests.length, 2);
  });
  await check('503 all the time: failure after 7 attempts', async () => {
    const r = await run([[503, 'unavailable']]);
    assert.equal(r.code, 1, r.out);
    assert.equal(requests.length, 7);
  });
  await check('no secret: failure without requests', async () => {
    const r = await run([[200, '{}']], { HUB_CALLER_SECRET_FILE: join(tmp, 'missing') });
    assert.equal(r.code, 1, r.out);
    assert.equal(requests.length, 0);
    assert.match(r.out, /no caller secret/);
  });
  if (!process.exitCode) console.log(`tests: passed ${cases.length} failed 0`);
} finally {
  server?.close();
  rmSync(tmp, { recursive: true, force: true });
}
