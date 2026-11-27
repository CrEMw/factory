// Documents of all repos in the graph (migration 0017): parsing of the ticket and closes lines, explicit links to
// documents, the topic rule, retirement and return. PGlite in memory; whole snapshots: test-repository-documents.mjs.
// The 0017 backfill of the documents an install had synced before it: the tests of that install.
import assert from 'node:assert/strict';
import test from 'node:test';
import { migrationFiles, migrationSql } from './hub-migrations.mjs';

const { PGlite } = await import(new URL('../.tools/node_modules/@electric-sql/pglite/dist/index.js', import.meta.url));
const code = (c) => (e) => e.code === c;
const S = 'docs/superpowers/';
// The document repository registry, as the seed of the hub start writes it.
const REGISTRY = ['apps', 'gadgets', 'tools', 'widgets'].map((n) => ({ repo: 'acme/' + n, project: n, caller: 'docs-' + n }));

async function fresh(t) {
  const db = new PGlite();
  t.after(() => db.close());
  await db.exec(`CREATE ROLE app; CREATE ROLE hub; CREATE ROLE hub_web; GRANT ALL ON SCHEMA public TO app;
    DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO app', current_database()); END $$;
    SET ROLE app;`);
  const sql = (q, p = []) => db.query(q, p).then((r) => r.rows);
  for (const file of await migrationFiles()) {
    await db.exec('BEGIN;\n' + (await migrationSql(file)) + '\nCOMMIT;');
  }
  // Like Job hub-db-migrate: the instance row (0023) after the migrations; without it the account default (0025) gives HUBIN.
  // Link labels of an install in two languages (Spanish and English): the fixtures use both.
  await db.query("SELECT public.seed_instance('demo', 'Europe/Athens', 20, NULL, ARRAY['docs/superpowers'], $1::jsonb)",
    [JSON.stringify({ describes: ['Tique', 'Tiques', 'Ticket', 'Tickets'], closes: ['Cierra', 'Closes'], and: ['y', 'and'] })]);
  await db.query('SELECT public.import_document_repositories($1::jsonb)', [JSON.stringify(REGISTRY)]);
  // Tickets with made-up 4-character codes of the SQL code alphabet; legacy_id: 16 hex of the old system.
  // An actor only for this insert (0018 requires it for tickets), the session stays without an actor.
  await db.exec(`BEGIN; SELECT set_config('hub.actor', 'seed:test', true), set_config('hub.actor_kind', 'seed', true);
    INSERT INTO tickets(project, code, summary, legacy_id) VALUES
    ('widgets', 'k154', 'graph', NULL), ('widgets', 'k109', 'umbrella', NULL), ('apps', 'kt05', 'old', 'b7a6af9850b102c7'),
    ('widgets', 'k15g', 'stage', NULL), ('widgets', 'k159', 'stage', NULL) ON CONFLICT DO NOTHING; COMMIT;`);
  const as = (actor, kind) => sql(`SELECT set_config('hub.actor', $1, false), set_config('hub.actor_kind', $2, false)`, [actor, kind]);
  // Revisions in order, history grows: each next one is a descendant of the previous ones.
  const history = {};
  const sync = async (repo, docs) => {
    const caller = 'docs-' + repo.split('/')[1];
    await as(caller, 'service:' + caller);
    const h = (history[repo] ??= []);
    const revision = (h.length + 1).toString(16).padStart(40, '0');
    h.unshift(revision);
    const documents = Object.entries(docs).map(([p, body]) => ({ path: S + p, body }));
    return (await sql('SELECT public.sync_repository_documents($1::jsonb) AS r', [JSON.stringify({ repo, revision, ancestors: h, documents })]))[0].r;
  };
  const links = (repo, p) => sql(`SELECT t.code, l.rel FROM ticket_documents l JOIN tickets t ON t.seq = l.ticket_seq
    WHERE l.repo = $1 AND l.path = $2 AND l.valid_to IS NULL ORDER BY l.rel, t.code`, [repo, S + p]).then((r) => r.map((x) => `${x.rel}:${x.code}`));
  // Live edges from a document: rel, state, rule and target (repo:path or project key).
  const edges = (repo, p, all = false) => sql(`SELECT e.id, e.rel, e.state, e.evidence->>'rule' AS rule, n.key, e.close_reason, e.created_by
    FROM graph.edge e JOIN graph.document s ON s.node_id = e.src JOIN graph.node n ON n.id = e.dst
    WHERE s.repo = $1 AND s.path = $2 AND ($3 OR e.valid_to IS NULL) ORDER BY e.id`, [repo, S + p, all]);
  const docEdges = async (repo, p) => (await edges(repo, p)).filter((e) => e.rel !== 'in_project')
    .map((e) => `${e.rel}:${e.state}:${e.key.replace(/:docs\/superpowers\//, ':')}`);
  return { db, sql, as, sync, links, edges, docEdges };
}

await test('header lines give describes and closes; code blocks and unknown tokens do not', async (t) => {
  const { sync, links, sql } = await fresh(t);
  const r = await sync('acme/widgets', {
    'specs/a.md': '# A\n\nTique: `k154` (umbrella), `k109`\n',
    'specs/b.md': '**Ticket:** `b7a6af9850b102c7`\n# B\n',
    'specs/c.md': '# C\nTique `kt05`\n',
    'specs/d.md': '# D\n```\nTique: `k154`\n```\n',
    'specs/e.md': '# E\n\n    Tique: `k154`\n',
    'plans/f.md': '# F\nTique: `k154`, ver `widgets`\n\n- **Cierra:** `k15g`; `k159`\n```\nCierra: `k109`\n```\n',
    'plans/g.md': '---\ntitle: x\n---\n# G\nTickets: `KT05`\n'
  });
  assert.deepEqual(r.problems, []);
  assert.deepEqual(await links('acme/widgets', 'specs/a.md'), ['describes:k109', 'describes:k154']);
  assert.deepEqual(await links('acme/widgets', 'specs/b.md'), ['describes:kt05'], 'legacy id');
  assert.deepEqual(await links('acme/widgets', 'specs/c.md'), ['describes:kt05'], 'no colon');
  assert.deepEqual(await links('acme/widgets', 'specs/d.md'), [], 'fenced code');
  assert.deepEqual(await links('acme/widgets', 'specs/e.md'), [], 'indented code');
  assert.deepEqual(await links('acme/widgets', 'plans/f.md'), ['closes:k159', 'closes:k15g', 'describes:k154']);
  assert.deepEqual(await links('acme/widgets', 'plans/g.md'), ['describes:kt05'], 'front matter skipped, code lowercased');
  // Unknown code: the document does not change, the problem is in the response; more than 20 codes: likewise.
  const alphabet = '0123456789abcdefghjkmnpqrstvwxyz';
  const many = Array.from({ length: 21 }, (_, i) => '`w' + alphabet[i] + '`').join(', ');
  await sql(`INSERT INTO tickets(project, code, summary) SELECT 'widgets', 'w' || x, 'many' FROM unnest($1::text[]) AS x`,
    [[...alphabet.slice(0, 21)]]);
  const bad = await sync('acme/widgets', {
    'specs/a.md': '# A\nTique: `k154`, `qqq`\n', 'specs/b.md': '# B\nTique: ' + many + '\n'
  });
  assert.deepEqual(bad.problems.map((p) => `${p.path.slice(S.length)}:${p.kind}`).sort(),
    ['specs/a.md:unknown_code', 'specs/b.md:too_many_codes']);
  assert.deepEqual(await links('acme/widgets', 'specs/a.md'), ['describes:k109', 'describes:k154'], 'previous version kept');
});

await test('explicit links: own path, other repo by URL and by directory, self, dangling, code', async (t) => {
  const { sync, docEdges, sql } = await fresh(t);
  const r = await sync('acme/widgets', {
    'intents/hub.md': '# I\n',
    'specs/hub.md': '# S\nSee docs/superpowers/intents/hub.md and itself docs/superpowers/specs/hub.md.\n',
    'plans/hub.md': [
      '# P', 'Spec: [spec](docs/superpowers/specs/hub.md).',
      'Platform: https://github.com/acme/tools/blob/main/docs/superpowers/specs/p.md#section',
      'Platform intent: `tools/docs/superpowers/intents/p.md`.',
      'No such: docs/superpowers/plans/missing.md', '```', 'docs/superpowers/plans/in-code.md', '```', ''
    ].join('\n')
  });
  assert.deepEqual(r.problems, [{ path: S + 'plans/hub.md', kind: 'dangling_link', detail: S + 'plans/missing.md' }]);
  assert.deepEqual(await docEdges('acme/widgets', 'specs/hub.md'), ['specifies:confirmed:acme/widgets:intents/hub.md']);
  assert.deepEqual(await docEdges('acme/widgets', 'plans/hub.md'), ['implements:confirmed:acme/widgets:specs/hub.md'],
    'targets of another repo wait for it; the dangling link is stored, the document is synced');
  assert.deepEqual((await sql(`SELECT target_repo, target_path FROM document_ref WHERE path = $1 AND valid_to IS NULL ORDER BY 1, 2`,
    [S + 'plans/hub.md'])).map((x) => x.target_repo + ':' + x.target_path.slice(S.length)),
    ['acme/tools:intents/p.md', 'acme/tools:specs/p.md', 'acme/widgets:plans/missing.md', 'acme/widgets:specs/hub.md']);
  // A target from another repo is synced later: the edges appear on its sync.
  const p = await sync('acme/tools', { 'specs/p.md': '# PS\n', 'intents/p.md': '# PI\n' });
  assert.deepEqual(p.edges, { opened: 5, closed: 0, proposed: 1 }, 'two in_project, two links into platform, spec p to intent p by topic');
  assert.deepEqual((await docEdges('acme/widgets', 'plans/hub.md')).sort(), [
    'implements:confirmed:acme/tools:intents/p.md', 'implements:confirmed:acme/tools:specs/p.md',
    'implements:confirmed:acme/widgets:specs/hub.md']);
  // A bare path of another repo without a directory: leads to where there is exactly one such document.
  await sync('acme/widgets', {
    'intents/hub.md': '# I\nNeighbours: docs/superpowers/specs/p.md\n', 'specs/hub.md': '# S\n', 'plans/hub.md': '# P\n'
  });
  assert.deepEqual(await docEdges('acme/widgets', 'intents/hub.md'), ['references:confirmed:acme/tools:specs/p.md']);
  // Links removed from the text: the edges into platform are closed; the edge to the own repo's spec stays, the same topic gives it.
  assert.deepEqual(await docEdges('acme/widgets', 'plans/hub.md'), ['implements:confirmed:acme/widgets:specs/hub.md']);
});

await test('review fixes: header run, URL owner, bare paths in any sync order, problem marker, non-git proposals', async (t) => {
  const { sync, links, docEdges, edges, sql, as } = await fresh(t);
  // A "Tickets ..." line without a code right after the word is not a header; the code list stops at the first other word.
  const h = await sync('acme/widgets', {
    'specs/h1.md': '# H1\nTickets are kept in `db` tables.\n',
    'specs/h2.md': '# H2\nTique: `k154`, migracion `0017`\n',
    'specs/h3.md': '# H3\n**Tiques** `k154` y `k109`; ver `qqq`\n'
  });
  assert.deepEqual(h.problems, []);
  assert.deepEqual(await links('acme/widgets', 'specs/h1.md'), []);
  assert.deepEqual(await links('acme/widgets', 'specs/h2.md'), ['describes:k154']);
  assert.deepEqual(await links('acme/widgets', 'specs/h3.md'), ['describes:k109', 'describes:k154']);

  // A URL of another owner is not a link to a registry repo; the registry owner is case-insensitive.
  await sync('acme/tools', {
    'specs/u.md': ['# U', 'https://github.com/obra/superpowers/blob/main/docs/superpowers/specs/x.md',
      'https://github.com/someone/apps/blob/main/docs/superpowers/specs/y.md',
      'https://github.com/apps/widgets/blob/main/docs/superpowers/specs/z.md',
      'https://github.com/ACME/widgets/blob/main/docs/superpowers/specs/h1.md', ''].join('\n')
  });
  assert.deepEqual((await sql(`SELECT target_repo || ':' || target_path AS t FROM document_ref WHERE path = $1 AND valid_to IS NULL`,
    [S + 'specs/u.md'])).map((x) => x.t), ['acme/widgets:' + S + 'specs/h1.md']);

  // A bare path that exists nowhere yet: the edge appears when any repo syncs the target, without editing the referrer.
  const hub = { 'specs/h1.md': '# H1\n', 'plans/b.md': '# B\nSee docs/superpowers/specs/z.md\n' };
  const first = await sync('acme/widgets', hub);
  assert.deepEqual(first.problems.map((p) => p.kind), ['dangling_link']);
  await sync('acme/gadgets', { 'specs/z.md': '# Z\n' });
  assert.deepEqual(await docEdges('acme/widgets', 'plans/b.md'), ['implements:confirmed:acme/gadgets:specs/z.md']);
  // The same path appeared in the own repo: the bare link moves there.
  await sync('acme/widgets', { ...hub, 'specs/z.md': '# own Z\n' });
  assert.deepEqual(await docEdges('acme/widgets', 'plans/b.md'), ['implements:confirmed:acme/widgets:specs/z.md']);

  // A document with a problem is not marked retired and is parsed again with unchanged text.
  const dangling = { 'specs/h1.md': '# H1\n', 'plans/d.md': '# D\nSee docs/superpowers/specs/none.md\n' };
  await sync('acme/widgets', dangling);
  assert.deepEqual(await sql(`SELECT derived_sha256 FROM documents WHERE repo = 'acme/widgets' AND path = $1`, [S + 'plans/d.md']),
    [{ derived_sha256: null }]);
  const again = await sync('acme/widgets', dangling);
  assert.deepEqual(again.problems.map((p) => `${p.path.slice(S.length)}:${p.kind}`), ['plans/d.md:dangling_link']);

  // An agent proposal (not from git) is not closed by the sync, even when an explicit link gives the same edge.
  const node = async (p) => (await sql(`SELECT node_id FROM graph.document WHERE repo = 'acme/widgets' AND path = $1`, [S + p]))[0].node_id;
  await as('agent:x', 'agent');
  await sql(`INSERT INTO graph.edge(src, src_kind, rel, dst, dst_kind, state) VALUES ($1, 'document', 'implements', $2, 'document', 'proposed')`,
    [await node('plans/d.md'), await node('specs/h1.md')]);
  await sync('acme/widgets', { ...dangling, 'plans/d.md': '# D\nSee docs/superpowers/specs/h1.md\n' });
  const all = (await edges('acme/widgets', 'plans/d.md', true)).filter((e) => e.rel === 'implements');
  assert.deepEqual(all.map((e) => `${e.state}:${e.close_reason}`), ['proposed:null']);
});

await test('topic rule proposes one candidate, links supersede, rejected stays rejected', async (t) => {
  const { sync, docEdges, edges, sql, as } = await fresh(t);
  const repo = 'acme/gadgets';
  const docs = {
    'intents/2030-01-01-foo.md': '# I\n',
    'specs/2030-01-02-foo.md': '# S\n',
    'plans/2030-01-03-foo.md': '# P\n',
    'plans/2030-01-01-bar.md': '# early plan\n',
    'specs/2030-01-05-bar.md': '# later spec\n',
    'plans/2030-01-04-baz.md': '# plan without spec\n',
    'intents/2030-01-04-baz/intent.md': '# intent in a directory\n'
  };
  const r = await sync(repo, docs);
  assert.equal(r.edges.proposed, 3);
  assert.deepEqual(await docEdges(repo, 'plans/2030-01-03-foo.md'), [`implements:proposed:${repo}:specs/2030-01-02-foo.md`]);
  assert.deepEqual(await docEdges(repo, 'specs/2030-01-02-foo.md'), [`specifies:proposed:${repo}:intents/2030-01-01-foo.md`]);
  assert.deepEqual(await docEdges(repo, 'plans/2030-01-01-bar.md'), [], 'a spec dated after the plan is no candidate');
  assert.deepEqual(await docEdges(repo, 'plans/2030-01-04-baz.md'), [`implements:proposed:${repo}:intents/2030-01-04-baz/intent.md`],
    'plan to intent when the topic has no spec');

  // A second candidate: the proposal is withdrawn.
  const two = await sync(repo, { ...docs, 'specs/2030-01-02-foo-design.md': '# S2\n' });
  assert.deepEqual(await docEdges(repo, 'plans/2030-01-03-foo.md'), []);
  assert.ok(two.edges.closed >= 1);
  // An explicit link replaces the proposal: proposed is closed as superseded, confirmed is set.
  await sync(repo, docs);
  const plan = { ...docs, 'plans/2030-01-03-foo.md': '# P\nSpec: docs/superpowers/specs/2030-01-02-foo.md\n' };
  await sync(repo, plan);
  const all = await edges(repo, 'plans/2030-01-03-foo.md', true);
  assert.deepEqual(all.filter((e) => e.rel === 'implements').map((e) => `${e.state}:${e.rule}:${e.close_reason}`),
    ['proposed:topic:removed', 'proposed:topic:superseded', 'confirmed:link:null']);
  // A proposal rejected by a person does not come back.
  const [prop] = (await edges(repo, 'specs/2030-01-02-foo.md')).filter((e) => e.state === 'proposed');
  await as('user:alice', 'human');
  await sql(`UPDATE graph.edge SET valid_to = now(), close_reason = 'rejected' WHERE id = $1`, [prop.id]);
  await sync(repo, { ...plan, 'specs/2030-01-02-foo.md': '# S changed\n' });
  assert.deepEqual(await docEdges(repo, 'specs/2030-01-02-foo.md'), []);
  // A proposal confirmed by a person is not closed by the sync, even when there are now two candidates.
  const [baz] = (await edges(repo, 'plans/2030-01-04-baz.md')).filter((e) => e.state === 'proposed');
  await as('user:alice', 'human');
  await sql(`UPDATE graph.edge SET state = 'confirmed' WHERE id = $1`, [baz.id]);
  await sync(repo, { ...plan, 'specs/2030-01-02-foo.md': '# S changed\n', 'intents/2030-01-02-baz.md': '# second intent\n' });
  assert.deepEqual(await docEdges(repo, 'plans/2030-01-04-baz.md'), [`implements:confirmed:${repo}:intents/2030-01-04-baz/intent.md`]);
});

await test('retire closes git edges and ticket links, keeps the node and human edges; restore opens new edges', async (t) => {
  const { sync, edges, docEdges, links, sql, as } = await fresh(t);
  const repo = 'acme/widgets';
  const docs = {
    'specs/x.md': '# X\nTique: `k154`\n',
    'plans/x.md': '# PX\nSpec: docs/superpowers/specs/x.md\n',
    'plans/y.md': '# PY\n'
  };
  await sync(repo, docs);
  const node = async (p) => (await sql(`SELECT n.id, n.retired_at FROM graph.document d JOIN graph.node n ON n.id = d.node_id
    WHERE d.repo = $1 AND d.path = $2`, [repo, S + p]))[0];
  // A person's edge through the API (without git evidence) from plan y to spec x.
  await as('user:alice', 'human');
  await sql(`INSERT INTO graph.edge(src, src_kind, rel, dst, dst_kind) VALUES ($1, 'document', 'references', $2, 'document')`,
    [(await node('plans/y.md')).id, (await node('specs/x.md')).id]);
  const before = await node('specs/x.md');
  const r = await sync(repo, { 'plans/x.md': docs['plans/x.md'], 'plans/y.md': docs['plans/y.md'] });
  assert.deepEqual([r.retired, r.links.closed], [1, 1]);
  assert.equal(r.edges.closed, 2, 'in_project of x and the link plan -> x');
  assert.deepEqual(await node('specs/x.md'), before, 'node stays live');
  assert.deepEqual(await links(repo, 'specs/x.md'), []);
  assert.deepEqual(await docEdges(repo, 'plans/x.md'), []);
  assert.deepEqual(await docEdges(repo, 'plans/y.md'), [`references:confirmed:${repo}:specs/x.md`], 'human edge untouched');
  const closed = (await edges(repo, 'plans/x.md', true)).filter((e) => e.rel === 'implements');
  assert.deepEqual(closed.map((e) => e.close_reason), ['removed']);
  // Return: new edge rows, the history is kept.
  const back = await sync(repo, docs);
  assert.deepEqual([back.restored, back.links.opened], [1, 1]);
  const again = (await edges(repo, 'plans/x.md', true)).filter((e) => e.rel === 'implements');
  assert.equal(again.length, 2);
  assert.ok(again[1].id > again[0].id && again[1].close_reason === null);
  assert.equal((await edges(repo, 'specs/x.md')).filter((e) => e.rel === 'in_project').length, 1);
});

await test('a repeated revision writes nothing; a stale one is refused', async (t) => {
  const { sync, sql } = await fresh(t);
  const docs = { 'intents/r.md': '# I\nTique: `k154`\n', 'specs/r.md': '# S\n', 'plans/r.md': '# P\n' };
  await sync('acme/widgets', docs);
  const count = async () => (await sql(`SELECT (SELECT count(*) FROM graph.edge)::int AS e, (SELECT count(*) FROM ticket_change)::int AS c,
    (SELECT count(*) FROM ticket_documents)::int AS l, (SELECT count(*) FROM document_ref)::int AS r`))[0];
  const before = await count();
  const state = await sql('SELECT * FROM repository_document_sync_state');
  const revision = state.find((s) => s.repo === 'acme/widgets').revision;
  await sql(`SELECT set_config('hub.actor', 'docs-widgets', false), set_config('hub.actor_kind', 'service:docs-widgets', false)`);
  const snap = { repo: 'acme/widgets', revision, ancestors: [revision], documents: Object.entries(docs).map(([p, body]) => ({ path: S + p, body })) };
  const r = (await sql('SELECT public.sync_repository_documents($1::jsonb) AS r', [JSON.stringify(snap)]))[0].r;
  assert.deepEqual([r.updated, r.retired, r.restored, r.links, r.edges], [0, 0, 0, { opened: 0, closed: 0 }, { opened: 0, closed: 0, proposed: 0 }]);
  assert.deepEqual(await count(), before);
  assert.deepEqual(await sql('SELECT * FROM repository_document_sync_state'), state);
  const stale = { ...snap, revision: 'f'.repeat(40), ancestors: ['f'.repeat(40)] };
  await assert.rejects(sql('SELECT public.sync_repository_documents($1::jsonb)', [JSON.stringify(stale)]), code('DSSTL'));
});

await test('registry and raw links cannot be deleted or truncated', async (t) => {
  const { sql, sync } = await fresh(t);
  await sync('acme/widgets', { 'specs/a.md': '# A\n', 'plans/a.md': '# P\ndocs/superpowers/specs/a.md\n' });
  for (const table of ['document_repository', 'document_ref']) {
    await assert.rejects(sql(`DELETE FROM ${table}`), code('GRFRB'), `DELETE ${table}`);
    await assert.rejects(sql(`TRUNCATE ${table} CASCADE`), code('GRFRB'), `TRUNCATE ${table}`);
  }
  await assert.rejects(sql(`INSERT INTO repository_document_sync_state(repo, revision, synced_at) VALUES ('acme/x', $1, now())`, ['a'.repeat(40)]),
    /repository_document_sync_state_repo_fkey/);
  const shapes = await sql(`SELECT rel, src_kind, dst_kind, single, acyclic, needs_confirm FROM graph.rel_shape
    WHERE src_kind = 'document' AND account = public.instance_account() ORDER BY rel`);
  assert.deepEqual(shapes.map((s) => `${s.rel}:${s.dst_kind}:${s.single}${s.acyclic}${s.needs_confirm}`),
    ['implements:document:falsefalsefalse', 'in_project:project:falsefalsefalse', 'references:document:falsefalsefalse', 'specifies:document:falsefalsefalse']);
});
