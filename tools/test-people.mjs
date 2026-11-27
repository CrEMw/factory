// People, components, assignee (migration 0012_people_components) on PGlite in memory:
// a new assignee closes the previous one as superseded in the same transaction, the seed import is idempotent, carries
// tags into components once (when the component is created) and does not bring back removed links, attributes of people
// and components are not deleted and do not change key, handle none and an address in another case do not pass.
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

const PEOPLE = [
  { handle: 'alice', name: 'Alice' },
  { handle: 'bob', name: 'Bob', aliases: ['Bobby'] }
];
const COMPONENTS = [
  { slug: 'cockpit', title: 'Cockpit', project: 'apps', tags: ['cockpit'] },
  { slug: 'hub', title: 'Hub', description: 'Tickets', project: 'widgets', tags: ['roadmap', 'hub'] }
];

const importPeople = (sql, people = PEOPLE) => sql(`SELECT graph.import_people($1::jsonb) AS r`, [JSON.stringify(people)]).then((r) => r[0].r);
const importComponents = (sql, components = COMPONENTS) =>
  sql(`SELECT graph.import_components($1::jsonb) AS r`, [JSON.stringify(components)]).then((r) => r[0].r);

async function ticket(sql, code, tags = []) {
  await sql(`INSERT INTO tickets(project, code, summary, tags) VALUES ('apps', $1, $1, $2)`, [code, tags]);
  return (await sql(`SELECT gt.node_id AS id FROM graph.ticket gt JOIN tickets t ON t.seq = gt.ticket_seq WHERE t.code = $1`, [code]))[0].id;
}

const nodeOf = async (sql, kind, key) => (await sql(`SELECT id FROM graph.node WHERE kind = $1 AND key = $2`, [kind, key]))[0].id;

const assign = (sql, src, dst, state = 'confirmed') =>
  sql(`INSERT INTO graph.edge(src, src_kind, rel, dst, dst_kind, state) VALUES ($1, 'ticket', 'assigned_to', $2, 'person', $3) RETURNING id, state, created_at`, [
    src,
    dst,
    state
  ]).then((r) => r[0]);

const assignees = (sql, src) =>
  sql(`SELECT n.key, e.state, e.valid_to IS NULL AS live, e.close_reason, e.closed_by FROM graph.edge e JOIN graph.node n ON n.id = e.dst
    WHERE e.src = $1 AND e.rel = 'assigned_to' ORDER BY e.id`, [src]);

await test('a new assignee closes the old one with superseded in the same transaction', async (t) => {
  const { db, sql, as } = await fresh(t);
  await importPeople(sql);
  const [alice, bob] = [await nodeOf(sql, 'person', 'alice'), await nodeOf(sql, 'person', 'bob')];
  await as('user:1', 'agent');
  const a = await ticket(sql, 'a');
  await assign(sql, a, alice);

  // A change in a transaction: the previous edge is closed by the same transaction (valid_to = now() = created_at of the new one).
  await db.exec('BEGIN');
  const second = await assign(sql, a, bob);
  const closed = (await sql(`SELECT valid_to FROM graph.edge WHERE src = $1 AND rel = 'assigned_to' AND close_reason = 'superseded'`, [a]))[0];
  assert.equal(closed.valid_to.getTime(), second.created_at.getTime(), 'closed in the same transaction');
  await db.exec('ROLLBACK');
  assert.deepEqual(await assignees(sql, a), [{ key: 'alice', state: 'confirmed', live: true, close_reason: null, closed_by: null }], 'rollback keeps the old one live');

  await assign(sql, a, bob);
  assert.deepEqual(await assignees(sql, a), [
    { key: 'alice', state: 'confirmed', live: false, close_reason: 'superseded', closed_by: 'user:1' },
    { key: 'bob', state: 'confirmed', live: true, close_reason: null, closed_by: null }
  ]);
  // The same assignee a second time: exists by edge_live_unique, not a close and a new row.
  await assert.rejects(assign(sql, a, bob), (e) => e.code === '23505' && /edge_live_unique/.test(e.message));
  // A proposal takes no slot and does not close the previous one; a person's confirmation closes it.
  await as('user:1', 'human');
  const p = await assign(sql, a, alice, 'proposed');
  assert.equal((await assignees(sql, a)).filter((r) => r.live).length, 2);
  await sql(`UPDATE graph.edge SET state = 'confirmed' WHERE id = $1`, [p.id]);
  const live = (await assignees(sql, a)).filter((r) => r.live);
  assert.deepEqual(live.map((r) => r.key), ['alice']);
  // An agent cannot change an assignee set by a person: GRHCL, and the new edge rolls back together with the close.
  await as('user:1', 'agent');
  await assert.rejects(assign(sql, a, bob), code('GRHCL'));
  assert.deepEqual((await assignees(sql, a)).filter((r) => r.live).map((r) => r.key), ['alice']);
  // Log: the assignee change is visible as edge rows at the ticket.
  const log = await sql(`SELECT new->>'dst' AS dst, new->>'close_reason' AS reason FROM ticket_change WHERE field = 'edge:assigned_to' ORDER BY id`);
  assert.ok(log.some((r) => r.dst === 'alice' && r.reason === 'superseded'), JSON.stringify(log));
});

await test('import of people and components is idempotent and does not bring back removed links', async (t) => {
  const { sql, as } = await fresh(t);
  await as('user:1', 'human');
  const a = await ticket(sql, 'a', ['cockpit', 'bug']);
  await ticket(sql, 'b', ['roadmap']);
  await ticket(sql, 'c', ['bug']);
  await sql(`SELECT set_config('hub.actor', '', false), set_config('hub.actor_kind', '', false)`);

  assert.deepEqual(await importPeople(sql), { created: 2, updated: 0, unchanged: 0 });
  assert.deepEqual(await importPeople(sql), { created: 0, updated: 0, unchanged: 2 });
  assert.deepEqual(await importPeople(sql, [{ handle: 'alice', name: 'Alice A.' }, PEOPLE[1]]), { created: 0, updated: 1, unchanged: 1 });
  assert.deepEqual(await sql(`SELECT handle, name, aliases FROM graph.person ORDER BY handle`), [
    { handle: 'alice', name: 'Alice A.', aliases: [] },
    { handle: 'bob', name: 'Bob', aliases: ['Bobby'] }
  ]);

  assert.deepEqual(await importComponents(sql), { created: 2, updated: 0, unchanged: 0, projects: 2, tickets: 2 });
  const counts = async () => (await sql(`SELECT count(*)::int AS n FROM graph.edge`))[0].n;
  const before = await counts();
  assert.deepEqual(await importComponents(sql), { created: 0, updated: 0, unchanged: 2, projects: 0, tickets: 0 });
  assert.equal(await counts(), before, 'second import adds no edges');
  const links = await sql(`SELECT graph.node_label(src) AS src, rel, graph.node_label(dst) AS dst, created_by, confirmed_kind
    FROM graph.edge ORDER BY id`);
  assert.deepEqual(links, [
    { src: 'cockpit', rel: 'component_of', dst: 'apps', created_by: 'seed:components', confirmed_kind: 'seed' },
    { src: 'a', rel: 'in_component', dst: 'cockpit', created_by: 'seed:components', confirmed_kind: 'seed' },
    { src: 'hub', rel: 'component_of', dst: 'widgets', created_by: 'seed:components', confirmed_kind: 'seed' },
    { src: 'b', rel: 'in_component', dst: 'hub', created_by: 'seed:components', confirmed_kind: 'seed' }
  ]);
  assert.deepEqual(await sql(`SELECT tags FROM tickets WHERE code = 'a'`), [{ tags: ['cockpit', 'bug'] }], 'tags stay');

  // Tags are carried over once: the import does not bring back a removed component, a new ticket with a tag does not get
  // the component by tag (the outcome does not depend on when the pod restarted), and a new component in the file carries tags at once.
  await as('user:1', 'agent');
  const cockpit = await nodeOf(sql, 'component', 'cockpit');
  await sql(`UPDATE graph.edge SET valid_to = now(), close_reason = 'removed' WHERE src = $1 AND dst = $2`, [a, cockpit]);
  await ticket(sql, 'd', ['cockpit']);
  await sql(`SELECT set_config('hub.actor', '', false), set_config('hub.actor_kind', '', false)`);
  const changed = { ...COMPONENTS[1], title: 'Ticket hub' };
  const bugs = { slug: 'bugs', title: 'Bugs', project: 'apps', tags: ['bug'] };
  assert.deepEqual(await importComponents(sql, [COMPONENTS[0], changed, bugs]), { created: 1, updated: 1, unchanged: 1, projects: 1, tickets: 2 });
  const live = async (slug) =>
    (await sql(`SELECT graph.node_label(src) AS code FROM graph.edge WHERE dst = $1 AND rel = 'in_component' AND valid_to IS NULL ORDER BY src`, [await nodeOf(sql, 'component', slug)])).map((r) => r.code);
  assert.deepEqual(await live('cockpit'), [], 'removed stays removed, d gets nothing from its tag');
  assert.deepEqual(await live('bugs'), ['a', 'c'], 'a new component takes its tags once');
  assert.deepEqual(await importComponents(sql, [COMPONENTS[0], changed]), { created: 0, updated: 0, unchanged: 2, projects: 0, tickets: 0 });
  assert.deepEqual(await live('bugs'), ['a', 'c'], 'leaving the file does not close edges');
  // A person moving a component to another project is not rolled back by the import.
  await as('user:1', 'human');
  await sql(`UPDATE graph.edge SET valid_to = now(), close_reason = 'removed' WHERE src = $1 AND rel = 'component_of'`, [cockpit]);
  // Another project node of the account (the document repository registry may have put it already).
  await sql(`INSERT INTO graph.node(kind, key) VALUES ('project', 'platform') ON CONFLICT DO NOTHING`);
  const platform = (await sql(`SELECT id FROM graph.node WHERE kind = 'project' AND key = 'platform' AND account = public.instance_account()`))[0].id;
  await sql(`INSERT INTO graph.edge(src, src_kind, rel, dst, dst_kind) VALUES ($1, 'component', 'component_of', $2, 'project')`, [cockpit, platform]);
  assert.equal((await importComponents(sql)).projects, 0);
  // A retired component gets no new edges.
  await sql(`UPDATE graph.node SET retired_at = now(), retire_reason = 'test' WHERE id = $1`, [cockpit]);
  await ticket(sql, 'e', ['cockpit']);
  assert.equal((await importComponents(sql)).tickets, 0);
});

await test('people and components: checks, key match, actor, no delete, identity frozen', async (t) => {
  const { sql, as } = await fresh(t);
  await importPeople(sql);
  await importComponents(sql);
  await assert.rejects(importPeople(sql, [{ handle: 'Alice', name: 'x' }]), (e) => e.code === '23514', 'bad handle');
  await assert.rejects(importPeople(sql, [{ handle: '1ed', name: 'x' }]), (e) => e.code === '23514', 'handle starts with a letter');
  await assert.rejects(importPeople(sql, [{ handle: 'none', name: 'x' }]), (e) => e.code === '23514', 'none is the no-assignee filter');
  await assert.rejects(importComponents(sql, [{ slug: 'bad_slug', title: 'x', project: 'apps' }]), (e) => e.code === '23514', 'bad slug');
  await assert.rejects(importPeople(sql, { handle: 'alice' }), (e) => e.code === '22023', 'not an array');
  await assert.rejects(
    importPeople(sql, [{ handle: 'ann', name: 'A', email: 'a@x.test' }, { handle: 'bob', name: 'B', email: 'A@X.test' }]),
    (e) => e.code === '23505' && /person_email_unique/.test(e.message),
    'email unique per account'
  );
  // Without an actor, writing attributes is rejected; the row must match the key of its node.
  await sql(`SELECT set_config('hub.actor', '', false), set_config('hub.actor_kind', '', false)`);
  const zed = (await sql(`INSERT INTO graph.node(kind, key) VALUES ('person', 'zed') RETURNING id`))[0].id;
  await assert.rejects(sql(`INSERT INTO graph.person(node_id, handle, name) VALUES ($1, 'zed', 'Z')`, [zed]), code('GRACT'));
  await as('user:1', 'human');
  await assert.rejects(sql(`INSERT INTO graph.person(node_id, handle, name) VALUES ($1, 'zoe', 'Z')`, [zed]), code('GRKEY'));
  await sql(`INSERT INTO graph.person(node_id, handle, name) VALUES ($1, 'zed', 'Z')`, [zed]);
  for (const q of [
    `UPDATE graph.person SET handle = 'alicia' WHERE handle = 'alice'`,
    `UPDATE graph.component SET slug = 'cock' WHERE slug = 'cockpit'`,
    `DELETE FROM graph.person`,
    `DELETE FROM graph.component`,
    `TRUNCATE graph.person CASCADE`,
    `TRUNCATE graph.component CASCADE`
  ]) {
    await assert.rejects(sql(q), code('GRFRB'), q);
  }
  await sql(`UPDATE graph.person SET name = 'Alice' WHERE handle = 'alice'`);
  // Edge shapes: a component only at a ticket, an assignee only a person.
  const cockpit = await nodeOf(sql, 'component', 'cockpit');
  const alice = await nodeOf(sql, 'person', 'alice');
  await assert.rejects(
    sql(`INSERT INTO graph.edge(src, src_kind, rel, dst, dst_kind) VALUES ($1, 'component', 'assigned_to', $2, 'person')`, [cockpit, alice]),
    code('GRSHP')
  );
});

// Migration 0022_person_login. Binding a login to a person: only a person with a fresh sign-in and only
// for their own account, one live binding per account and per person, retirement once, other edits, deletion and clearing
// are forbidden; the binding makes the handle the owner of the MCP key. Only the owner by binding approves a mailbox key.
const bind = (sql, user, handle) =>
  sql(`INSERT INTO graph.person_login(user_id, handle) VALUES ($1, $2) RETURNING user_id, handle, actor, actor_kind`, [user, handle]).then((r) => r[0]);
const freshLogin = (sql, on = true) => sql(`SELECT set_config('hub.fresh', $1, false)`, [on ? 'on' : '']);

await test('person_login: only a fresh human binds own login, one live per login and per person', async (t) => {
  const { sql, as } = await fresh(t);
  await importPeople(sql, [...PEOPLE, { handle: 'bot', name: 'Bot', is_agent: true }, { handle: 'old', name: 'Old' }]);
  await as('user:u-alice', 'agent');
  await freshLogin(sql);
  await assert.rejects(bind(sql, 'u-alice', 'alice'), code('GRHUM'), 'agent');
  await as('seed:x', 'seed');
  await assert.rejects(bind(sql, 'u-alice', 'alice'), code('GRHUM'), 'seed');
  await as('user:u-alice', 'human');
  await freshLogin(sql, false);
  await assert.rejects(bind(sql, 'u-alice', 'alice'), code('GRFSH'), 'stale session');
  await freshLogin(sql);
  await assert.rejects(bind(sql, 'u-bob', 'bob'), code('GRLGN'), 'someone else login');
  await assert.rejects(bind(sql, 'u-alice', 'bot'), code('GRLGN'), 'agent person');
  await assert.rejects(bind(sql, 'u-alice', 'nobody'), (e) => e.code === '23503', 'unknown person');
  assert.deepEqual(await bind(sql, 'u-alice', 'alice'), { user_id: 'u-alice', handle: 'alice', actor: 'user:u-alice', actor_kind: 'human' });
  await assert.rejects(bind(sql, 'u-alice', 'bob'), (e) => e.code === '23505' && /person_login_user/.test(e.message), 'second login');
  await as('user:u-bob', 'human');
  await assert.rejects(bind(sql, 'u-bob', 'alice'), (e) => e.code === '23505' && /person_login_handle/.test(e.message), 'taken');
  // A retired person cannot be bound.
  await sql(`UPDATE graph.node SET retired_at = now(), retire_reason = 'test' WHERE kind = 'person' AND key = 'old'`);
  await assert.rejects(bind(sql, 'u-bob', 'old'), code('GRRET'), 'retired person');

  // Retirement: any person with a fresh sign-in, once; the database writes the actor.
  await as('user:u-bob', 'agent');
  await assert.rejects(sql(`UPDATE graph.person_login SET retired_at = now(), retired_by = 'x' WHERE handle = 'alice'`), code('GRHUM'));
  await as('user:u-bob', 'human');
  for (const q of [
    `UPDATE graph.person_login SET handle = 'bob' WHERE handle = 'alice'`,
    `UPDATE graph.person_login SET user_id = 'u-bob' WHERE handle = 'alice'`,
    `UPDATE graph.person_login SET actor = 'user:x' WHERE handle = 'alice'`,
    `DELETE FROM graph.person_login`,
    `TRUNCATE graph.person_login`
  ]) {
    await assert.rejects(sql(q), code('GRFRB'), q);
  }
  const [r] = await sql(`UPDATE graph.person_login SET retired_at = now(), retired_by = 'x' WHERE handle = 'alice' RETURNING retired_by`);
  assert.equal(r.retired_by, 'user:u-bob', 'retired_by is the actor');
  await assert.rejects(sql(`UPDATE graph.person_login SET retired_at = now(), retired_by = 'x' WHERE handle = 'alice'`), code('GRFRB'), 'retired once');
  // After retirement the real owner binds again.
  await as('user:u-alice', 'human');
  assert.equal((await bind(sql, 'u-alice', 'alice')).handle, 'alice');
});

await test('mailbox keys: the login owns its MCP key, only the bound owner with a fresh login reviews a key', async (t) => {
  const { sql, as } = await fresh(t);
  await importPeople(sql);
  // A new installation expects no legacy mailbox import, so the mailbox is open without one.
  const curl = 'sha256:' + 'a'.repeat(64);
  await sql(`SELECT mailbox.exchange($1, 'bob', 'bob-a', 'hi', now(), false)`, [curl]);
  await sql(`SELECT mailbox.exchange('user:u-alice', 'bob', 'bob-typo', 'hi', now(), true)`);
  const key = (id) => sql(`SELECT owner, status FROM mailbox.keys WHERE id = $1`, [id]).then((r) => r[0]);
  const review = (id, status) => sql(`SELECT status FROM mailbox.review_key($1, $2)`, [id, status]).then((r) => r[0].status);
  // Only review_key changes the status.
  await as('user:u-bob', 'human');
  await freshLogin(sql);
  await assert.rejects(sql(`UPDATE mailbox.keys SET status = 'approved' WHERE id = $1`, [curl]), code('GRFRB'));
  // Without a binding nobody can approve, not even the one whose handle equals the owner.
  await assert.rejects(review(curl, 'approved'), code('GRNLG'));
  // Binding of alice: their MCP key, registered with someone else's prefix, passes to them.
  await as('user:u-alice', 'human');
  await bind(sql, 'u-alice', 'alice');
  assert.deepEqual(await key('user:u-alice'), { owner: 'alice', status: 'approved' });
  // A non-owner (bound to alice) does not approve the key of bob.
  await assert.rejects(review(curl, 'approved'), code('GROWN'));
  // The owner: only with a fresh sign-in and only a person.
  await as('user:u-bob', 'human');
  await bind(sql, 'u-bob', 'bob');
  await freshLogin(sql, false);
  await assert.rejects(review(curl, 'approved'), code('GRFSH'));
  await freshLogin(sql);
  await as('user:u-bob', 'agent');
  await assert.rejects(review(curl, 'approved'), code('GRHUM'));
  await as('user:u-bob', 'human');
  await assert.rejects(review(curl, 'maybe'), (e) => e.code === '22023');
  await assert.rejects(review('sha256:' + 'b'.repeat(64), 'approved'), (e) => e.code === 'P0002');
  assert.equal(await review(curl, 'approved'), 'approved');
  assert.equal(await review(curl, 'revoked'), 'revoked');
  assert.deepEqual(await sql(`SELECT key_id, status, owner, actor, actor_kind FROM mailbox.key_review ORDER BY id`), [
    { key_id: curl, status: 'approved', owner: 'bob', actor: 'user:u-bob', actor_kind: 'human' },
    { key_id: curl, status: 'revoked', owner: 'bob', actor: 'user:u-bob', actor_kind: 'human' }
  ]);
  for (const q of [`UPDATE mailbox.key_review SET status = 'approved'`, `DELETE FROM mailbox.key_review`, `TRUNCATE mailbox.key_review`]) {
    await assert.rejects(sql(q), code('GRFRB'), q);
  }
  // The exchange still does not touch the status and runs without an actor.
  await sql(`SELECT set_config('hub.actor', '', false), set_config('hub.actor_kind', '', false)`);
  assert.equal((await sql(`SELECT mailbox.exchange($1, 'bob', 'bob-a', 'x', now(), false) AS r`, [curl]))[0].r.status, 'revoked');
});

await test('mailbox keys: 20 stray pending keys of an unbound owner are revoked by any bound human, registration opens again', async (t) => {
  const { sql, as } = await fresh(t);
  await importPeople(sql, [...PEOPLE, { handle: 'bot', name: 'Bot', is_agent: true }]);
  // A new installation expects no legacy mailbox import, so the mailbox is open without one.
  const exchange = (id, owner) =>
    sql(`SELECT mailbox.exchange($1, $2, $2 || '-a', 'x', now(), false) AS r`, [id, owner]).then((r) => r[0].r);
  const stray = Array.from({ length: 20 }, (_, i) => 'sha256:' + i.toString(16).padStart(64, '0'));
  // Owners without a binding: outside the graph (zzz), a graph agent (bot), a graph person without a binding (bob).
  for (const [i, id] of stray.entries()) {
    assert.equal((await exchange(id, i < 18 ? 'zzz' : i === 18 ? 'bot' : 'bob')).registered, true);
  }
  const blocked = 'sha256:' + 'f'.repeat(64);
  assert.equal((await exchange(blocked, 'alice')).registered, undefined, 'the shared limit of 20 pending is full');
  const review = (id, status) => sql(`SELECT status FROM mailbox.review_key($1, $2)`, [id, status]).then((r) => r[0].status);
  await as('user:u-alice', 'human');
  await freshLogin(sql);
  // Without its own binding it does not revoke garbage either.
  await assert.rejects(review(stray[0], 'revoked'), code('GRNLG'));
  await bind(sql, 'u-alice', 'alice');
  // Approving someone else's key is still impossible, even without the owner's binding.
  await assert.rejects(review(stray[0], 'approved'), code('GROWN'));
  await freshLogin(sql, false);
  await assert.rejects(review(stray[0], 'revoked'), code('GRFSH'));
  await freshLogin(sql);
  for (const id of stray) assert.equal(await review(id, 'revoked'), 'revoked');
  // A revoked key is no longer pending: only the owner can bring it back.
  await assert.rejects(review(stray[0], 'approved'), code('GROWN'));
  await assert.rejects(review(stray[0], 'revoked'), code('GROWN'));
  assert.deepEqual(
    await sql(`SELECT count(*)::int AS n, min(actor) AS actor, min(status) AS status FROM mailbox.key_review`),
    [{ n: 20, actor: 'user:u-alice', status: 'revoked' }]
  );
  // The limit is free: a new key registers.
  await sql(`SELECT set_config('hub.actor', '', false), set_config('hub.actor_kind', '', false)`);
  assert.equal((await exchange(blocked, 'alice')).registered, true);
  // A pending key of a bound owner is not revoked by someone else.
  await as('user:u-bob', 'human');
  await bind(sql, 'u-bob', 'bob');
  await assert.rejects(review(blocked, 'revoked'), code('GROWN'));
});
