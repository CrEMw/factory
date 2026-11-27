// Document sync client tools/doc-sync-client.mjs: its test tools/doc-sync-client.test.mjs
// (copied byte for byte to other repos) as a separate process, so that node --test tools/test-*.mjs sees a failure.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

test('doc-sync-client: hub stub and temporary git repo', () => {
  const file = fileURLToPath(new URL('./doc-sync-client.test.mjs', import.meta.url));
  const out = execFileSync(process.execPath, [file], { encoding: 'utf8' });
  assert.match(out, /tests: passed \d+ failed 0/);
});
