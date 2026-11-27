// Step 2.3 of the factory plan: the installation row public.instance (migration 0023) on an isolated PostgreSQL: PGlite
// in memory, no network. Seeding goes through public.seed_instance, which `hub migrate` calls too (services/hub/src/migrations.rs).
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { migrationFiles, migrationSql } from './hub-migrations.mjs';

const { PGlite } = await import(new URL('../.tools/node_modules/@electric-sql/pglite/dist/index.js', import.meta.url));
const root = new URL('../', import.meta.url);
const read = (path) => readFile(new URL(path, root), 'utf8');
const code = (c) => (e) => e.code === c;

async function fresh(t) {
  const db = new PGlite();
  t.after(() => db.close());
  await db.exec(`CREATE ROLE app; CREATE ROLE hub; CREATE ROLE hub_web; GRANT ALL ON SCHEMA public TO app;
    DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO app', current_database()); END $$;
    SET ROLE app;`);
  const migrations = await migrationFiles();
  for (const file of migrations) await db.exec('BEGIN;\n' + (await migrationSql(file)) + '\nCOMMIT;');
  const sql = (q, p = []) => db.query(q, p).then((r) => r.rows);
  const seed = (account, timezone, limit = 20, ttl = null, roots = ['docs/superpowers']) =>
    sql('SELECT public.seed_instance($1, $2, $3, $4::interval, $5) AS old', [account, timezone, limit, ttl, roots]).then((r) => r[0].old);
  const row = () => sql('SELECT account, timezone, mailbox_pending_limit AS l, mailbox_pending_ttl::text AS ttl, document_roots AS roots FROM public.instance');
  // A query as the service role; the owner role app comes back whatever the outcome.
  const as = async (role, q) => {
    await db.exec(`RESET ROLE; SET ROLE ${role}`);
    try {
      return await sql(q);
    } finally {
      await db.exec('RESET ROLE; SET ROLE app');
    }
  };
  return { sql, seed, row, as };
}

await test('no row: instance_account() and instance_timezone() raise HUBIN instead of NULL', async (t) => {
  const { sql } = await fresh(t);
  await assert.rejects(sql('SELECT public.instance_account()'), code('HUBIN'));
  await assert.rejects(sql('SELECT public.instance_timezone()'), code('HUBIN'));
});

await test('seed inserts once, repeats idempotently and updates only mutable fields', async (t) => {
  const { sql, seed, row } = await fresh(t);
  assert.equal(await seed('demo', 'Europe/Athens'), null, 'new row: no previous timezone');
  const first = await row();
  assert.deepEqual(first, [{ account: 'demo', timezone: 'Europe/Athens', l: 20, ttl: null, roots: ['docs/superpowers'] }]);
  assert.equal(await seed('demo', 'Europe/Athens'), 'Europe/Athens', 'repeat returns the same timezone');
  assert.deepEqual(await row(), first, 'repeat changes nothing');
  assert.deepEqual(await sql('SELECT public.instance_account() AS a, public.instance_timezone() AS z'), [{ a: 'demo', z: 'Europe/Athens' }]);

  assert.equal(await seed('demo', 'UTC', 5, '7 days', ['docs', 'notes']), 'Europe/Athens', 'old timezone for the WARN');
  assert.deepEqual(await row(), [{ account: 'demo', timezone: 'UTC', l: 5, ttl: '7 days', roots: ['docs', 'notes'] }]);
});

await test('account is write-once: another account is refused and the row stays as it was', async (t) => {
  const { sql, seed, row } = await fresh(t);
  await seed('demo', 'Europe/Athens');
  const before = await row();
  await assert.rejects(seed('acme', 'UTC', 7), code('HUBAC'));
  assert.deepEqual(await row(), before, 'nothing written');
  await assert.rejects(sql(`UPDATE public.instance SET account = 'acme'`), code('HUBAC'), 'direct UPDATE by the owner too');
  assert.deepEqual(await row(), before);
});

await test('singleton, account format, time zone and limits are checked', async (t) => {
  const { sql, seed, row } = await fresh(t);
  for (const bad of ['Demo', '1demo', 'demo_hub', '', 'a'.repeat(41), 'de mo']) {
    await assert.rejects(seed(bad, 'UTC'), code('23514'), JSON.stringify(bad));
  }
  await assert.rejects(seed('demo', 'Mars/Olympus'), code('22023'), 'unknown time zone on the first seed');
  // `now() at time zone` accepts abbreviations and POSIX offsets, but they are not IANA names: UTC+3 in POSIX means UTC-3.
  for (const bad of ['MSK', 'UTC+3', '+03:00', 'utc']) {
    await assert.rejects(seed('demo', bad), code('22023'), bad);
  }
  assert.deepEqual(await row(), [], 'refused seeds wrote nothing');
  await seed('demo', 'Europe/Athens');
  await assert.rejects(sql(`INSERT INTO public.instance(account, timezone) VALUES ('demo', 'UTC')`), code('23505'), 'second row');
  await assert.rejects(sql(`INSERT INTO public.instance(id, account, timezone) VALUES (false, 'demo', 'UTC')`), code('23514'), 'id false');
  await assert.rejects(seed('demo', 'Mars/Olympus'), code('22023'), 'unknown time zone');
  await assert.rejects(seed('demo', 'Europe/Athens', 0), code('23514'), 'limit must be positive');
  await assert.rejects(seed('demo', 'Europe/Athens', 20, '-1 day'), code('23514'), 'ttl must be positive');
  assert.equal((await row())[0].timezone, 'Europe/Athens', 'refused seeds left the row alone');
});

await test('roles: hub and hub_web read, only the migration owner writes and seeds', async (t) => {
  const { seed, as } = await fresh(t);
  await seed('demo', 'Europe/Athens');
  for (const role of ['hub', 'hub_web']) {
    assert.deepEqual(await as(role, 'SELECT public.instance_account() AS a, public.instance_timezone() AS z'), [{ a: 'demo', z: 'Europe/Athens' }], role);
    assert.deepEqual(await as(role, 'SELECT account FROM public.instance'), [{ account: 'demo' }], role);
    for (const q of [
      `UPDATE public.instance SET timezone = 'UTC'`,
      'DELETE FROM public.instance',
      `INSERT INTO public.instance(account, timezone) VALUES ('x', 'UTC')`,
      `SELECT public.seed_instance('demo', 'UTC', 20, null, '{}')`
    ]) {
      await assert.rejects(as(role, q), code('42501'), `${role}: ${q}`);
    }
  }
});
