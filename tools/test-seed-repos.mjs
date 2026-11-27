// Step 2.6 of the factory plan. The document repo registry from the repositories.toml seed (migration 0024_seed_repos,
// public.import_document_repositories) on PGlite in memory: the import adds and updates, deletes nothing,
// a repeat changes nothing, a row with sync state stays, the repo project gets a graph node, an invalid
// row rolls back the whole call. An installation seed against the registry of 0017 is checked by that installation.
import assert from 'node:assert/strict';
import test from 'node:test';
import { migrationFiles, migrationSql } from './hub-migrations.mjs';

const { PGlite } = await import(new URL('../.tools/node_modules/@electric-sql/pglite/dist/index.js', import.meta.url));

async function fresh(t) {
  const db = new PGlite();
  t.after(() => db.close());
  await db.exec(`CREATE ROLE app; CREATE ROLE hub; CREATE ROLE hub_web; GRANT ALL ON SCHEMA public TO app;
    DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO app', current_database()); END $$;
    SET ROLE app;`);
  for (const file of await migrationFiles()) {
    await db.exec('BEGIN;\n' + (await migrationSql(file)) + '\nCOMMIT;');
  }
  // `hub migrate` seeds the installation row after the migrations; without it the account default (0025) gives HUBIN.
  await db.exec("SELECT public.seed_instance('demo', 'Europe/Athens', 20, NULL, ARRAY['docs/superpowers'])");
  const sql = (q, p = []) => db.query(q, p).then((r) => r.rows);
  // The registry the first start of hub leaves (its seed), and one repo with sync state.
  await importRepos(sql, BASE);
  await sql(`INSERT INTO public.repository_document_sync_state(repo, revision, synced_at) VALUES ('acme/apps', $1, now())`, ['a'.repeat(40)]);
  return { db, sql };
}

const code = (c) => (e) => e.code === c;
const importRepos = (sql, repos) =>
  sql(`SELECT public.import_document_repositories($1::jsonb) AS r`, [JSON.stringify(repos)]).then((r) => r[0].r);
const registry = (sql) => sql(`SELECT repo, project, caller FROM public.document_repository ORDER BY repo`);

// The seed of the first start; a repeat changes nothing.
const BASE = [
  { repo: 'acme/apps', project: 'apps', caller: 'docs-apps' },
  { repo: 'acme/tools', project: 'tools', caller: 'docs-tools' }
];

await test('the same seed again changes nothing', async (t) => {
  const { sql } = await fresh(t);
  const before = await registry(sql);
  assert.deepEqual(await importRepos(sql, BASE), { created: 0, updated: 0, unchanged: 2 });
  assert.deepEqual(await registry(sql), before);
});

await test('import adds and updates, never deletes, and is idempotent', async (t) => {
  const { sql } = await fresh(t);
  // A repo with sync state (FK from repository_document_sync_state): the row must survive an import without it.
  assert.equal((await sql(`SELECT count(*)::int AS n FROM public.repository_document_sync_state WHERE repo = 'acme/apps'`))[0].n, 1);
  const before = (await registry(sql)).length;
  const widgets = { repo: 'acme/widgets', project: 'widgets', caller: 'docs-widgets' };
  assert.deepEqual(await importRepos(sql, [widgets]), { created: 1, updated: 0, unchanged: 0 });
  assert.deepEqual(await importRepos(sql, [widgets]), { created: 0, updated: 0, unchanged: 1 });
  const rows = await registry(sql);
  assert.equal(rows.length, before + 1, 'repos missing from the seed stay');
  assert.ok(rows.some((r) => r.repo === 'acme/apps'));
  // The repo project is a graph node: the sync puts the document's in_project edge to it.
  assert.equal((await sql(`SELECT count(*)::int AS n FROM graph.node WHERE kind = 'project' AND key = 'widgets'`))[0].n, 1);
  // A project change arrives as a row update.
  assert.deepEqual(await importRepos(sql, [{ ...widgets, project: 'gadgets' }]), { created: 0, updated: 1, unchanged: 0 });
  assert.deepEqual((await registry(sql)).find((r) => r.repo === 'acme/widgets'),
    { repo: 'acme/widgets', project: 'gadgets', caller: 'docs-widgets' });
  assert.equal((await sql(`SELECT count(*)::int AS n FROM graph.node WHERE kind = 'project' AND key = 'gadgets'`))[0].n, 1);
});

await test('a bad row rolls the whole call back', async (t) => {
  const { sql } = await fresh(t);
  await assert.rejects(importRepos(sql, { repo: 'x' }), code('22023'), 'not an array');
  const before = await registry(sql);
  // The first row is valid, the second violates a CHECK: the call fails as a whole, the first row is not in the registry.
  await assert.rejects(importRepos(sql, [
    { repo: 'acme/widgets', project: 'widgets', caller: 'docs-widgets' },
    { repo: 'acme/bad', project: 'Bad Project', caller: 'docs-bad' }
  ]), code('23514'));
  assert.deepEqual(await registry(sql), before);
  // The caller is taken by another repo: UNIQUE caller, a refusal rather than silently re-gluing someone else's secret.
  await assert.rejects(importRepos(sql, [{ repo: 'acme/widgets', project: 'widgets', caller: 'docs-apps' }]), code('23505'));
  // A field that is not a string: a refusal with a clear error rather than NULL in NOT NULL.
  await assert.rejects(importRepos(sql, [{ repo: 'acme/widgets', project: 'widgets' }]), code('22023'));
});

await test('the hub role may call the import', async (t) => {
  const { sql } = await fresh(t);
  await sql('RESET ROLE');
  await sql('SET ROLE hub');
  assert.deepEqual(await importRepos(sql, BASE), { created: 0, updated: 0, unchanged: 2 });
});
