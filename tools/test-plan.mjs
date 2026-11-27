// The plan schema (migration 0013_plan) on PGlite in memory: versioned rows with an actor (an insert
// closes the previous one, edits only by closing), actor kinds per table, the baseline only by insert and only by a person
// (one live, the default estimate threshold), the plan.dirty queue without deletes: marks from estimates, status, both ends of
// blocks, the old and the new part_of parent, the assignee, the reference tables and a new current plan.
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
  // Like Job hub-db-migrate: the instance row (0023) after the migrations; without it the account default (0025) gives HUBIN.
  await db.exec("SELECT public.seed_instance('demo', 'Europe/Athens', 20, NULL, ARRAY['docs/superpowers'])");
  const sql = (q, p = []) => db.query(q, p).then((r) => r.rows);
  const as = (actor, kind) => sql(`SELECT set_config('hub.actor', $1, false), set_config('hub.actor_kind', $2, false)`, [actor, kind]);
  return { db, sql, as };
}

const code = (c) => (e) => e.code === c;

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
  for (const c of codes) {
    out[c] = (await withActor(sql, () => sql(`INSERT INTO tickets(project, code, summary) VALUES ('widgets', $1, $1) RETURNING seq`, [c])))[0].seq;
  }
  return out;
}

// An edge between tickets by code (the actor is already set).
const link = (sql, src, rel, dst) =>
  sql(`INSERT INTO graph.edge(src, src_kind, rel, dst, dst_kind)
    SELECT s.node_id, 'ticket', $2, d.node_id, 'ticket' FROM graph.ticket s JOIN tickets st ON st.seq = s.ticket_seq,
      graph.ticket d JOIN tickets dt ON dt.seq = d.ticket_seq WHERE st.code = $1 AND dt.code = $3 RETURNING id`, [src, rel, dst]).then((r) => r[0].id);

const estimate = (sql, seq, sp, extra = {}) =>
  sql(`INSERT INTO plan.ticket_estimate(ticket_seq, effort_sp, aggressive_hd, remaining_hd) VALUES ($1, $2, $3, $4) RETURNING *`, [
    seq,
    sp,
    extra.a ?? null,
    extra.r ?? null
  ]).then((r) => r[0]);

const scope = (sql, seq, extra = '') => sql(`INSERT INTO plan.scope(root_seq${extra ? ', max_defaulted_pct' : ''}) VALUES ($1${extra ? ', ' + extra : ''}) RETURNING *`, [seq]);

// Queue state: root -> 1 if there is an unprocessed mark (marked > done), otherwise 0. How many times it was marked
// does not matter: inserting a reference table version, for example, marks twice (the insert and closing the previous row).
async function dirty(sql) {
  const rows = await sql(`SELECT t.code, (d.marked > d.done)::int AS p FROM plan.dirty d JOIN tickets t ON t.seq = d.root_seq ORDER BY t.code`);
  return Object.fromEntries(rows.map((r) => [r.code, r.p]));
}
// "Done" recomputation: done = marked everywhere (only service:plan raises done; the session actor does not change).
const settle = (db) =>
  db.transaction(async (tx) => {
    await tx.query(`SELECT set_config('hub.actor', 'hub-plan', true), set_config('hub.actor_kind', 'service:plan', true)`);
    await tx.query(`UPDATE plan.dirty SET done = marked`);
  });

await test('versions: an insert closes the live row, history stays, a version is only closed once, actor is written', async (t) => {
  const { sql, as } = await fresh(t);
  const { a } = await tickets(sql, 'a');
  await assert.rejects(estimate(sql, a, 3), code('GRACT'), 'no actor');
  await as('user:1', 'agent');
  const first = await estimate(sql, a, 3);
  assert.equal(first.actor, 'user:1');
  assert.equal(first.actor_kind, 'agent');
  await as('user:2', 'human');
  const second = await estimate(sql, a, 5, { a: 4 });
  const rows = await sql(`SELECT id, effort_sp, valid_from, valid_to, closed_by, actor FROM plan.ticket_estimate WHERE ticket_seq = $1 ORDER BY id`, [a]);
  assert.equal(rows.length, 2);
  assert.equal(rows[0].valid_to.getTime(), second.valid_from.getTime(), 'closed by the same command');
  assert.equal(rows[0].closed_by, 'user:2');
  assert.equal(rows[1].valid_to, null);
  assert.equal((await sql(`SELECT count(*)::int AS n FROM plan.ticket_estimate WHERE valid_to IS NULL`))[0].n, 1);

  // Editing a version is forbidden; closing only once; a closed one cannot be reopened.
  await assert.rejects(sql(`UPDATE plan.ticket_estimate SET effort_sp = 8 WHERE id = $1`, [second.id]), code('GRFRB'));
  await assert.rejects(sql(`UPDATE plan.ticket_estimate SET effort_sp = 8, valid_to = now() WHERE id = $1`, [second.id]), code('GRFRB'));
  await assert.rejects(sql(`UPDATE plan.ticket_estimate SET valid_to = NULL, closed_by = NULL WHERE id = $1`, [first.id]), code('GRFRB'));
  // Only a person can close a live row without a new version.
  await as('user:1', 'agent');
  await assert.rejects(sql(`UPDATE plan.ticket_estimate SET valid_to = now() WHERE id = $1`, [second.id]), code('GRFRB'));
  await as('user:2', 'human');
  await sql(`UPDATE plan.ticket_estimate SET valid_to = now() WHERE id = $1`, [second.id]);
  assert.equal((await sql(`SELECT closed_by FROM plan.ticket_estimate WHERE id = $1`, [second.id]))[0].closed_by, 'user:2');
  await assert.rejects(sql(`UPDATE plan.ticket_estimate SET valid_to = now() WHERE id = $1`, [second.id]), code('GRFRB'));

  // The database holds the bounds of estimates and WSJF.
  await assert.rejects(estimate(sql, a, 4), code('23514'), 'not Fibonacci');
  await assert.rejects(sql(`INSERT INTO plan.ticket_estimate(ticket_seq, effort_sp, aggressive_hd, safe_hd) VALUES ($1, 3, 4, 3)`, [a]), code('23514'));
  await assert.rejects(
    sql(`INSERT INTO plan.ticket_wsjf(ticket_seq, business_value, time_criticality, risk_opportunity) VALUES ($1, 4, 1, 1)`, [a]),
    code('23514')
  );
  const w = (await sql(`INSERT INTO plan.ticket_wsjf(ticket_seq, business_value, time_criticality, risk_opportunity) VALUES ($1, 5, 3, 2) RETURNING *`, [a]))[0];
  assert.equal(w.actor_kind, 'human');

  // No table in plan has deletes or truncation (tools/test-graph.mjs also checks TRUNCATE of each by the triggers).
  const tables = ['baseline', 'current', 'dirty', 'fever_point', 'points_to_hd', 'scope', 'ticket_estimate', 'ticket_wsjf', 'type_default_estimate'];
  const guarded = async (name) =>
    (await sql(`SELECT c.relname FROM pg_trigger g JOIN pg_class c ON c.oid = g.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace
      WHERE n.nspname = 'plan' AND g.tgname = $1 ORDER BY 1`, [name])).map((r) => r.relname);
  assert.deepEqual(await guarded('forbid_delete'), tables);
  assert.deepEqual(await guarded('forbid_truncate'), tables);
  for (const table of ['ticket_estimate', 'ticket_wsjf', 'type_default_estimate']) {
    await assert.rejects(sql(`DELETE FROM plan.${table}`), code('GRFRB'), `DELETE plan.${table}`);
  }
  for (const table of tables) {
    await assert.rejects(sql(`TRUNCATE plan.${table} CASCADE`), code('GRFRB'), `TRUNCATE plan.${table}`);
  }
});

await test('actor kinds: dictionaries need a human or seed, current and fever only service:plan; seeded defaults', async (t) => {
  const { sql, as } = await fresh(t);
  assert.deepEqual(
    // The defaults of the instance account: seed_instance copies them for a new account (seed:instance).
    await sql(`SELECT tag, effort_sp, actor, actor_kind FROM plan.type_default_estimate
      WHERE valid_to IS NULL AND account = public.instance_account() ORDER BY tag`),
    [
      { tag: '*', effort_sp: 3, actor: 'seed:instance', actor_kind: 'seed' },
      { tag: 'bug', effort_sp: 2, actor: 'seed:instance', actor_kind: 'seed' },
      { tag: 'chore', effort_sp: 2, actor: 'seed:instance', actor_kind: 'seed' },
      { tag: 'feature', effort_sp: 5, actor: 'seed:instance', actor_kind: 'seed' }
    ]
  );
  const { r } = await tickets(sql, 'r');
  await as('user:1', 'agent');
  await assert.rejects(sql(`INSERT INTO plan.points_to_hd(effort_sp, aggressive_hd) VALUES (3, 4)`), code('PLKND'));
  await assert.rejects(sql(`INSERT INTO plan.type_default_estimate(tag, effort_sp) VALUES ('bug', 3)`), code('PLKND'));
  await as('hub-ingest', 'service:ingest');
  await assert.rejects(estimate(sql, r, 3), code('PLKND'));
  await assert.rejects(scope(sql, r), code('PLKND'));
  await as('user:1', 'human');
  await sql(`INSERT INTO plan.points_to_hd(effort_sp, aggressive_hd) VALUES (3, 4)`);
  await sql(`INSERT INTO plan.type_default_estimate(tag, effort_sp) VALUES ('bug', 3)`);
  assert.deepEqual(await sql(`SELECT effort_sp FROM plan.type_default_estimate WHERE tag = 'bug' AND account = public.instance_account() ORDER BY id`),
    [{ effort_sp: 2 }, { effort_sp: 3 }]);

  const current = () =>
    sql(`INSERT INTO plan.current(root_seq, input_hash, algo_version, today, result) VALUES ($1, sha256('x'), 1, current_date, '{"tasks":[]}')
      ON CONFLICT (account, root_seq) DO UPDATE SET result = excluded.result RETURNING actor`, [r]);
  await assert.rejects(current(), code('PLKND'), 'human');
  await as('user:1', 'agent');
  await assert.rejects(current(), code('PLKND'), 'agent');
  await as('hub-plan', 'service:plan');
  assert.deepEqual(await current(), [{ actor: 'hub-plan' }]);
  assert.deepEqual(await current(), [{ actor: 'hub-plan' }], 'upsert');
  await assert.rejects(scope(sql, r), code('PLKND'), 'the planner does not write scopes');
});

await test('baseline: insert only, human only, one live per scope, the defaults threshold unless accepted', async (t) => {
  const { sql, as } = await fresh(t);
  const { r } = await tickets(sql, 'r');
  const base = (supersedes, pct, accepted = false) =>
    sql(`INSERT INTO plan.baseline(root_seq, supersedes, scope_id, defaulted_pct, accepted_defaults, algo_version, result)
      VALUES ($1, $2, 0, $3, $4, 1, '{"chain":[]}') RETURNING *`, [r, supersedes, pct, accepted]).then((x) => x[0]);
  await as('user:1', 'human');
  await assert.rejects(base(null, 0), code('PLSCP'), 'no scope yet');
  await scope(sql, r, '20');
  await as('user:1', 'agent');
  await assert.rejects(base(null, 0), code('GRHUM'));
  await as('user:1', 'human');
  await assert.rejects(base(null, 25), code('PLDEF'));
  const b1 = await base(null, 20);
  assert.equal(b1.approved_by, 'user:1');
  assert.equal(b1.approved_kind, 'human');
  assert.equal(b1.max_defaulted_pct, 20);
  const live = await sql(`SELECT id FROM plan.scope WHERE root_seq = $1 AND valid_to IS NULL`, [r]);
  assert.equal(b1.scope_id, live[0].id, 'scope version from the live row');
  // A second baseline without replacing the live one and replacing a non-live one are rejected; replacing the live one passes.
  await assert.rejects(base(null, 0), code('PLSUP'));
  const b2 = await base(b1.id, 40, true);
  assert.equal(b2.accepted_defaults, true);
  await assert.rejects(base(b1.id, 0), code('PLSUP'), 'b1 is no longer live');
  assert.deepEqual(await sql(`SELECT id FROM plan.baseline_live`), [{ id: b2.id }]);
  // Repeating the same replacement also hits the index (parallel approvals, bypassing the trigger).
  await sql(`ALTER TABLE plan.baseline DISABLE TRIGGER baseline_guard`);
  await assert.rejects(base(b1.id, 0), (e) => e.code === '23505' && /baseline_succession/.test(e.message));
  await sql(`ALTER TABLE plan.baseline ENABLE TRIGGER baseline_guard`);
  // Insert only.
  await assert.rejects(sql(`UPDATE plan.baseline SET accepted_defaults = false WHERE id = $1`, [b2.id]), code('GRFRB'));
  await assert.rejects(sql(`DELETE FROM plan.baseline WHERE id = $1`, [b1.id]), code('GRFRB'));
});

await test('dirty queue: estimates, status, both ends of blocks, old and new part_of parent, assignee, dictionaries, current', async (t) => {
  const { db, sql, as } = await fresh(t);
  // Scopes A (a1, a2 under it) and B (b1 under it); x outside scopes.
  const s = await tickets(sql, 'ra', 'a1', 'a2', 'rb', 'b1', 'x');
  await as('user:1', 'human');
  await link(sql, 'a1', 'part_of', 'ra');
  await link(sql, 'a2', 'part_of', 'a1');
  await link(sql, 'b1', 'part_of', 'rb');
  assert.deepEqual(await dirty(sql), {}, 'no scopes: nothing to mark');
  await scope(sql, s.ra);
  await scope(sql, s.rb);
  assert.deepEqual(await dirty(sql), { ra: 1, rb: 1 });
  assert.deepEqual(await sql(`SELECT plan.roots_of('demo', ARRAY[$1::bigint, $2::bigint, $3::bigint]) AS r`, [s.a2, s.b1, s.x]), [{ r: [s.ra, s.rb] }]);
  await settle(db);

  // An estimate and WSJF of a grandchild mark the root; repeated marks before recomputation accumulate in marked.
  await estimate(sql, s.a2, 3);
  const [m1] = await sql(`SELECT marked FROM plan.dirty WHERE root_seq = $1`, [s.ra]);
  await sql(`INSERT INTO plan.ticket_wsjf(ticket_seq, business_value, time_criticality, risk_opportunity) VALUES ($1, 1, 1, 1)`, [s.a2]);
  const [m2] = await sql(`SELECT marked FROM plan.dirty WHERE root_seq = $1`, [s.ra]);
  assert.ok(m2.marked > m1.marked);
  assert.deepEqual(await dirty(sql), { ra: 1, rb: 0 });
  await settle(db);
  await estimate(sql, s.x, 3);
  assert.deepEqual(await dirty(sql), { ra: 0, rb: 0 }, 'a ticket outside scopes marks nothing');

  // Ticket status: its scope; an edit without changing marked fields marks nothing.
  await sql(`UPDATE tickets SET status = 'doing' WHERE seq = $1`, [s.b1]);
  await sql(`UPDATE tickets SET summary = 'another summary' WHERE seq = $1`, [s.a1]);
  assert.deepEqual(await dirty(sql), { ra: 0, rb: 1 });
  await settle(db);

  // blocks between scopes: both. A blocker outside scopes: its status marks the scope of the one it blocks.
  await link(sql, 'a1', 'blocks', 'b1');
  assert.deepEqual(await dirty(sql), { ra: 1, rb: 1 });
  await link(sql, 'x', 'blocks', 'a2');
  await settle(db);
  await sql(`UPDATE tickets SET status = 'done' WHERE seq = $1`, [s.x]);
  assert.deepEqual(await dirty(sql), { ra: 1, rb: 0 });
  await settle(db);

  // Moving a2 from A to B: closing the old part_of marks A, the new edge B.
  await sql(`UPDATE graph.edge SET valid_to = now(), close_reason = 'removed' WHERE rel = 'part_of'
    AND src = (SELECT node_id FROM graph.ticket WHERE ticket_seq = $1)`, [s.a2]);
  assert.deepEqual(await dirty(sql), { ra: 1, rb: 0 });
  await link(sql, 'a2', 'part_of', 'b1');
  assert.deepEqual(await dirty(sql), { ra: 1, rb: 1 });
  await settle(db);

  // Assignee: the scopes of the ticket and of all live tickets of this person.
  await sql(`SELECT graph.import_people('[{"handle":"alice","name":"Alice"}]'::jsonb)`);
  await as('user:1', 'human');
  const assign = (seq) =>
    sql(`INSERT INTO graph.edge(src, src_kind, rel, dst, dst_kind) SELECT gt.node_id, 'ticket', 'assigned_to', n.id, 'person'
      FROM graph.ticket gt, graph.node n WHERE gt.ticket_seq = $1 AND n.kind = 'person' AND n.key = 'alice'`, [seq]);
  await assign(s.a1);
  assert.deepEqual(await dirty(sql), { ra: 1, rb: 0 });
  await settle(db);
  await assign(s.b1);
  assert.deepEqual(await dirty(sql), { ra: 1, rb: 1 }, 'alice already holds a1 in A');
  await settle(db);

  // Point reference tables: all scopes.
  await sql(`INSERT INTO plan.points_to_hd(effort_sp, aggressive_hd) VALUES (5, 6)`);
  assert.deepEqual(await dirty(sql), { ra: 1, rb: 1 });
  await settle(db);
  await sql(`SELECT plan.mark_all_now('demo')`);
  assert.deepEqual(await dirty(sql), { ra: 1, rb: 1 });
  await settle(db);

  // A new current plan of A changes the milestones of its tasks for B (a1 blocks b1), but not for A itself.
  await as('hub-plan', 'service:plan');
  await sql(`INSERT INTO plan.current(root_seq, input_hash, algo_version, today, result)
    VALUES ($1, sha256('a'), 1, current_date, jsonb_build_object('tasks', jsonb_build_array(jsonb_build_object('seq', $2::bigint))))`, [s.ra, s.a1]);
  assert.deepEqual(await dirty(sql), { ra: 0, rb: 1 });

  // Queue without deletes: since holds the first unprocessed mark; done is not above marked.
  const [before] = await sql(`SELECT since FROM plan.dirty WHERE root_seq = $1`, [s.rb]);
  await sql(`SELECT plan.mark_now('demo', ARRAY[$1::bigint])`, [s.rb]);
  assert.deepEqual(await sql(`SELECT since FROM plan.dirty WHERE root_seq = $1`, [s.rb]), [before]);
  await assert.rejects(sql(`UPDATE plan.dirty SET done = marked + 1`), code('23514'));

  // Counters only grow, only recomputation raises done; a new row always has one mark.
  await as('user:1', 'agent');
  await assert.rejects(sql(`UPDATE plan.dirty SET done = marked`), code('PLKND'));
  await assert.rejects(sql(`UPDATE plan.dirty SET marked = 0, done = 0`), code('GRFRB'));
  await assert.rejects(sql(`UPDATE plan.dirty SET root_seq = $1 WHERE root_seq = $2`, [s.x, s.ra]), code('GRFRB'));
  await sql(`UPDATE plan.dirty SET marked = marked + 1 WHERE root_seq = $1`, [s.ra]);
  await settle(db);

  // Marks accumulate until commit: the queue rows are untouched inside the transaction, one write per scope at COMMIT.
  const marks = async () => Object.fromEntries((await sql(`SELECT root_seq, marked FROM plan.dirty`)).map((r) => [r.root_seq, r.marked]));
  const n0 = await marks();
  await db.transaction(async (tx) => {
    await tx.query(`SELECT set_config('hub.actor', 'user:1', true), set_config('hub.actor_kind', 'agent', true)`);
    await tx.query(`UPDATE tickets SET status = 'doing' WHERE seq = $1`, [s.a1]);
    await tx.query(`UPDATE tickets SET status = 'doing' WHERE seq = $1`, [s.a2]);
    await tx.query(`UPDATE tickets SET priority = 'p1' WHERE seq = $1`, [s.b1]);
    const inside = Object.fromEntries((await tx.query(`SELECT root_seq, marked FROM plan.dirty`)).rows.map((r) => [r.root_seq, r.marked]));
    assert.deepEqual(inside, n0, 'nothing written before commit');
  });
  const n1 = await marks();
  assert.equal(n1[s.ra], n0[s.ra] + 1, 'one mark per scope per transaction');
  assert.equal(n1[s.rb], n0[s.rb] + 1);
});

await test('scope thresholds: only a human or seed changes them; an agent keeps the live values', async (t) => {
  const { sql, as } = await fresh(t);
  const { r } = await tickets(sql, 'r');
  const put = (cols, vals) => sql(`INSERT INTO plan.scope(root_seq, ${cols}) VALUES ($1, ${vals}) RETURNING *`, [r]).then((x) => x[0]);
  await as('user:1', 'agent');
  await assert.rejects(put('max_defaulted_pct', '100'), code('PLTHR'), 'first version, not the default');
  await assert.rejects(put('green_a_pct', '5'), code('PLTHR'));
  const s1 = await put('due_date', "'2030-01-01'");
  assert.equal(s1.max_defaulted_pct, 20);
  await as('user:2', 'human');
  const s2 = await put('max_defaulted_pct, zone_slope_pct', '40, 70');
  assert.equal(s2.actor_kind, 'human');
  await as('user:1', 'agent');
  await assert.rejects(put('max_defaulted_pct, zone_slope_pct', '20, 60'), code('PLTHR'), 'back to defaults is a change too');
  await assert.rejects(put('max_defaulted_pct, zone_slope_pct', '100, 70'), code('PLTHR'));
  const s3 = await put('max_defaulted_pct, zone_slope_pct, due_date', "40, 70, '2030-02-01'");
  assert.equal(s3.actor_kind, 'agent');
  // An agent does not remove a scope without a new version; a person can.
  await assert.rejects(sql(`UPDATE plan.scope SET valid_to = now() WHERE valid_to IS NULL`), code('GRFRB'));
  assert.equal((await sql(`SELECT count(*)::int AS n FROM plan.scope WHERE valid_to IS NULL`))[0].n, 1);
  await as('user:2', 'human');
  await sql(`UPDATE plan.scope SET valid_to = now() WHERE valid_to IS NULL`);
  assert.equal((await sql(`SELECT count(*)::int AS n FROM plan.scope WHERE valid_to IS NULL`))[0].n, 0);
});

// The "swing" of nested scopes (part_of is single, so a ticket in two scopes lies in the outer A and in the
// nested B). A new plan wakes only the scopes that read its milestones as an external blocker, and only when end shifts.
await test('current plan marks only scopes that read its milestones, and only when an end moves (no ping-pong)', async (t) => {
  const { db, sql, as } = await fresh(t);
  const s = await tickets(sql, 'ra', 'rb', 'x', 't', 'w');
  await as('user:1', 'human');
  await link(sql, 'rb', 'part_of', 'ra');
  await link(sql, 'x', 'part_of', 'ra');
  await link(sql, 'w', 'part_of', 'ra');
  await link(sql, 't', 'part_of', 'rb');
  await link(sql, 'x', 'blocks', 't'); // B reads the end of x from the plan of A
  await link(sql, 't', 'blocks', 'w'); // A computes t itself: it is its own task
  await scope(sql, s.ra);
  await scope(sql, s.rb);
  assert.deepEqual(await sql(`SELECT plan.roots_of('demo', ARRAY[$1::bigint]) AS r`, [s.t]), [{ r: [s.ra, s.rb] }], 't is in both');
  await settle(db);

  await as('hub-plan', 'service:plan');
  const task = (seq, end) => ({ seq, end });
  const write = (rootSeq, hash, tasks) =>
    sql(`INSERT INTO plan.current(root_seq, input_hash, algo_version, today, result)
      VALUES ($1, sha256(convert_to($2, 'UTF8')), 1, current_date, jsonb_build_object('tasks', $3::jsonb))
      ON CONFLICT (account, root_seq) DO UPDATE SET input_hash = excluded.input_hash, result = excluded.result`,
      [rootSeq, hash, JSON.stringify(tasks)]);

  // Plan of B: t blocks w outside B, but w is in A, and t is A's own task. This used to wake A (an extra pass of A after
  // every write of B).
  await write(s.rb, 'b1', [task(s.rb, 6), task(s.t, 6)]);
  assert.deepEqual(await dirty(sql), { ra: 0, rb: 0 }, 'B does not wake A');
  // Plan of A: x outside B blocks t in B. t used to be skipped as a task of A, and B stayed with the old milestone of x.
  await write(s.ra, 'a1', [task(s.ra, 9), task(s.rb, 6), task(s.t, 6), task(s.w, 9), task(s.x, 4)]);
  assert.deepEqual(await dirty(sql), { ra: 0, rb: 1 }, 'A wakes B: B reads the end of x');
  await settle(db);

  // A new computation of A without a milestone shift (another input_hash, the same end) wakes nobody; a shift of only w (blocks
  // nothing outside A) neither; a shift of x wakes B.
  await write(s.ra, 'a2', [task(s.ra, 9), task(s.rb, 6), task(s.t, 6), task(s.w, 9), task(s.x, 4)]);
  await write(s.ra, 'a3', [task(s.ra, 10), task(s.rb, 6), task(s.t, 6), task(s.w, 10), task(s.x, 4)]);
  assert.deepEqual(await dirty(sql), { ra: 0, rb: 0 }, 'same milestones: no marks');
  await write(s.ra, 'a4', [task(s.ra, 10), task(s.rb, 7), task(s.t, 7), task(s.w, 10), task(s.x, 5)]);
  assert.deepEqual(await dirty(sql), { ra: 0, rb: 1 });
  await settle(db);
  // B's answer with a shift of t does not wake A again: the circle A -> B -> A does not close.
  await write(s.rb, 'b2', [task(s.rb, 8), task(s.t, 8)]);
  assert.deepEqual(await dirty(sql), { ra: 0, rb: 0 });
  // A task disappeared from the plan: its former readers are marked.
  await write(s.ra, 'a5', [task(s.ra, 10), task(s.rb, 7), task(s.t, 7), task(s.w, 10)]);
  assert.deepEqual(await dirty(sql), { ra: 0, rb: 1 }, 'x left the plan of A');
});
