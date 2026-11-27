// Step 2.12c of the factory plan: the PGlite tests that do not depend on the history of the chain also pass on the
// baseline services/hub/baseline (HUB_MIGRATIONS=baseline, tools/hub-migrations.mjs). Since step 2.14 every PGlite
// test of the engine is neutral and seeds what it needs (registry, instance row); tests of what the chain wrote for an
// install (the 0010 backfill, the 0017 registry rows, the 0013 dictionary actors) are that install's own.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const files = ['document-graph', 'document-link-labels', 'graph', 'graph-import', 'instance', 'instance-account', 'people', 'plan',
  'plan-seed', 'repository-documents', 'seed-repos', 'ticket-status'].map((n) =>
  fileURLToPath(new URL(`test-${n}.mjs`, import.meta.url)));

test('PGlite tests on the baseline', () => {
  // Without NODE_TEST_CONTEXT the child is a test runner of its own and prints its report, not the parent protocol.
  const { NODE_TEST_CONTEXT: _, ...env } = process.env;
  const r = spawnSync(process.execPath, ['--test', '--test-concurrency=1', '--test-reporter=tap', ...files], {
    env: { ...env, HUB_MIGRATIONS: 'baseline' },
    encoding: 'utf8',
  });
  assert.equal(r.status, 0, r.stdout + r.stderr);
  assert.match(r.stdout, /^# pass [1-9]/m);
  assert.match(r.stdout, /^# fail 0$/m);
});
