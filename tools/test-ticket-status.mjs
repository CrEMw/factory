// Ticket statuses in the hub schema (migration 0009) on an isolated PostgreSQL: PGlite in memory, no network.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { migrationFiles, migrationSql } from './hub-migrations.mjs';

const { PGlite } = await import(new URL('../.tools/node_modules/@electric-sql/pglite/dist/index.js', import.meta.url));
const root = new URL('../', import.meta.url);
const read = (path) => readFile(new URL(path, root), 'utf8');

await test('cancelled is a status: set, filtered out of active, reversible; unknown statuses refused', async (t) => {
  const db = new PGlite();
  t.after(() => db.close());
  await db.exec(`CREATE ROLE app; CREATE ROLE hub; CREATE ROLE hub_web; GRANT ALL ON SCHEMA public TO app;
    DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO app', current_database()); END $$;
    SET ROLE app;`);
  const migrations = await migrationFiles();
  for (const file of migrations) await db.exec('BEGIN;\n' + (await migrationSql(file)) + '\nCOMMIT;');
  // Like Job hub-db-migrate: the instance row (0023) after the migrations; without it the account default (0025) gives HUBIN.
  await db.exec("SELECT public.seed_instance('demo', 'Europe/Athens', 20, NULL, ARRAY['docs/superpowers'])");
  const sql = (q, p = []) => db.query(q, p).then((r) => r.rows);
  await sql(`SELECT set_config('hub.actor', 'user:1', false), set_config('hub.actor_kind', 'human', false)`);

  await sql(`INSERT INTO tickets(project, code, summary, status) VALUES ('p', 'a1', 'duplicate', 'open'), ('p', 'a2', 'alive', 'open')`);
  await sql(`UPDATE tickets SET status = 'cancelled' WHERE code = 'a1'`);
  const active = await sql(`SELECT code FROM tickets WHERE status = any($1) ORDER BY code`, [['open', 'doing']]);
  assert.deepEqual(active.map((r) => r.code), ['a2'], 'cancelled leaves the active list');
  assert.equal((await sql(`SELECT count(*)::int AS n FROM tickets`))[0].n, 2, 'nothing is deleted');
  await sql(`UPDATE tickets SET status = 'open' WHERE code = 'a1'`);
  assert.equal((await sql(`SELECT status FROM tickets WHERE code = 'a1'`))[0].status, 'open', 'reversible');
  for (const bad of ['closed', 'archived', 'Done']) {
    await assert.rejects(sql(`UPDATE tickets SET status = $1 WHERE code = 'a2'`, [bad]), /tickets_status/, bad);
  }
});
