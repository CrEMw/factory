// Seed of the starting scopes plan.import_scopes (migration 0019) on PGlite: an umbrella ticket, part_of edges of
// the children, a scope with actor seed:scopes, a repeat changes nothing, a file edit arrives, a person's edit and removal
// of a scope or an edge are not rolled back by the import, it does not create a baseline. The file services/hub/seed/scopes.toml itself is checked by cargo
// test (services/hub/src/plan_seed.rs).
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { migrationFiles, migrationSql } from './hub-migrations.mjs';

const { PGlite } = await import(new URL('../.tools/node_modules/@electric-sql/pglite/dist/index.js', import.meta.url));
const root = new URL('../', import.meta.url);
const read = (path) => readFile(new URL(path, root), 'utf8');

async function fresh(t) {
  const db = new PGlite();
  t.after(() => db.close());
  await db.exec(`CREATE ROLE app; CREATE ROLE hub; CREATE ROLE hub_web; GRANT ALL ON SCHEMA public TO app;
    DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO app', current_database()); END $$;
    SET ROLE app;`);
  const migrations = await migrationFiles();
  for (const file of migrations) {
    await db.exec('BEGIN;\n' + (await migrationSql(file)) + '\nCOMMIT;');
  }
  // Like Job hub-db-migrate: the instance row (0023) after the migrations; without it the account default (0025) gives HUBIN.
  await db.exec("SELECT public.seed_instance('demo', 'Europe/Athens', 20, NULL, ARRAY['docs/superpowers'])");
  const sql = (q, p = []) => db.query(q, p).then((r) => r.rows);
  const as = (actor, kind) => sql(`SELECT set_config('hub.actor', $1, false), set_config('hub.actor_kind', $2, false)`, [actor, kind]);
  return { db, sql, as };
}

const code = (c) => (e) => e.code === c;
// Like hub at start: the import in its own transaction (actor seed:scopes is set locally and does not leak into the session).
const seed = (db, scopes) =>
  db.transaction(async (tx) => (await tx.query(`SELECT plan.import_scopes($1::jsonb) AS r`, [JSON.stringify(scopes)])).rows[0].r);
const PC = {
  root: 'pcchain',
  project: 'gadgets',
  summary: '[PC] Chain',
  body: 'umbrella',
  priority: 'p1',
  tags: ['gadgets', 'scope'],
  children: ['yy', 'yz', 'z0']
};
const children = (sql) =>
  sql(`SELECT t.code, e.state, e.created_by FROM graph.edge e JOIN graph.ticket gt ON gt.node_id = e.src
    JOIN tickets t ON t.seq = gt.ticket_seq JOIN graph.ticket p ON p.node_id = e.dst JOIN tickets pt ON pt.seq = p.ticket_seq
    WHERE e.rel = 'part_of' AND pt.code = 'pcchain' AND e.valid_to IS NULL ORDER BY t.code`);
const scopes = (sql) =>
  sql(`SELECT s.due_date, s.mode, s.max_defaulted_pct, s.actor, s.actor_kind, s.valid_to IS NULL AS live
    FROM plan.scope s JOIN tickets t ON t.seq = s.root_seq WHERE t.code = 'pcchain' ORDER BY s.id`);

await test('seed creates the umbrella, part_of edges and the scope once; edits by the file apply, human edits stay', async (t) => {
  const { db, sql, as } = await fresh(t);
  await as('user:1', 'human');
  await sql(`INSERT INTO tickets(project, code, summary) VALUES ('gadgets', 'yy', 'PC-1'), ('gadgets', 'yz', 'PC-2'),
    ('widgets', 'z0', 'PC-3')`);
  await as('', '');

  const r1 = await seed(db, [PC]);
  assert.deepEqual([r1.tickets_created, r1.created, r1.updated, r1.unchanged, r1.closed, r1.unknown], [1, 1, 0, 0, 0, []]);
  assert.deepEqual([r1.links.pcchain.status, r1.links.pcchain.created], ['ok', 3]);
  const [umbrella] = await sql(`SELECT project, summary, body, priority, tags FROM tickets WHERE code = 'pcchain'`);
  assert.deepEqual(umbrella, { project: 'gadgets', summary: '[PC] Chain', body: 'umbrella', priority: 'p1', tags: ['gadgets', 'scope'] });
  assert.deepEqual(
    await sql(`SELECT c.actor, c.actor_kind FROM ticket_change c JOIN tickets t ON t.seq = c.ticket_seq
      WHERE t.code = 'pcchain' AND c.field = 'created'`),
    [{ actor: 'seed:scopes', actor_kind: 'seed' }]
  );
  assert.deepEqual(
    (await children(sql)).map((c) => `${c.code}:${c.state}:${c.created_by}`),
    ['yy:confirmed:seed:scopes', 'yz:confirmed:seed:scopes', 'z0:confirmed:seed:scopes']
  );
  assert.deepEqual(await scopes(sql), [{ due_date: null, mode: 'forward', max_defaulted_pct: 20, actor: 'seed:scopes', actor_kind: 'seed', live: true }]);
  assert.equal((await sql(`SELECT count(*)::int AS n FROM plan.baseline`))[0].n, 0, 'no baseline: a human approves it');
  assert.deepEqual(
    await sql(`SELECT (d.marked > d.done) AS dirty FROM plan.dirty d JOIN tickets t ON t.seq = d.root_seq WHERE t.code = 'pcchain'`),
    [{ dirty: true }],
    'the new scope is queued for its first plan'
  );
  assert.deepEqual(await sql(`SELECT current_setting('hub.actor', true) AS a`), [{ a: '' }], 'the seed actor stays in its transaction');

  // A repeat changes nothing.
  const r2 = await seed(db, [PC]);
  assert.deepEqual([r2.tickets_created, r2.created, r2.updated, r2.unchanged, r2.links.pcchain.created, r2.links.pcchain.exists], [0, 0, 0, 1, 0, 3]);
  assert.equal((await scopes(sql)).length, 1);

  // A file edit arrives as a new version while the seed wrote the live one; the existing umbrella ticket is not edited.
  const r3 = await seed(db, [{ ...PC, summary: 'another', due_date: '2030-03-01', max_defaulted_pct: 30, children: [...PC.children, 'nope'] }]);
  assert.deepEqual([r3.tickets_created, r3.updated], [0, 1]);
  assert.deepEqual(r3.links.pcchain.unknown, [{ src: 'nope', rel: 'part_of', dst: 'pcchain', reason: 'unknown_src' }]);
  assert.equal((await sql(`SELECT summary FROM tickets WHERE code = 'pcchain'`))[0].summary, '[PC] Chain');
  const v = await scopes(sql);
  assert.deepEqual(v.map((x) => [x.live, x.max_defaulted_pct]), [[false, 20], [true, 30]]);
  assert.equal(v[1].due_date.getFullYear(), 2030);

  // A person changes the scope and removes an edge: the import rolls back neither.
  await as('user:1', 'human');
  await sql(`INSERT INTO plan.scope(root_seq, max_defaulted_pct) SELECT seq, 40 FROM tickets WHERE code = 'pcchain'`);
  await sql(`UPDATE graph.edge SET valid_to = now(), close_reason = 'removed' WHERE rel = 'part_of'
    AND src = (SELECT gt.node_id FROM graph.ticket gt JOIN tickets t ON t.seq = gt.ticket_seq WHERE t.code = 'z0')`);
  await as('', '');
  const r4 = await seed(db, [PC]);
  assert.deepEqual([r4.updated, r4.unchanged, r4.links.pcchain.closed], [0, 1, 1]);
  assert.deepEqual((await scopes(sql)).at(-1), { due_date: null, mode: 'forward', max_defaulted_pct: 40, actor: 'user:1', actor_kind: 'human', live: true });
  assert.deepEqual((await children(sql)).map((c) => c.code), ['yy', 'yz']);
  await as('user:1', 'human');
  await sql(`UPDATE plan.scope SET valid_to = now() WHERE valid_to IS NULL`);
  await as('', '');
  const r5 = await seed(db, [PC]);
  assert.deepEqual([r5.created, r5.updated, r5.closed], [0, 0, 1], 'a scope closed by a human stays closed');
});

await test('unknown root without umbrella fields is reported; bad input refused; the seed cannot approve a baseline', async (t) => {
  const { db, sql, as } = await fresh(t);
  const r = await seed(db, [{ root: 'ghost', children: ['a'] }]);
  assert.deepEqual([r.created, r.unknown], [0, [{ root: 'ghost', reason: 'unknown_root' }]]);
  await assert.rejects(seed(db, { root: 'x' }), code('22023'));
  await seed(db, [{ ...PC, children: [] }]);
  await as('seed:scopes', 'seed');
  await assert.rejects(
    sql(`INSERT INTO plan.baseline(root_seq, scope_id, defaulted_pct, algo_version, result)
      SELECT s.root_seq, s.id, 0, 1, '{}'::jsonb FROM plan.scope s WHERE s.valid_to IS NULL`),
    code('GRHUM')
  );
});
