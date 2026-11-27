// Step 2.12b of the factory plan: tools/canonical-bodies.sh keeps the stale check meaningful after 0028 is frozen.
// The canonical migrations (0028 and later) must reproduce every function, view and comment of the baseline
// verbatim; a baseline change without a migration that carries the new text fails, the default mode prints that text.
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { cpSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync, appendFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

const root = new URL('../', import.meta.url).pathname;
// The canonical migrations are part of the chain: a tree with only the baseline has nothing to check.
const chainOnly = { skip: existsSync(join(root, 'services/hub/migrations')) ? false : 'no migration chain in this tree' };
const script = join(root, 'tools/canonical-bodies.sh');
const run = (dir, ...args) => spawnSync('sh', [script, '--root', dir, ...args], { encoding: 'utf8' });

function copy(t) {
  const dir = mkdtempSync(join(tmpdir(), 'canonical-bodies-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  for (const d of ['services/hub/baseline', 'services/hub/migrations']) cpSync(join(root, d), join(dir, d), { recursive: true });
  return dir;
}
const baseline = (dir) => join(dir, 'services/hub/baseline/1000_baseline.sql');
// A changed function text in the baseline: the first RAISE EXCEPTION message of the file gets a suffix.
function changeBaseline(dir) {
  const text = readFileSync(baseline(dir), 'utf8');
  const i = text.indexOf("RAISE EXCEPTION '");
  assert.ok(i > 0, 'the baseline has a RAISE EXCEPTION');
  writeFileSync(baseline(dir), text.slice(0, i + 17) + 'changed: ' + text.slice(i + 17));
}

test('the repository passes: the canonical migrations reproduce the baseline', chainOnly, () => {
  const r = run(root, '--check');
  assert.equal(r.status, 0, r.stderr);
  assert.equal(run(root).stdout, '', 'nothing missing');
});

test('--stdout prints every function, view and comment of the baseline as CREATE OR REPLACE / COMMENT ON', chainOnly, () => {
  const out = execFileSync('sh', [script, '--stdout'], { encoding: 'utf8' });
  assert.ok(out.split('\nCREATE OR REPLACE FUNCTION ').length > 50);
  assert.doesNotMatch(out, /\nCREATE (FUNCTION|VIEW)/);
});

test('a baseline change without a migration is stale; the printed text in a new migration fixes it', chainOnly, (t) => {
  const dir = copy(t);
  changeBaseline(dir);
  const stale = run(dir, '--check');
  assert.equal(stale.status, 1);
  assert.match(stale.stderr, /differs from the baseline: FUNCTION /);
  const missing = run(dir).stdout;
  assert.equal(missing.split('\nCREATE OR REPLACE ').length, 2, 'exactly the changed function');
  assert.match(missing, /changed: /);
  const migration = join(dir, 'services/hub/migrations/0099_test.sql');
  writeFileSync(migration, "SET LOCAL lock_timeout = '5s';\n" + missing);
  assert.equal(run(dir, '--check').status, 0, run(dir, '--check').stderr);
  // A later migration with another text for the same function wins and is stale again.
  appendFileSync(migration, '\n' + missing.replace('changed: ', 'other: '));
  assert.equal(run(dir, '--check').status, 1);
});

test('a function or comment that the baseline does not have fails; a DROP forgets it', chainOnly, (t) => {
  const dir = copy(t);
  const migration = join(dir, 'services/hub/migrations/0099_test.sql');
  writeFileSync(migration, `CREATE FUNCTION public.zz_test() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$;
COMMENT ON FUNCTION public.zz_test() IS 'x';\n`);
  const r = run(dir, '--check');
  assert.equal(r.status, 1);
  assert.match(r.stderr, /not in the baseline: FUNCTION public\.zz_test\(\)/);
  assert.match(r.stderr, /not in the baseline: COMMENT ON FUNCTION public\.zz_test\(\)/);
  appendFileSync(migration, 'DROP FUNCTION IF EXISTS public.zz_test();\n');
  assert.equal(run(dir, '--check').status, 0);
});

test('a DROP of several names forgets each of them', chainOnly, (t) => {
  const dir = copy(t);
  writeFileSync(join(dir, 'services/hub/migrations/0099_test.sql'), `CREATE FUNCTION public.zz_a(a int, b text) RETURNS int LANGUAGE sql AS $$ SELECT 1 $$;
CREATE VIEW public.zz_v AS SELECT 1 AS x;
CREATE FUNCTION public.zz_b() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$;
DROP FUNCTION public.zz_a(int, text) ,\n  public.zz_b;
DROP VIEW IF EXISTS public.zz_v CASCADE;\n`);
  const r = run(dir, '--check');
  assert.equal(r.status, 0, r.stderr);
});

test('migrations before 0028 do not count', chainOnly, (t) => {
  const dir = copy(t);
  writeFileSync(join(dir, 'services/hub/migrations/0027_zz_test.sql'), 'CREATE FUNCTION public.zz_test() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$;\n');
  assert.equal(run(dir, '--check').status, 0);
});
