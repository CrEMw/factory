// Agent mailbox logic (migration 0008) on an isolated PostgreSQL: PGlite in memory,
// no network and no database address. The cases were carried over from the former cockpit mailbox spec.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile, readdir } from 'node:fs/promises';
import test from 'node:test';

const { PGlite } = await import(new URL('../.tools/node_modules/@electric-sql/pglite/dist/index.js', import.meta.url));
const root = new URL('../', import.meta.url);
const read = (path) => readFile(new URL(path, root), 'utf8');

const T0 = Date.parse('2030-01-16T10:00:00Z');
const at = (ms) => new Date(T0 + ms).toISOString();
const keyId = (key) => 'sha256:' + createHash('sha256').update(key).digest('hex');
const ALICE = keyId('e'.repeat(40));
const BOB = keyId('m'.repeat(40));

const migrations = (await readdir(new URL('services/hub/migrations/', root))).filter((f) => f.endsWith('.sql')).sort();
const migrate = async (db, files) => {
  for (const file of files) await db.exec('BEGIN;\n' + (await read('services/hub/migrations/' + file)) + '\nCOMMIT;');
};
// The one legacy source that 0008 allowed by a literal, read from that migration, not repeated here.
const SOURCE_0008 = (await read('services/hub/migrations/0008_agent_mailbox.sql')).match(/check \(source = '([^']+)'\)/)[1];

// legacy: the source of the legacy history import the installation expects (public.instance.mailbox_legacy_import);
// upTo: apply only the migrations before this file name (the rest with m.migrate()).
async function fresh(t, { legacy = null, upTo = null } = {}) {
  const db = new PGlite();
  t.after(() => db.close());
  await db.exec(`CREATE ROLE app; CREATE ROLE hub; CREATE ROLE hub_web; GRANT ALL ON SCHEMA public TO app;
    DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO app', current_database()); END $$;
    SET ROLE app;`);
  const first = upTo ? migrations.filter((f) => f < upTo) : migrations;
  await migrate(db, first);
  // Like Job hub-db-migrate: the instance row (0023) after the migrations; without it the account default (0025) gives HUBIN.
  await db.exec("SELECT public.seed_instance('demo', 'Europe/Athens', 20, NULL, ARRAY['docs/superpowers'])");
  if (legacy) await db.query('UPDATE public.instance SET mailbox_legacy_import = $1 WHERE id', [legacy]);
  const sql = (q, p = []) => db.query(q, p).then((r) => r.rows);
  const one = async (q, p) => (await sql(q, p))[0];
  const exchange = (key, agent, msg, ms, trusted = false) =>
    one('SELECT mailbox.exchange($1, $2, $3, $4, $5::timestamptz, $6) AS r', [key, agent.slice(0, agent.indexOf('-')), agent, msg, at(ms), trusted]).then((r) => r.r);
  const importHistory = (h) => one('SELECT mailbox.import_history($1::jsonb) AS r', [JSON.stringify(h)]).then((r) => r.r);
  // Only mailbox.review_key by the owner bound by login with a fresh sign-in changes a key status.
  const approve = (key) =>
    db.transaction(async (tx) => {
      const { owner } = (await tx.query('SELECT owner FROM mailbox.keys WHERE id = $1', [key])).rows[0];
      await tx.query('SELECT graph.import_people($1::jsonb)', [JSON.stringify([{ handle: owner, name: owner }])]);
      await tx.query(`SELECT set_config('hub.actor', $1, true), set_config('hub.actor_kind', 'human', true), set_config('hub.fresh', 'on', true)`, [
        `user:h-${owner}`
      ]);
      await tx.query(
        `INSERT INTO graph.person_login(user_id, handle) SELECT $1, $2
         WHERE NOT EXISTS (SELECT 1 FROM graph.person_login WHERE handle = $2 AND retired_at IS NULL)`,
        [`h-${owner}`, owner]
      );
      await tx.query(`SELECT mailbox.review_key($1, 'approved')`, [key]);
    });
  const rest = () => migrate(db, migrations.filter((f) => !first.includes(f)));
  return { db, sql, one, exchange, importHistory, approve, migrate: rest };
}
const texts = (r) => r.messages.map((m) => `${m.from}: ${m.text}`);

await test('a new installation: the mailbox is open without any import, and no import is taken', async (t) => {
  const m = await fresh(t);
  assert.equal((await m.one('SELECT mailbox_legacy_import FROM public.instance')).mailbox_legacy_import, null);
  assert.deepEqual(await m.exchange(ALICE, 'alice-a', 'hi', 0), { status: 'pending', registered: true, messages: [] });
  await assert.rejects(m.importHistory({ source: 'old-mailbox', keys: [], cursors: [], messages: [] }), /unsupported mailbox source: old-mailbox \(expected: none\)/);
  assert.equal((await m.sql('SELECT * FROM mailbox.imports')).length, 0);
});

await test('a mailbox that expects a legacy import is closed until that history is imported', async (t) => {
  const m = await fresh(t, { legacy: 'old-mailbox' });
  assert.deepEqual(await m.exchange(ALICE, 'alice-a', 'hi', 0), { status: 'moving', messages: [] });
  assert.equal((await m.sql('SELECT * FROM mailbox.keys')).length, 0, 'closed mailbox registers nothing');
  await m.importHistory({ source: 'old-mailbox', keys: [], cursors: [], messages: [] });
  assert.equal((await m.exchange(ALICE, 'alice-a', 'hi', 0)).status, 'pending');
  assert.deepEqual(await m.sql('SELECT source FROM mailbox.imports'), [{ source: 'old-mailbox' }]);
});

await test('first contact registers pending, text is dropped until approval', async (t) => {
  const m = await fresh(t);
  const r = await m.exchange(ALICE, 'alice-a', 'hello', 0);
  assert.deepEqual(r, { status: 'pending', registered: true, messages: [] });
  assert.equal((await m.exchange(ALICE, 'alice-a', 'again', 1000)).status, 'pending');
  assert.equal((await m.sql('SELECT * FROM mailbox.messages')).length, 0);
  // While the key is pending, a typo in the owner prefix can be fixed.
  await m.exchange(ALICE, 'alicee-a', 'typo fixed', 2000);
  assert.equal((await m.one('SELECT owner FROM mailbox.keys WHERE id = $1', [ALICE])).owner, 'alicee');
  await m.exchange(ALICE, 'alice-a', 'back', 3000);
  await m.approve(ALICE);
  assert.deepEqual(await m.exchange(ALICE, 'alice-a', 'now', 4000), { status: 'approved', messages: [] });
  assert.equal((await m.exchange(ALICE, 'bob-a', 'spoof', 5000)).status, 'wrong-owner');
});

await test('sessions see other sessions, never themselves; new session starts at the head', async (t) => {
  const m = await fresh(t);
  for (const k of [ALICE, BOB]) { await m.exchange(k, k === ALICE ? 'alice-a' : 'bob-a', 'reg', 0); await m.approve(k); }
  await m.exchange(BOB, 'bob-a', 'start', 500);
  await m.exchange(ALICE, 'alice-a', 'one', 1000);
  await m.exchange(ALICE, 'alice-a', 'two', 40_000);
  assert.deepEqual(texts(await m.exchange(BOB, 'bob-a', 'hi', 80_000)), ['alice-a: one', 'alice-a: two']);
  assert.deepEqual(texts(await m.exchange(ALICE, 'alice-a', 'three', 120_000)), ['bob-a: hi']);
  // Another session of the same person sees the first one's messages, but not the history before it appeared.
  assert.deepEqual(texts(await m.exchange(ALICE, 'alice-b', 'b here', 160_000)), []);
  assert.deepEqual(texts(await m.exchange(BOB, 'bob-a', 'x', 200_000)), ['alice-a: three', 'alice-b: b here']);
});

await test('a retry inside 30 s gets the same batch; the window is anchored to the first hand-out', async (t) => {
  const m = await fresh(t);
  for (const k of [ALICE, BOB]) { await m.exchange(k, k === ALICE ? 'alice-a' : 'bob-a', 'reg', 0); await m.approve(k); }
  await m.exchange(BOB, 'bob-a', 'start', 1000);
  await m.exchange(ALICE, 'alice-a', 'm1', 2000);
  assert.deepEqual(texts(await m.exchange(BOB, 'bob-a', 'r1', 10_000)), ['alice-a: m1']);
  assert.deepEqual(texts(await m.exchange(BOB, 'bob-a', 'r2', 25_000)), ['alice-a: m1'], 'retry inside window');
  assert.deepEqual(texts(await m.exchange(BOB, 'bob-a', 'r3', 41_000)), [], 'acked after window from FIRST hand-out');
});

await test('at most 20 per call, rest on the next call; newlines become spaces only on render', async (t) => {
  const m = await fresh(t);
  for (const k of [ALICE, BOB]) { await m.exchange(k, k === ALICE ? 'alice-a' : 'bob-a', 'reg', 0); await m.approve(k); }
  await m.exchange(BOB, 'bob-a', 'start', 1000);
  for (let i = 1; i <= 25; i++) await m.exchange(ALICE, 'alice-a', `n${i}`, 1000 + i * 31_000);
  const first = await m.exchange(BOB, 'bob-a', 'a', 2_000_000);
  assert.equal(first.messages.length, 20);
  assert.equal(first.messages[0].text, 'n1');
  const second = await m.exchange(BOB, 'bob-a', 'b', 2_100_000);
  assert.deepEqual(second.messages.map((x) => x.text), ['n21', 'n22', 'n23', 'n24', 'n25']);
});

await test('pending keys are capped at 20; known keys are never refused', async (t) => {
  const m = await fresh(t);
  for (let i = 0; i < 20; i++) await m.exchange(keyId('k' + String(i).padStart(39, '0')), 'x-a', 'reg', i);
  const over = await m.exchange(keyId('z'.repeat(40)), 'x-a', 'reg', 100);
  assert.deepEqual(over, { status: 'pending', messages: [] }, 'over the cap: pending, not stored');
  assert.equal((await m.one('SELECT count(*)::int AS n FROM mailbox.keys')).n, 20);
  assert.equal((await m.exchange(keyId('k' + '0'.repeat(39)), 'x-a', 'known', 200)).status, 'pending');
});

await test('trusted (MCP) identity is approved at first contact', async (t) => {
  const m = await fresh(t);
  await m.exchange(ALICE, 'alice-a', 'reg', 0);
  await m.approve(ALICE);
  await m.exchange(ALICE, 'alice-a', 'from curl', 1000);
  const r = await m.exchange('user:u1', 'alice-mcp', 'from mcp', 2000, true);
  assert.deepEqual(r, { status: 'approved', messages: [] }, 'new session starts at head');
  assert.deepEqual(texts(await m.exchange(ALICE, 'alice-a', 'x', 40_000)), ['alice-mcp: from mcp']);
  assert.equal((await m.exchange('user:u1', 'bob-z', 'spoof', 80_000, true)).status, 'wrong-owner');
});

const history = {
  source: 'old-mailbox',
  keys: [
    { id: ALICE, owner: 'alice', status: 'approved', created_at_ms: T0 - 9000, approved_at_ms: T0 - 8000, last_seen_ms: T0 - 100 },
    { id: BOB, owner: 'bob', status: 'revoked', created_at_ms: T0 - 9000, approved_at_ms: T0 - 8000, last_seen_ms: T0 - 100 }
  ],
  cursors: [
    { key_id: ALICE, agent: 'alice-a', last_id: 41, pending_id: 42, pending_at_ms: T0 - 500, last_seen_ms: T0 - 100 }
  ],
  messages: [
    { id: 41, key_id: BOB, agent: 'bob-a', text: 'old one', created_at_ms: T0 - 7000 },
    { id: 42, key_id: ALICE, agent: 'alice-b', text: 'old two', created_at_ms: T0 - 6000 }
  ]
};

await test('history import keeps ids, opens the mailbox, continues numbering and is idempotent', async (t) => {
  const m = await fresh(t, { legacy: 'old-mailbox' });
  assert.deepEqual(await m.importHistory(history), { keys: 2, cursors: 1, messages: 2, max_id: 42 });
  const snap = async () => ({
    keys: await m.sql('SELECT * FROM mailbox.keys ORDER BY id'),
    cursors: await m.sql('SELECT * FROM mailbox.cursors ORDER BY key_id, agent'),
    messages: await m.sql('SELECT * FROM mailbox.messages ORDER BY id'),
    imports: await m.sql('SELECT source, keys, cursors, messages, max_id FROM mailbox.imports')
  });
  const before = await snap();
  assert.deepEqual(await m.importHistory(history), { keys: 2, cursors: 1, messages: 2, max_id: 42 }, 'repeat is a no-op');
  assert.deepEqual(await snap(), before);
  assert.equal(before.messages[0].created_at.getTime(), T0 - 7000, 'milliseconds are exact');
  // The mailbox is open, numbering continues after the carried-over messages.
  const r = await m.exchange(ALICE, 'alice-a', 'new', 60_000);
  assert.equal(r.status, 'approved');
  assert.equal((await m.one('SELECT max(id)::int AS id FROM mailbox.messages')).id, 43);
  assert.equal((await m.exchange(BOB, 'bob-a', 'x', 61_000)).status, 'revoked');
});

await test('import rejects drift, raw keys and a second source without changing anything', async (t) => {
  const m = await fresh(t, { legacy: 'old-mailbox' });
  await m.importHistory(history);
  const count = async () => (await m.one('SELECT count(*)::int AS n FROM mailbox.messages')).n;
  const drift = structuredClone(history);
  drift.messages[1].text = 'changed';
  await assert.rejects(m.importHistory(drift), /differs/);
  const raw = structuredClone(history);
  raw.keys[0].id = 'e'.repeat(40);
  await assert.rejects(m.importHistory(raw), /key id/);
  await assert.rejects(m.importHistory({ ...history, source: 'other' }), /unsupported mailbox source: other \(expected: old-mailbox\)/);
  assert.equal(await count(), 2);
  assert.deepEqual(await m.sql('SELECT source FROM mailbox.imports'), [{ source: 'old-mailbox' }]);
});

await test('the source name is the one the installation names, not a fixed one', async (t) => {
  const m = await fresh(t, { legacy: 'second-source' });
  await assert.rejects(m.importHistory(history), /expected: second-source/);
  assert.equal((await m.exchange(ALICE, 'alice-a', 'hi', 0)).status, 'moving');
  assert.deepEqual(await m.importHistory({ ...history, source: 'second-source' }), { keys: 2, cursors: 1, messages: 2, max_id: 42 });
  assert.deepEqual(await m.sql('SELECT source, messages FROM mailbox.imports'), [{ source: 'second-source', messages: 2 }]);
  assert.equal((await m.exchange(ALICE, 'alice-a', 'hi', 60_000)).status, 'approved');
});

await test('import is refused once the mailbox has its own messages beyond the import', async (t) => {
  const m = await fresh(t, { legacy: 'old-mailbox' });
  await m.importHistory(history);
  await m.exchange(ALICE, 'alice-a', 'new', 60_000);
  const more = structuredClone(history);
  more.messages.push({ id: 43, key_id: ALICE, agent: 'alice-a', text: 'late old', created_at_ms: T0 - 10 });
  await assert.rejects(m.importHistory(more), /differs/);
});

const literals = (m, text) =>
  m.one(
    `SELECT (SELECT count(*)::int FROM pg_proc WHERE strpos(prosrc, $1) > 0) AS procs,
            (SELECT count(*)::int FROM pg_constraint WHERE strpos(pg_get_constraintdef(oid), $1) > 0) AS constraints`,
    [text]
  );

await test('an installation that imported before 0029 keeps its source and an open mailbox', async (t) => {
  const m = await fresh(t, { upTo: '0029' });
  await m.importHistory({ ...history, source: SOURCE_0008 });
  assert.equal((await m.exchange(ALICE, 'alice-a', 'before', 60_000)).status, 'approved');
  const counts = () =>
    m.one(`SELECT (SELECT count(*)::int FROM mailbox.messages) AS messages, (SELECT count(*)::int FROM mailbox.keys) AS keys,
      (SELECT count(*)::int FROM mailbox.cursors) AS cursors, (SELECT count(*)::int FROM mailbox.imports) AS imports`);
  const before = await counts();
  await m.migrate();
  assert.equal((await m.one('SELECT mailbox_legacy_import FROM public.instance')).mailbox_legacy_import, SOURCE_0008);
  assert.deepEqual(await counts(), before);
  assert.deepEqual(await m.sql('SELECT source FROM mailbox.imports'), [{ source: SOURCE_0008 }]);
  assert.equal((await m.exchange(ALICE, 'alice-a', 'after', 120_000)).status, 'approved');
  assert.equal((await m.exchange(BOB, 'bob-a', 'x', 121_000)).status, 'revoked');
  // The repeat of the same import is still a no-op; another source is refused.
  assert.deepEqual(await m.importHistory({ ...history, source: SOURCE_0008 }), { keys: 2, cursors: 1, messages: 2, max_id: 42 });
  await assert.rejects(m.importHistory({ ...history, source: 'other' }), /source/);
  assert.deepEqual(await literals(m, SOURCE_0008), { procs: 0, constraints: 0 });
});

await test('the chain leaves no legacy source literal in functions and constraints; a source is never empty', async (t) => {
  const m = await fresh(t);
  assert.deepEqual(await literals(m, SOURCE_0008), { procs: 0, constraints: 0 });
  await assert.rejects(m.sql(`UPDATE public.instance SET mailbox_legacy_import = '' WHERE id`), /instance_mailbox_legacy_import_check/);
  await assert.rejects(m.sql(`INSERT INTO mailbox.imports VALUES ('', now(), 0, 0, 0, 0)`), /imports_source_check/);
});
