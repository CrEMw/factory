// "Blocked by" into edges (migration 0014_graph_import) on PGlite in memory: the import creates
// confirmed edges from seed:links, a repeated import changes nothing, an edge closed by a person does not come back,
// an unknown or retired code is skipped with a report, a second part_of parent gives single_violation,
// cycles in the file give a report with all paths and no partial write (the install seed file: an install test of its own).
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
  const migrations = await migrationFiles();
  for (const file of migrations) {
    await db.exec('BEGIN;\n' + (await migrationSql(file)) + '\nCOMMIT;');
  }
  // Like Job hub-db-migrate: the instance row (0023) after the migrations; without it the account default (0025) gives HUBIN.
  await db.exec("SELECT public.seed_instance('demo', 'Europe/Athens', 20, NULL, ARRAY['docs/superpowers'])");
  const sql = (q, p = []) => db.query(q, p).then((r) => r.rows);
  // Tickets are written only with an actor (0018); the import sets its own actor.
  await sql(`SELECT set_config('hub.actor', 'seed:test', false), set_config('hub.actor_kind', 'seed', false)`);
  return { db, sql };
}

const tickets = (sql, codes) =>
  Promise.all(codes.map((c) => sql(`INSERT INTO tickets(project, code, summary) VALUES ('gadgets', $1, $1)`, [c])));

const importLinks = (sql, links) => sql(`SELECT graph.import_links($1::jsonb) AS r`, [JSON.stringify(links)]).then((r) => r[0].r);

const edges = (sql) =>
  sql(`SELECT graph.node_label(src) AS src, rel, graph.node_label(dst) AS dst, state, created_by, confirmed_kind,
    valid_to IS NULL AS live, evidence FROM graph.edge ORDER BY id`);

const link = (src, dst) => ({ src, rel: 'blocks', dst, source: 'body' });

await test('import creates confirmed seed edges and a second run changes nothing', async (t) => {
  const { sql } = await fresh(t);
  await tickets(sql, ['a', 'b', 'c']);
  const first = await importLinks(sql, [link('a', 'b'), link('b', 'c')]);
  assert.deepEqual(first, { status: 'ok', created: 2, exists: 0, closed: 0, unknown: [], cycles: [] });
  const after = await edges(sql);
  assert.deepEqual(after.map((e) => [e.src, e.dst, e.state, e.created_by, e.confirmed_kind, e.live]), [
    ['a', 'b', 'confirmed', 'seed:links', 'seed', true],
    ['b', 'c', 'confirmed', 'seed:links', 'seed', true]
  ]);
  assert.deepEqual(after[0].evidence, { source: 'body', seed: 'links' });
  const changes = (await sql(`SELECT count(*)::int AS n FROM ticket_change`))[0].n;

  const second = await importLinks(sql, [link('a', 'b'), link('b', 'c')]);
  assert.deepEqual(second, { status: 'ok', created: 0, exists: 2, closed: 0, unknown: [], cycles: [] });
  assert.deepEqual(await edges(sql), after, 'second import writes nothing');
  assert.equal((await sql(`SELECT count(*)::int AS n FROM ticket_change`))[0].n, changes, 'journal unchanged');
});

await test('an edge closed by a human is not brought back, unknown codes are reported', async (t) => {
  const { sql } = await fresh(t);
  await tickets(sql, ['a', 'b']);
  await importLinks(sql, [link('a', 'b')]);
  await sql(`SELECT set_config('hub.actor', 'user:1', false), set_config('hub.actor_kind', 'human', false)`);
  await sql(`UPDATE graph.edge SET valid_to = now(), close_reason = 'removed', closed_by = 'user:1'`);
  const r = await importLinks(sql, [link('a', 'b'), link('a', 'zz'), link('qq', 'b')]);
  assert.equal(r.status, 'ok');
  assert.equal(r.created, 0);
  assert.equal(r.closed, 1);
  assert.deepEqual(r.unknown.map((u) => u.reason), ['unknown_dst', 'unknown_src']);
  assert.deepEqual((await edges(sql)).map((e) => e.live), [false]);
});

await test('a cycle in the file is reported and nothing is written', async (t) => {
  const { sql } = await fresh(t);
  await tickets(sql, ['a', 'b', 'c', 'd']);
  const r = await importLinks(sql, [link('d', 'a'), link('a', 'b'), link('b', 'c'), link('c', 'a')]);
  assert.equal(r.status, 'cycle');
  assert.equal(r.created, 0);
  assert.deepEqual(r.cycles, [['c', 'a', 'b', 'c']]);
  assert.deepEqual(await edges(sql), [], 'no partial write');

  // A cycle together with an edge already in the database: also a report and not a single new row.
  await importLinks(sql, [link('a', 'b')]);
  const r2 = await importLinks(sql, [link('c', 'd'), link('b', 'a')]);
  assert.equal(r2.status, 'cycle');
  assert.deepEqual((await edges(sql)).map((e) => [e.src, e.dst]), [['a', 'b']]);
});

await test('two separate cycles in one file are both reported, nothing is written', async (t) => {
  const { sql } = await fresh(t);
  await tickets(sql, ['a', 'b', 'c', 'x', 'y', 'e']);
  const r = await importLinks(sql, [link('a', 'b'), link('b', 'a'), link('x', 'y'), link('e', 'c'), link('y', 'x')]);
  assert.equal(r.status, 'cycle');
  assert.equal(r.created, 0);
  assert.deepEqual(r.cycles, [['b', 'a', 'b'], ['y', 'x', 'y']]);
  assert.deepEqual(await edges(sql), [], 'the edge outside the cycles is rolled back too');
});

await test('a retired ticket is reported as retired, not unknown, and gets no edge', async (t) => {
  const { sql } = await fresh(t);
  await tickets(sql, ['a', 'b', 'c']);
  await sql(`SELECT set_config('hub.actor', 'user:1', false), set_config('hub.actor_kind', 'human', false)`);
  await sql(`UPDATE graph.node SET retired_at = now(), retire_reason = 'test'
    WHERE id = (SELECT gt.node_id FROM graph.ticket gt JOIN tickets t ON t.seq = gt.ticket_seq WHERE t.code = 'c')`);
  const r = await importLinks(sql, [link('c', 'a'), link('a', 'c'), link('a', 'b')]);
  assert.equal(r.status, 'ok');
  assert.equal(r.created, 1);
  assert.deepEqual(r.unknown.map((u) => [u.src, u.dst, u.reason]), [['c', 'a', 'retired_src'], ['a', 'c', 'retired_dst']]);
  assert.deepEqual((await edges(sql)).map((e) => [e.src, e.dst]), [['a', 'b']]);
});

await test('a second parent part_of is single_violation; the same parent again is exists', async (t) => {
  const { sql } = await fresh(t);
  await tickets(sql, ['a', 'p', 'q']);
  const part = (src, dst) => ({ src, rel: 'part_of', dst, source: src + ':body' });
  const r = await importLinks(sql, [part('a', 'p'), part('a', 'q')]);
  assert.equal(r.status, 'ok');
  assert.equal(r.created, 1);
  assert.deepEqual(r.unknown.map((u) => [u.src, u.dst, u.reason]), [['a', 'q', 'single_violation']]);
  const e = await edges(sql);
  assert.deepEqual(e.map((x) => [x.src, x.rel, x.dst]), [['a', 'part_of', 'p']]);
  assert.deepEqual(e[0].evidence, { source: 'a:body', seed: 'links' });
  const again = await importLinks(sql, [part('a', 'p')]);
  assert.deepEqual([again.created, again.exists, again.unknown], [0, 1, []]);
});
