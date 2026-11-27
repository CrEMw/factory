// Step 2.4 of the factory plan: the account from public.instance instead of a literal (migration 0025) on an isolated
// PostgreSQL: PGlite in memory, no network. A fresh database with account demo writes demo rows in all tables with account,
// the plan.mark triggers see them; a ticket without a project is rejected, and no code path writes like that. What 0025
// changed for the account the chain wrote before it: the tests of that install.
import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import test from 'node:test';
import { hasChain, migrationFiles, migrationSql } from './hub-migrations.mjs';

const { PGlite } = await import(new URL('../.tools/node_modules/@electric-sql/pglite/dist/index.js', import.meta.url));
const root = new URL('../', import.meta.url);
const read = (path) => readFile(new URL(path, root), 'utf8');
const code = (c) => (e) => e.code === c;

// Like Job hub-db-migrate: all migrations, then seeding the instance row with the account from env.
async function fresh(t, account) {
  const db = new PGlite();
  t.after(() => db.close());
  await db.exec(`CREATE ROLE app; CREATE ROLE hub; CREATE ROLE hub_web; GRANT ALL ON SCHEMA public TO app;
    DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO app', current_database()); END $$;
    SET ROLE app;`);
  for (const file of await migrationFiles()) await db.exec('BEGIN;\n' + (await migrationSql(file)) + '\nCOMMIT;');
  const sql = (q, p = []) => db.query(q, p).then((r) => r.rows);
  const one = (q, p = []) => sql(q, p).then((r) => r[0]);
  const as = (actor, kind, fresh = '') =>
    sql(`SELECT set_config('hub.actor', $1, false), set_config('hub.actor_kind', $2, false), set_config('hub.fresh', $3, false)`,
      [actor, kind, fresh]);
  if (account) {
    await sql(`SELECT public.seed_instance($1, 'UTC', 20, NULL, ARRAY['docs/superpowers'])`, [account]);
  }
  return { db, sql, one, as };
}

// All tables with an account column, except the installation row itself.
const accountTables = (sql) =>
  sql(`SELECT c.table_schema || '.' || c.table_name AS t, c.column_default AS d
       FROM information_schema.columns c
       JOIN information_schema.tables x ON x.table_schema = c.table_schema AND x.table_name = c.table_name
       WHERE c.column_name = 'account' AND x.table_type = 'BASE TABLE'
         AND c.table_schema IN ('public', 'graph', 'plan', 'mailbox') AND c.table_name <> 'instance'
       ORDER BY 1`);

const countBy = async (sql, tables, account) => {
  const out = {};
  for (const { t } of tables) out[t] = (await sql(`SELECT count(*)::int AS n FROM ${t} WHERE account = $1`, [account]))[0].n;
  return out;
};

await test('every account column defaults to public.instance_account()', async (t) => {
  const { sql } = await fresh(t);
  const tables = await accountTables(sql);
  assert.equal(tables.length, 19, tables.map((x) => x.t).join(' '));
  for (const { t: name, d } of tables) assert.match(d ?? '', /^(public\.)?instance_account\(\)$/, name);
  assert.deepEqual(
    await sql(`SELECT column_default AS d FROM information_schema.columns
               WHERE table_schema = 'public' AND table_name = 'tickets' AND column_name = 'project'`),
    [{ d: null }],
    'tickets.project has no default');
});

await test('without the instance row an insert relying on the default raises HUBIN, not a silent account', async (t) => {
  const { sql, as } = await fresh(t);
  await as('seed:test', 'seed');
  await assert.rejects(sql(`INSERT INTO tickets(project, code, summary) VALUES ('p', 'a', 'a')`), code('HUBIN'));
});

await test('a ticket without project is refused', async (t) => {
  const { sql, as } = await fresh(t, 'demo');
  await as('seed:test', 'seed');
  await assert.rejects(sql(`INSERT INTO tickets(code, summary) VALUES ('np', 'no project')`), code('23502'));
  assert.deepEqual(await sql(`SELECT count(*)::int AS n FROM tickets WHERE code = 'np'`), [{ n: 0 }]);
});

await test('a fresh base with account demo writes demo rows in every table and plan.mark sees them', async (t) => {
  const { db, sql, one, as } = await fresh(t);
  const tables = await accountTables(sql);
  // Rows of other accounts (a chain may have written its own before 0025) must not grow.
  const others = async () => {
    const out = {};
    for (const { t } of tables) out[t] = (await sql(`SELECT count(*)::int AS n FROM ${t} WHERE account <> 'demo'`))[0].n;
    return out;
  };
  const before = await others();
  await sql(`SELECT public.seed_instance('demo', 'UTC', 20, NULL, ARRAY['docs/superpowers'])`);
  await sql(`SELECT public.import_document_repositories('[{"repo": "acme/apps", "project": "apps", "caller": "docs-apps"}]')`);

  // Migration reference data (node kinds, edge shapes, default points) is there for the installation account at seeding.
  for (const [table, live] of [['graph.node_kind', ''], ['graph.rel_shape', ''], ['plan.type_default_estimate', ' AND valid_to IS NULL']]) {
    const [{ n: d }] = await sql(`SELECT count(*)::int AS n FROM ${table} WHERE account = 'demo'${live}`);
    assert.ok(d > 0, `${table}: demo ${d}`);
  }
  // A repeated seeding copies nothing a second time.
  await sql(`SELECT public.seed_instance('demo', 'UTC', 20, NULL, ARRAY['docs/superpowers'])`);
  assert.equal((await one(`SELECT count(*)::int AS n FROM graph.node_kind WHERE account = 'demo'`)).n, 6);

  await as('seed:test', 'seed');
  await sql(`INSERT INTO tickets(project, code, summary, tags) VALUES
    ('demo-proj', 'r', 'root', '{}'), ('demo-proj', 'a', 'a', '{w}'), ('demo-proj', 'b', 'b', '{}'), ('apps', 'd', 'd', '{}')`);
  const seq = async (c) => (await one(`SELECT seq FROM tickets WHERE code = $1`, [c])).seq;
  const [r, a, b] = [await seq('r'), await seq('a'), await seq('b')];
  await sql(`INSERT INTO documents(repo, path, role, title, body, sha256, created_at, synced_at)
    VALUES ('acme/apps', 'docs/superpowers/plans/2030-01-25-demo.md', 'plan', 'Demo', 'x', 'h', now(), now())`);
  assert.deepEqual(await sql(`SELECT graph.import_people('[{"handle": "alice", "name": "Alice"}]') AS r`),
    [{ r: { created: 1, updated: 0, unchanged: 0 } }]);
  assert.deepEqual(
    await sql(`SELECT graph.import_components('[{"slug": "widgets", "title": "W", "project": "demo-proj", "tags": ["w"]}]') AS r`),
    [{ r: { created: 1, updated: 0, unchanged: 0, projects: 1, tickets: 1 } }]);
  const links = await one(`SELECT graph.import_links('[{"src": "a", "rel": "blocks", "dst": "b"}]') AS r`);
  assert.equal(links.r.created, 1, JSON.stringify(links.r));
  const scopes = await one(`SELECT plan.import_scopes('[{"root": "r", "children": ["a", "b"]}]') AS r`);
  assert.equal(scopes.r.created, 1, JSON.stringify(scopes.r));
  assert.equal(scopes.r.links.r.created, 2, JSON.stringify(scopes.r));
  await sql(`INSERT INTO plan.ticket_estimate(ticket_seq, effort_sp) VALUES ($1, 3)`, [a]);
  await sql(`INSERT INTO plan.ticket_wsjf(ticket_seq, business_value, time_criticality, risk_opportunity) VALUES ($1, 1, 2, 3)`, [a]);
  await sql(`INSERT INTO plan.points_to_hd(effort_sp, aggressive_hd) VALUES (3, 2)`);

  // Document: the repo project from the registry (project node apps, also of ticket d) and retirement.
  assert.deepEqual(await one(`SELECT public.doc_reconcile('acme/apps', 'docs/superpowers/plans/2030-01-25-demo.md', 'rev') AS r`),
    { r: [1, 0, 0] });

  await as('user:u1', 'human', 'on');
  await sql(`INSERT INTO graph.person_login(user_id, handle) VALUES ('u1', 'alice')`);
  await sql(`INSERT INTO mailbox.keys(id, owner, status, created_at, last_seen_at) VALUES ('user:u2', 'alice', 'pending', now(), now())`);
  assert.equal((await one(`SELECT (mailbox.review_key('user:u2', 'approved')).status AS s`)).s, 'approved');
  await sql(`INSERT INTO plan.baseline(root_seq, defaulted_pct, algo_version, result, accepted_defaults) VALUES ($1, 0, 1, '{}', true)`, [r]);
  const baseline = (await one(`SELECT id FROM plan.baseline_live WHERE account = 'demo' AND root_seq = $1`, [r])).id;
  await as('hub-plan', 'service:plan');
  await sql(`INSERT INTO plan.current(root_seq, input_hash, algo_version, today, baseline_id, result)
    VALUES ($1, sha256('x'), 1, current_date, $2, '{}')`, [r, baseline]);
  await sql(`INSERT INTO plan.fever_point(root_seq, day, buffer_key, baseline_id, kind, chain_complete_pct, consumed_pct)
    VALUES ($1, current_date, 'pb', $2, 'project', 0, 0)`, [r, baseline]);
  await as('seed:test', 'seed');
  assert.deepEqual(await one(`SELECT public.doc_retire('acme/apps', 'docs/superpowers/plans/2030-01-25-demo.md') AS r`), { r: [0, 1] });

  // Every table got demo rows, not a single new row of another account.
  const demo = await countBy(sql, tables, 'demo');
  for (const { t: name } of tables) assert.ok(demo[name] > 0, `${name}: no demo rows`);
  assert.deepEqual(await others(), before, 'no new rows of other accounts');

  // plan.mark: a ticket edit, an edge, a point reference table mark the demo scope in the queue.
  const marked = async () => (await one(`SELECT marked FROM plan.dirty WHERE account = 'demo' AND root_seq = $1`, [r])).marked;
  let m = await marked();
  await as('seed:test', 'seed');
  await sql(`UPDATE tickets SET status = 'doing' WHERE seq = $1`, [b]);
  assert.equal(await marked(), m + 1, 'ticket_dirty');
  m += 1;
  await sql(`INSERT INTO plan.type_default_estimate(tag, effort_sp) VALUES ('bug', 3)`);
  assert.equal(await marked(), m + 1, 'all_dirty');
  m += 1;
  await db.exec('BEGIN');
  await sql(`SELECT plan.mark_all_now(public.instance_account())`);
  await db.exec('COMMIT');
  assert.equal(await marked(), m + 1, 'mark_all_now with instance_account()');
  assert.deepEqual(await sql(`SELECT DISTINCT account FROM plan.dirty`), [{ account: 'demo' }]);
});

// A ticket without a project is rejected by the database; here we check that no code path writes a ticket without project.
const walk = async (dir, exts) => {
  const out = [];
  for (const e of await readdir(new URL(dir, root), { withFileTypes: true })) {
    if (e.name === 'node_modules' || e.name === 'target' || e.name.startsWith('.')) continue;
    const p = dir + e.name;
    if (e.isDirectory()) out.push(...(await walk(p + '/', exts)));
    else if (exts.some((x) => e.name.endsWith(x))) out.push(p);
  }
  return out;
};

await test('every INSERT into tickets in hub, hub-web and migrations names project', async () => {
  const files = [
    ...(await walk('services/hub/src/', ['.rs'])),
    ...(hasChain ? await walk('services/hub/migrations/', ['.sql']) : []),
    ...(await walk('services/hub/baseline/', ['.sql'])),
    ...(await walk('hub-web/src/', ['.ts', '.js', '.svelte'])),
  ];
  let seen = 0;
  for (const f of files) {
    const text = await read(f);
    for (const m of text.matchAll(/insert\s+into\s+(?:public\.)?tickets\b\s*(\([^)]*\))?/gi)) {
      seen += 1;
      assert.ok(m[1] && /\bproject\b/.test(m[1]), `${f}: ${m[0].replace(/\s+/g, ' ').slice(0, 120)}`);
    }
  }
  assert.ok(seen >= 3, `only ${seen} inserts found: the pattern no longer matches the code`);
});
