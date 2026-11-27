// The hub graph (migrations 0010_graph and 0011_ticket_change) on an isolated PostgreSQL: PGlite in memory.
// PGlite has no parallel connections: "two parallel single inserts" are checked with two inserts in a row,
// the second fails on the edge_single index rather than on the trigger (the index also catches parallel inserts in Postgres).
// Two parallel cycle inserts on a real Postgres are checked by tools/test-graph-pg.sh (docker, local only). The 0010
// backfill of the tickets and documents an install had before the graph is checked by that install, not here.
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
  for (const file of await migrationFiles()) {
    await db.exec('BEGIN;\n' + (await migrationSql(file)) + '\nCOMMIT;');
  }
  // Like Job hub-db-migrate: the instance row (0023) after the migrations; without it the account default (0025) gives HUBIN.
  await db.exec("SELECT public.seed_instance('demo', 'Europe/Athens', 20, NULL, ARRAY['docs/superpowers'])");
  const sql = (q, p = []) => db.query(q, p).then((r) => r.rows);
  const as = (actor, kind) => sql(`SELECT set_config('hub.actor', $1, false), set_config('hub.actor_kind', $2, false)`, [actor, kind]);
  // Like the seed at the hub start: the document repository registry.
  await sql(`SELECT public.import_document_repositories($1::jsonb)`, [JSON.stringify([
    { repo: 'acme/apps', project: 'apps', caller: 'docs-apps' }, { repo: 'acme/tools', project: 'tools', caller: 'docs-tools' }])]);
  return { db, sql, as };
}

const node = async (sql, code) =>
  (await sql(`SELECT gt.node_id AS id FROM graph.ticket gt JOIN tickets t ON t.seq = gt.ticket_seq WHERE t.code = $1`, [code]))[0].id;

// Tickets are written only with an actor (0018): without a session actor the insert goes as seed:test, the session actor does not change.
async function withActor(sql, fn) {
  const [{ a }] = await sql(`SELECT current_setting('hub.actor', true) AS a`);
  if (a) return fn();
  await sql(`SELECT set_config('hub.actor', 'seed:test', false), set_config('hub.actor_kind', 'seed', false)`);
  try {
    return await fn();
  } finally {
    await sql(`SELECT set_config('hub.actor', '', false), set_config('hub.actor_kind', '', false)`);
  }
}

async function tickets(sql, ...codes) {
  const out = {};
  for (const code of codes) {
    await withActor(sql, () => sql(`INSERT INTO tickets(project, code, summary) VALUES ('demo', $1, $1)`, [code]));
    out[code] = await node(sql, code);
  }
  return out;
}

const link = (sql, src, rel, dst, state = 'confirmed', kind = 'ticket') =>
  sql(
    `INSERT INTO graph.edge(src, src_kind, rel, dst, dst_kind, state) VALUES ($1, 'ticket', $2, $3, $4, $5) RETURNING id, state`,
    [src, rel, dst, kind, state]
  ).then((r) => r[0]);

const code = (c) => (e) => e.code === c;

await test('triggers give nodes to new tickets, projects and documents', async (t) => {
  const { sql } = await fresh(t);
  // A ticket without an actor: the strict log (0018) rejects the whole write, there is no node.
  await assert.rejects(sql(`INSERT INTO tickets(project, code, summary) VALUES ('newproj', 'n1', 'new')`), code('GRACT'));
  assert.equal((await sql(`SELECT count(*)::int AS n FROM tickets WHERE code = 'n1'`))[0].n, 0);
  // With an actor outside a hub transaction (like the seed): the node appears.
  await sql(`SELECT set_config('hub.actor', 'seed:old', false), set_config('hub.actor_kind', 'seed', false)`);
  await sql(`INSERT INTO tickets(project, code, summary) VALUES ('newproj', 'n1', 'new')`);
  await sql(`INSERT INTO documents(repo, path, role, title, body, sha256, created_at, synced_at)
    VALUES ('acme/tools', 'docs/superpowers/specs/x.md', 'spec', 'X', 'x', 'h', now(), now())`);
  assert.ok(await node(sql, 'n1'));
  assert.equal((await sql(`SELECT count(*)::int AS n FROM graph.node WHERE kind = 'project' AND key = 'newproj'`))[0].n, 1);
  assert.equal((await sql(`SELECT count(*)::int AS n FROM graph.document WHERE repo = 'acme/tools'`))[0].n, 1);
  // A long document path does not break the sync either: the node appears.
  await sql(`INSERT INTO documents(repo, path, role, title, body, sha256, created_at, synced_at)
    VALUES ('acme/tools', 'docs/superpowers/specs/' || repeat('x', 900) || '.md', 'spec', 'X', 'x', 'h', now(), now())`);
  assert.equal((await sql(`SELECT count(*)::int AS n FROM graph.node WHERE kind = 'document' AND length(key) > 900`))[0].n, 1);
  await assert.rejects(sql(`INSERT INTO graph.node(kind, key) VALUES ('meeting', repeat('m', 301))`), (e) => e.code === '23514');
  // The project when the ticket's project changes.
  await sql(`UPDATE tickets SET project = 'moved' WHERE code = 'n1'`);
  assert.equal((await sql(`SELECT count(*)::int AS n FROM graph.node WHERE kind = 'project' AND key = 'moved'`))[0].n, 1);
  // Log of creation and edit with the writing actor (legacy-pod is no longer substituted).
  const log = await sql(`SELECT field, old, new, actor, actor_kind FROM ticket_change ORDER BY id`);
  assert.deepEqual(log, [
    { field: 'created', old: null, new: { code: 'n1', project: 'newproj', status: 'open', priority: null }, actor: 'seed:old', actor_kind: 'seed' },
    { field: 'project', old: 'newproj', new: 'moved', actor: 'seed:old', actor_kind: 'seed' }
  ]);
  // An edit without an actor, even of updated_at only, is rejected too.
  await sql(`SELECT set_config('hub.actor', '', false), set_config('hub.actor_kind', '', false)`);
  await assert.rejects(sql(`UPDATE tickets SET updated_at = now() WHERE code = 'n1'`), code('GRACT'));
  await sql(`SELECT set_config('hub.actor', 'seed:old', false), set_config('hub.actor_kind', '', false)`);
  await assert.rejects(sql(`UPDATE tickets SET status = 'doing' WHERE code = 'n1'`), code('GRACT'), 'kind is required too');
});

await test('edge_v joins live edges, in_project and ticket_documents', async (t) => {
  const { sql, as } = await fresh(t);
  await as('user:alice', 'human');
  const n = await tickets(sql, 'a', 'b');
  await link(sql, n.a, 'blocks', n.b);
  await sql(`INSERT INTO documents(repo, path, role, title, body, sha256, created_at, synced_at)
    VALUES ('acme/apps', 'docs/superpowers/plans/a.md', 'plan', 'A', 'x', 'h', now(), now())`);
  await sql(`INSERT INTO ticket_documents(ticket_seq, repo, path, rel, valid_from)
    SELECT seq, 'acme/apps', 'docs/superpowers/plans/a.md', 'describes', now() FROM tickets WHERE code = 'a'`);
  const rows = await sql(`SELECT rel, source FROM graph.edge_v ORDER BY source, rel`);
  const count = (rel, source) => rows.filter((r) => r.rel === rel && r.source === source).length;
  assert.equal(count('blocks', 'edge'), 1);
  assert.equal(count('in_project', 'tickets.project'), 2, 'every ticket is in its project');
  assert.equal(count('describes', 'ticket_documents'), 1, 'document describes a');
});

await test('shapes, self links, single, proposed and the edge_single list', async (t) => {
  const { sql, as } = await fresh(t);
  await as('user:alice', 'human');
  const n = await tickets(sql, 'a', 'b', 'c');
  await assert.rejects(link(sql, n.a, 'blocks', n.a), code('GRSLF'), 'self link');
  await assert.rejects(link(sql, n.a, 'owns', n.b), code('GRSHP'), 'unknown rel');
  const project = (await sql(`SELECT id FROM graph.node WHERE kind = 'project' AND key = 'demo' AND account = public.instance_account()`))[0].id;
  await assert.rejects(link(sql, n.a, 'blocks', project, 'confirmed', 'project'), code('GRSHP'), 'blocks ticket -> project');

  // part_of single: a second live confirmed edge from the same src fails on the index.
  await link(sql, n.a, 'part_of', n.b);
  await assert.rejects(link(sql, n.a, 'part_of', n.c), (e) => e.code === '23505' && /edge_single/.test(e.message));
  // proposed takes no slot: there can be several proposals.
  await link(sql, n.a, 'part_of', n.c, 'proposed');
  await assert.rejects(link(sql, n.a, 'part_of', n.b), (e) => e.code === '23505' && /edge_live_unique/.test(e.message), 'same live edge twice');

  // The edge_single index and the dictionary say the same thing.
  const singles = (await sql(`SELECT DISTINCT rel FROM graph.rel_shape WHERE single ORDER BY rel`)).map((r) => r.rel);
  const def = (await sql(`SELECT indexdef FROM pg_indexes WHERE schemaname = 'graph' AND indexname = 'edge_single'`))[0].indexdef;
  const listed = [...def.matchAll(/'([a-z_]+)'::text/g)].map((m) => m[1]).filter((r) => r !== 'confirmed').sort();
  assert.deepEqual(listed, singles);
});

await test('cycles of two and three edges and through a confirmation are refused with the path', async (t) => {
  const { sql, as } = await fresh(t);
  await as('user:alice', 'human');
  const n = await tickets(sql, 'a', 'b', 'c', 'd');
  await link(sql, n.a, 'blocks', n.b);
  await assert.rejects(link(sql, n.b, 'blocks', n.a), (e) => e.code === 'GRCYC' && e.detail === '["b", "a", "b"]');
  await link(sql, n.b, 'blocks', n.c);
  await assert.rejects(link(sql, n.c, 'blocks', n.a), (e) => e.code === 'GRCYC' && e.detail === '["c", "a", "b", "c"]');
  // relates_to is not acyclic.
  await link(sql, n.b, 'relates_to', n.a);
  // proposed is not checked for a cycle, confirmation is.
  const p = await link(sql, n.c, 'blocks', n.a, 'proposed');
  await assert.rejects(
    sql(`UPDATE graph.edge SET state = 'confirmed' WHERE id = $1`, [p.id]),
    (e) => e.code === 'GRCYC' && JSON.parse(e.detail)[0] === 'c'
  );
  // A closed edge does not hold a cycle.
  const ab = (await sql(`SELECT id FROM graph.edge WHERE src = $1 AND dst = $2 AND rel = 'blocks'`, [n.a, n.b]))[0].id;
  await sql(`UPDATE graph.edge SET valid_to = now(), close_reason = 'removed' WHERE id = $1`, [ab]);
  await sql(`UPDATE graph.edge SET state = 'confirmed' WHERE id = $1`, [p.id]);
  const row = (await sql(`SELECT state, confirmed_by, created_by FROM graph.edge WHERE id = $1`, [p.id]))[0];
  assert.deepEqual(row, { state: 'confirmed', confirmed_by: 'user:alice', created_by: 'user:alice' });
});

await test('actor: empty refused, agent cannot confirm, needs_confirm from an agent is proposed, seed confirms', async (t) => {
  const { sql, as } = await fresh(t);
  await as('', '');
  const n = await tickets(sql, 'a', 'b');
  await assert.rejects(link(sql, n.a, 'blocks', n.b), code('GRACT'), 'empty actor');
  await as('user:x', 'agent');
  const p = await link(sql, n.a, 'blocks', n.b, 'proposed');
  await assert.rejects(sql(`UPDATE graph.edge SET state = 'confirmed' WHERE id = $1`, [p.id]), code('GRHUM'));
  // Meeting: an edge from an agent becomes proposed, from the seed confirmed.
  const m = (await sql(`INSERT INTO graph.node(kind, key) VALUES ('meeting', 'm-1') RETURNING id`))[0].id;
  assert.equal((await link(sql, n.a, 'mentioned_in', m, 'confirmed', 'meeting')).state, 'proposed');
  await as('seed:blocked-by', 'seed');
  assert.equal((await link(sql, n.b, 'mentioned_in', m, 'confirmed', 'meeting')).state, 'confirmed');
  // Log: actor and kind on every edge row, at both ends of ticket-ticket.
  const log = await sql(`SELECT field, actor, actor_kind FROM ticket_change WHERE field LIKE 'edge:%' ORDER BY id`);
  assert.deepEqual(log, [
    { field: 'edge:blocks', actor: 'user:x', actor_kind: 'agent' },
    { field: 'edge:blocks', actor: 'user:x', actor_kind: 'agent' },
    { field: 'edge:mentioned_in', actor: 'user:x', actor_kind: 'agent' },
    { field: 'edge:mentioned_in', actor: 'seed:blocked-by', actor_kind: 'seed' }
  ]);
  // A ticket edit with an actor; ticket creation by an agent is visible in the log.
  await as('user:alice', 'human');
  await sql(`UPDATE tickets SET status = 'doing' WHERE code = 'a'`);
  assert.deepEqual((await sql(`SELECT actor, actor_kind FROM ticket_change WHERE field = 'status'`))[0], { actor: 'user:alice', actor_kind: 'human' });
  const created = await sql(`SELECT c.actor_kind FROM ticket_change c JOIN tickets t ON t.seq = c.ticket_seq
    WHERE c.field = 'created' AND t.code IN ('a', 'b') ORDER BY c.id`);
  assert.deepEqual(created.map((r) => r.actor_kind), ['seed', 'seed'], 'a and b were inserted by the test seed, never legacy-pod');
  await as('user:x', 'agent');
  await tickets(sql, 'byagent');
  assert.deepEqual(
    (await sql(`SELECT c.actor, c.actor_kind FROM ticket_change c JOIN tickets t ON t.seq = c.ticket_seq WHERE t.code = 'byagent'`))[0],
    { actor: 'user:x', actor_kind: 'agent' }
  );
});

await test('no DELETE, no identity UPDATE, closed edges and retired nodes are frozen', async (t) => {
  const { sql, as } = await fresh(t);
  await as('user:alice', 'human');
  const n = await tickets(sql, 'a', 'b', 'c');
  const e = await link(sql, n.a, 'blocks', n.b);
  // Rows for the row-level DELETE triggers of documents and ticket_documents.
  await sql(`INSERT INTO documents(repo, path, role, title, body, sha256, created_at, synced_at)
    VALUES ('acme/apps', 'docs/superpowers/plans/a.md', 'plan', 'A', 'x', 'h', now(), now())`);
  await sql(`INSERT INTO ticket_documents(ticket_seq, repo, path, rel, valid_from)
    SELECT seq, 'acme/apps', 'docs/superpowers/plans/a.md', 'describes', now() FROM tickets WHERE code = 'a'`);
  for (const q of [
    `DELETE FROM tickets WHERE code = 'a'`,
    `DELETE FROM documents`,
    `DELETE FROM ticket_documents`,
    `DELETE FROM graph.node WHERE id = ${n.c}`,
    `DELETE FROM graph.edge`,
    `DELETE FROM graph.rel_shape`,
    `DELETE FROM ticket_change`
  ]) {
    await assert.rejects(sql(q), code('GRFRB'), q);
  }
  // TRUNCATE bypasses DELETE: a ban on every table with forbid_delete, the list keeps up with the triggers.
  const guarded = (name) =>
    sql(`SELECT n.nspname || '.' || c.relname AS t FROM pg_trigger g JOIN pg_class c ON c.oid = g.tgrelid
      JOIN pg_namespace n ON n.oid = c.relnamespace WHERE g.tgname = $1 ORDER BY 1`, [name]).then((r) => r.map((x) => x.t));
  const noDelete = await guarded('forbid_delete');
  assert.deepEqual(await guarded('forbid_truncate'), noDelete);
  assert.ok(noDelete.includes('graph.ticket') && noDelete.includes('public.ticket_documents'), noDelete.join());
  // The document repo registry and raw document links.
  assert.ok(noDelete.includes('public.document_repository') && noDelete.includes('public.document_ref'), noDelete.join());
  // Without CASCADE Postgres itself does not clear a referenced table; with CASCADE the trigger catches the ban.
  for (const table of noDelete) {
    await assert.rejects(sql(`TRUNCATE ${table} CASCADE`), code('GRFRB'), `TRUNCATE ${table} CASCADE`);
  }
  for (const table of ['graph.ticket', 'graph.document', 'ticket_documents', 'ticket_change']) {
    await assert.rejects(sql(`TRUNCATE ${table}`), code('GRFRB'), `TRUNCATE ${table}`);
  }
  for (const q of [
    `UPDATE graph.node SET key = 'zz' WHERE id = ${n.a}`,
    `UPDATE graph.ticket SET ticket_seq = ticket_seq WHERE node_id = ${n.a}`,
    `UPDATE graph.edge SET dst = ${n.c} WHERE id = ${e.id}`,
    `UPDATE graph.edge SET evidence = '{}' WHERE id = ${e.id}`,
    `UPDATE graph.edge SET created_by = 'x' WHERE id = ${e.id}`,
    `UPDATE graph.edge SET state = 'proposed', confirmed_at = NULL, confirmed_by = NULL WHERE id = ${e.id}`,
    `UPDATE graph.rel_shape SET single = true WHERE rel = 'blocks'`,
    `UPDATE ticket_change SET actor = 'x'`
  ]) {
    await assert.rejects(sql(q), code('GRFRB'), q);
  }
  await sql(`UPDATE graph.edge SET valid_to = now(), close_reason = 'removed' WHERE id = $1`, [e.id]);
  assert.equal((await sql(`SELECT closed_by FROM graph.edge WHERE id = $1`, [e.id]))[0].closed_by, 'user:alice');
  await assert.rejects(sql(`UPDATE graph.edge SET close_reason = 'rejected' WHERE id = $1`, [e.id]), code('GRFRB'), 'closed once');
  // evidence: links only.
  const m = (await sql(`INSERT INTO graph.node(kind, key) VALUES ('meeting', 'm-2') RETURNING id`))[0].id;
  const withEvidence = (ev) =>
    sql(`INSERT INTO graph.edge(src, src_kind, rel, dst, dst_kind, evidence) VALUES ($1, 'ticket', 'raised_in', $2, 'meeting', $3)`, [n.a, m, ev]);
  await assert.rejects(withEvidence({ quote: 'x' }), code('GREVD'));
  await assert.rejects(withEvidence({ note: 'x'.repeat(101) }), code('GREVD'));
  await assert.rejects(withEvidence({ nested: { a: 1 } }), code('GREVD'));
  await withEvidence({ transcript_id: 'abc', segment_seq: 3, t_offset_s: 12 });
  // A retired node: new edges to it are forbidden, retirement happens once.
  await sql(`UPDATE graph.node SET retired_at = now(), retire_reason = 'test' WHERE id = $1`, [n.c]);
  await assert.rejects(link(sql, n.a, 'relates_to', n.c), code('GRRET'));
  await assert.rejects(sql(`UPDATE graph.node SET retire_reason = 'again' WHERE id = $1`, [n.c]), code('GRFRB'));
});

// The spec target is 200 ms on a real Postgres (about 60 ms locally). PGlite in the shared gate pod is slower and noisy
// (203 ms in the gate once), so the threshold here has headroom: the test catches the order of magnitude (a full scan), not noise.
const PERF_BUDGET_MS = Number(process.env.GRAPH_PERF_BUDGET_MS ?? 1000);

await test('1500 nodes and 3000 edges with diamonds: an edge insert with the cycle check stays within the budget', async (t) => {
  const { sql, as } = await fresh(t);
  await as('seed:perf', 'seed');
  // Layers of 50 nodes, edges only forward: diamonds between neighbouring layers, the graph is acyclic.
  await sql(`INSERT INTO tickets(project, code, summary) SELECT 'perf', 'p' || i, 'p' FROM generate_series(1, 1500) i`);
  await sql(`CREATE TEMP TABLE ids AS SELECT gt.node_id AS id, substr(t.code, 2)::int AS i
    FROM graph.ticket gt JOIN tickets t ON t.seq = gt.ticket_seq WHERE t.project = 'perf'`);
  // Setup without the check trigger (otherwise 3000 cycle checks for setup): the graph is acyclic by construction.
  await sql(`ALTER TABLE graph.edge DISABLE TRIGGER edge_b_check`);
  await sql(`INSERT INTO graph.edge(src, src_kind, rel, dst, dst_kind, created_by, confirmed_at, confirmed_by, confirmed_kind)
    SELECT a.id, 'ticket', 'blocks', b.id, 'ticket', 'seed:perf', now(), 'seed:perf', 'seed' FROM ids a JOIN ids b
      ON b.i = a.i + 50 OR (b.i = a.i + 51 AND a.i % 50 <> 0) OR (b.i = a.i + 52 AND a.i % 10 = 1)
    ORDER BY a.i, b.i`);
  await sql(`ALTER TABLE graph.edge ENABLE TRIGGER edge_b_check`);
  const edges = (await sql(`SELECT count(*)::int AS n FROM graph.edge`))[0].n;
  assert.ok(edges >= 3000, `edges ${edges}`);
  const id = async (i) => (await sql(`SELECT id FROM ids WHERE i = $1`, [i]))[0].id;
  const [deep, side, last, mid] = [await id(1450), await id(1499), await id(1500), await id(800)];
  // For 1450 almost the whole graph is ancestors: the reverse walk goes through all of it.
  let t0 = performance.now();
  await link(sql, deep, 'blocks', side);
  const ok = performance.now() - t0;
  assert.ok(ok < PERF_BUDGET_MS, `acyclic insert ${ok.toFixed(1)} ms`);
  t.diagnostic(`acyclic insert ${ok.toFixed(1)} ms`);
  t0 = performance.now();
  // 800 -> 850 -> ... -> 1500 already exists: a back edge closes a cycle of 16 nodes.
  await assert.rejects(link(sql, last, 'blocks', mid), (e) => e.code === 'GRCYC' && JSON.parse(e.detail).length === 16);
  const bad = performance.now() - t0;
  assert.ok(bad < PERF_BUDGET_MS, `cycle refusal ${bad.toFixed(1)} ms`);
  t.diagnostic(`cycle refusal ${bad.toFixed(1)} ms`);
});

// The ready condition from services/hub/src/query.rs, as Rust builds it: concat! of strings (a backslash with a line
// break eats the indent) and local_date! from services/hub/src/config.rs (step 2.5: today in the time zone of the
// public.instance row; the cargo test sql_local_time_reads_the_instance_timezone pins the macro text).
async function readyCodes(sql) {
  const src = await read('services/hub/src/query.rs');
  const body = src.match(/const READY: &str = concat!\(([\s\S]*?)\n\);/)[1];
  const ready = [...body.matchAll(/local_date!\("([^"]*)"\)|"((?:[^"\\]|\\[\s\S])*)"/g)]
    .map(([, at, s]) => (at !== undefined ? `(${at} at time zone (select public.instance_timezone()))::date` : s))
    .join('')
    .replace(/\\\n\s*/g, '');
  if ((await sql('SELECT count(*)::int AS n FROM public.instance'))[0].n === 0) {
    await sql(`SELECT public.seed_instance('demo', 'Europe/Athens', 20, NULL, NULL)`);
  }
  return (await sql(`SELECT code FROM tickets WHERE project = 'demo'${ready} ORDER BY code`)).map((r) => r.code);
}

await test('blocks and part_of make one precedence order: a mixed cycle is refused; ready skips blocked, parents and duplicates', async (t) => {
  const { sql, as } = await fresh(t);
  await as('user:alice', 'human');
  const n = await tickets(sql, 'a', 'c', 'd', 'e', 'f');
  // c part_of a (a waits for c), then a blocks c (c waits for a): together neither a nor c is ever ready.
  await link(sql, n.c, 'part_of', n.a);
  await assert.rejects(link(sql, n.a, 'blocks', n.c), (e) => e.code === 'GRCYC' && e.detail === '["a", "c", "a"]');
  // A longer mixed path: d blocks c, c part_of a; a part_of d would close a -> d -> c -> a.
  await link(sql, n.d, 'blocks', n.c);
  await assert.rejects(link(sql, n.a, 'part_of', n.d), (e) => e.code === 'GRCYC' && e.detail === '["a", "d", "c", "a"]');
  // duplicates does not mix its order with blocks.
  await link(sql, n.e, 'blocks', n.f);
  await link(sql, n.f, 'duplicates', n.e);
  // Ready: d (waits for nothing), e (waits for nothing); a waits for c, c waits for d, f waits for e and is itself a duplicate.
  assert.deepEqual(await readyCodes(sql), ['d', 'e']);
  await sql(`UPDATE tickets SET status = 'done' WHERE code IN ('d', 'e')`);
  assert.deepEqual(await readyCodes(sql), ['c'], 'f stays out as a duplicate even with e done');
});

await test('cycle check refuses REPEATABLE READ and SERIALIZABLE, where the advisory lock does not see a fresh commit', async (t) => {
  const { db, sql, as } = await fresh(t);
  await as('user:alice', 'human');
  const n = await tickets(sql, 'a', 'b');
  for (const level of ['REPEATABLE READ', 'SERIALIZABLE']) {
    await db.exec(`BEGIN ISOLATION LEVEL ${level}`);
    await assert.rejects(link(sql, n.a, 'blocks', n.b), code('GRISO'), level);
    await db.exec('ROLLBACK');
  }
  // relates_to without a cycle check works at any isolation level.
  await db.exec('BEGIN ISOLATION LEVEL REPEATABLE READ');
  await link(sql, n.a, 'relates_to', n.b);
  await db.exec('COMMIT');
  await link(sql, n.a, 'blocks', n.b);
});

await test('an edge a human confirmed is closed only by a human; agent edges and proposals the agent may close', async (t) => {
  const { sql, as } = await fresh(t);
  await as('user:alice', 'human');
  const n = await tickets(sql, 'a', 'b', 'c', 'd');
  const human = await link(sql, n.a, 'part_of', n.b);
  await as('user:alice', 'agent');
  const close = (id, reason = 'removed') => sql(`UPDATE graph.edge SET valid_to = now(), close_reason = $2 WHERE id = $1`, [id, reason]);
  // The same user:alice, but kind agent: it cannot remove or replace the parent.
  await assert.rejects(close(human.id), code('GRHCL'));
  await assert.rejects(link(sql, n.a, 'part_of', n.c), (e) => /edge_single/.test(e.message));
  // An agent closes its own confirmed and proposed edges.
  const own = await link(sql, n.c, 'blocks', n.d);
  await close(own.id);
  const proposal = await link(sql, n.d, 'relates_to', n.a, 'proposed');
  await close(proposal.id, 'rejected');
  // A person who confirmed an agent's proposal protects the edge too.
  const p = await link(sql, n.b, 'blocks', n.c, 'proposed');
  await as('user:alice', 'human');
  await sql(`UPDATE graph.edge SET state = 'confirmed' WHERE id = $1`, [p.id]);
  assert.equal((await sql(`SELECT confirmed_kind FROM graph.edge WHERE id = $1`, [p.id]))[0].confirmed_kind, 'human');
  await as('hub-ingest', 'service:ingest');
  await assert.rejects(close(p.id), code('GRHCL'));
  await assert.rejects(sql(`UPDATE graph.edge SET confirmed_kind = 'agent' WHERE id = $1`, [p.id]), code('GRFRB'));
  // needs_confirm confirmed by the seed cannot be removed by an agent either.
  const m = (await sql(`INSERT INTO graph.node(kind, key) VALUES ('meeting', 'm-3') RETURNING id`))[0].id;
  await as('seed:x', 'seed');
  const seeded = await link(sql, n.a, 'mentioned_in', m, 'confirmed', 'meeting');
  await as('user:alice', 'agent');
  await assert.rejects(close(seeded.id), code('GRHCL'));
  // A person closes it.
  await as('user:alice', 'human');
  await close(human.id);
  await close(seeded.id);
});
