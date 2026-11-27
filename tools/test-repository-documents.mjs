// Document snapshots from git (public.sync_repository_documents, migrations 0005 and 0017) on
// an isolated PostgreSQL: PGlite in memory, no network and no database address. Document links and edges: test-document-graph.mjs;
// gate layout: gate-layout.test.mjs (taken from main).
// Frozen documents (an install that imported documents once from an older system) are stood in for by a small
// synthetic set in acme/apps, written the way such an import writes them, with a watermark revision in
// repository_document_sync_state.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import test from 'node:test';
import { migrationFiles, migrationSql } from './hub-migrations.mjs';

const { PGlite } = await import(new URL('../.tools/node_modules/@electric-sql/pglite/dist/index.js', import.meta.url));
// The watermark of the frozen acme/apps documents: the first snapshot must descend from it.
const baseline = 'd0d84c0565e868cde0d7d28a67dfec4303ead6b2';
// The document repository registry, as the seed of the hub start writes it.
const REGISTRY = ['apps', 'gadgets', 'tools', 'widgets'].map((n) => ({ repo: 'acme/' + n, project: n, caller: 'docs-' + n }));
const rev = (letter) => letter.repeat(40);
const hash = (text) => createHash('sha256').update(text).digest('hex');
const path = 'docs/superpowers/plans/repository-sync-test.md';
const header = (codes) => (codes.length ? `Ticket: ${codes.map((c) => '`' + c + '`').join(', ')}\n` : '');
// New format: path and body. Ticket codes stand in the body on the ticket label line (header above).
const doc = (body = '# Probe\n', codes = ['z9'], p = path) => ({ path: p, body: header(codes) + body });
// The old format of the first document collector: role, title, hash, size and codes next to the body.
function oldDoc(body = '# Probe\n', codes = ['z9'], p = path) {
  const full = header(codes) + body;
  return { path: p, role: 'plan', title: 'Probe', body: full, bytes: Buffer.byteLength(full), sha: hash(full), codes };
}
const snapshot = (repo, revision, documents, ancestors = [revision, baseline]) => ({ repo, revision, ancestors, documents });
// The frozen set: tickets of the old system (legacy_id) and acme/apps documents with their links, as imported.
const FROZEN_AT = '2030-01-01T00:00:00Z';
const frozenTickets = [
  { id: 'a1b2c3d4e5f60789', seq: 1, code: 'z9', summary: 'Sync', status: 'open', body_md: '' },
  { id: 'b1b2c3d4e5f60789', seq: 2, code: 'z8', summary: 'Format', status: 'done', body_md: '' },
  { id: 'c1b2c3d4e5f60789', seq: 3, code: 'z7', summary: 'Old', status: 'done', body_md: '' }
];
const frozenDoc = (p, body, retired = false) => ({ path: p, role: p.split('/')[2] === 'specs' ? 'spec' : 'plan',
  title: body.match(/^# (.+)$/m)[1], body_md: body, body_sha256: hash(body), source_revision: baseline,
  created_at: FROZEN_AT, synced_at: FROZEN_AT, retired_at: retired ? FROZEN_AT : null });
const frozenDocuments = [
  frozenDoc('docs/superpowers/specs/2030-01-01-sync.md', header(['z9']) + '# Sync\n'),
  frozenDoc('docs/superpowers/plans/2030-01-02-format.md', header(['z9', 'z8']) + '# Format plan\n'),
  frozenDoc('docs/superpowers/plans/2030-01-03-plain.md', '# Plain\n'),
  frozenDoc('docs/superpowers/plans/2030-01-04-gone.md', header(['z7']) + '# Gone\n', true)
];
const frozenEdges = [
  { ticket_id: 'a1b2c3d4e5f60789', path: 'docs/superpowers/specs/2030-01-01-sync.md' },
  { ticket_id: 'a1b2c3d4e5f60789', path: 'docs/superpowers/plans/2030-01-02-format.md' },
  { ticket_id: 'b1b2c3d4e5f60789', path: 'docs/superpowers/plans/2030-01-02-format.md' },
  { ticket_id: 'c1b2c3d4e5f60789', path: 'docs/superpowers/plans/2030-01-04-gone.md', valid_to: FROZEN_AT }
].map((e) => ({ rel: 'describes', valid_from: FROZEN_AT, valid_to: null, ...e }));
const liveFrozenLinks = frozenEdges.filter((e) => !e.valid_to).length;
const code = (c) => (e) => e.code === c;

await test('repository snapshots preserve the frozen documents and isolate repositories', async (t) => {
  const db = new PGlite();
  t.after(() => db.close());
  // In CNPG the app role owns the database and creates schemas (migration 0007_auth creates schema auth).
  await db.exec(`CREATE ROLE app; CREATE ROLE hub; CREATE ROLE hub_web; GRANT ALL ON SCHEMA public TO app;
    DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO app', current_database()); END $$;
    SET ROLE app;`);
  for (const file of await migrationFiles()) {
    await db.exec('BEGIN;\n' + await migrationSql(file) + '\nCOMMIT;');
  }
  // Like Job hub-db-migrate: the instance row (0023) after the migrations; without it the account default (0025) gives HUBIN.
  // Default (English) link labels: the fixtures use English headers.
  await db.query("SELECT public.seed_instance('demo', 'Europe/Athens', 20, NULL, ARRAY['docs/superpowers'])");
  await db.query('SELECT public.import_document_repositories($1::jsonb)', [JSON.stringify(REGISTRY)]);
  await db.query(`INSERT INTO repository_document_sync_state(repo, revision, synced_at) VALUES ('acme/apps', $1, $2)`, [baseline, FROZEN_AT]);
  // The frozen set as the old import wrote it, with its actor; 0018 does not let tickets in without an actor.
  await db.query(`SELECT set_config('hub.actor', 'seed:legacy-apps', false), set_config('hub.actor_kind', 'seed', false)`);
  const [tickets, documents, edges] = [frozenTickets, frozenDocuments, frozenEdges];
  // Project explicitly (no default since 0025).
  await db.query(`INSERT INTO tickets(seq,project,code,summary,status,body,legacy_id)
    OVERRIDING SYSTEM VALUE SELECT seq,'apps',code,summary,status,body_md,id FROM jsonb_to_recordset($1)
    AS x(seq bigint,code text,summary text,status text,body_md text,id text)`, [JSON.stringify(tickets)]);
  await db.query(`INSERT INTO documents(repo,path,role,title,body,sha256,source_revision,created_at,synced_at,retired_at)
    SELECT 'acme/apps',path,role,title,body_md,body_sha256,source_revision,created_at,synced_at,retired_at
    FROM jsonb_to_recordset($1) AS x(path text,role text,title text,body_md text,body_sha256 text,
    source_revision text,created_at timestamptz,synced_at timestamptz,retired_at timestamptz)`, [JSON.stringify(documents)]);
  await db.query(`INSERT INTO ticket_documents SELECT t.seq,'acme/apps',x.path,x.rel,x.valid_from,x.valid_to
    FROM jsonb_to_recordset($1) AS x(ticket_id text,path text,rel text,valid_from timestamptz,valid_to timestamptz)
    JOIN tickets t ON t.legacy_id=x.ticket_id`, [JSON.stringify(edges)]);
  const sql = (query, params = []) => db.query(query, params).then((r) => r.rows);
  // Actor as in hub: caller docs-<repo> writes as service:docs-<repo>. Without an actor: the exec path.
  const as = (caller) => sql(`SELECT set_config('hub.actor', $1, false), set_config('hub.actor_kind', $2, false)`,
    [caller ?? '', caller ? 'service:' + caller : '']);
  const callerOf = (repo) => 'docs-' + String(repo).split('/')[1];
  const sync = async (value, caller = callerOf(value.repo)) => {
    await as(caller);
    return (await sql('SELECT public.sync_repository_documents($1::jsonb) AS result', [JSON.stringify(value)]))[0].result;
  };
  const all = async () => ({
    docs: await sql('SELECT * FROM documents ORDER BY repo,path'),
    links: await sql('SELECT * FROM ticket_documents ORDER BY repo,path,ticket_seq,rel,valid_from'),
    refs: await sql('SELECT * FROM document_ref ORDER BY repo,path,target_repo,target_path,valid_from'),
    state: await sql('SELECT * FROM repository_document_sync_state ORDER BY repo'),
    tickets: await sql('SELECT * FROM tickets ORDER BY seq'),
    edges: await sql('SELECT * FROM graph.edge ORDER BY id'),
    changes: await sql('SELECT count(*)::int AS n FROM ticket_change')
  });
  const rejectUnchanged = async (value, check, caller) => {
    const before = await all();
    await assert.rejects(sync(value, caller), check);
    assert.deepEqual(await all(), before, 'rejected snapshot must leave every table unchanged');
  };
  const frozenDocs = documents.filter((d) => !d.retired_at).map((d) => ({ path: d.path, body: d.body_md }));
  const toolsDoc = async () => (await sql("SELECT * FROM documents WHERE repo='acme/tools' AND path=$1", [path]))[0];

  await t.test('first snapshot cannot bypass the frozen export watermark', async () => {
    // The frozen repository has its watermark before any snapshot.
    await rejectUnchanged(snapshot('acme/apps', rev('b'), frozenDocs, [rev('b')]), code('DSSTL'));
  });

  await t.test('a descendant with every live frozen document changes no document and no title', async () => {
    const before = await sql('SELECT * FROM documents ORDER BY repo,path');
    assert.equal(before.length, frozenDocuments.length);
    const result = await sync(snapshot('acme/apps', rev('a'), frozenDocs));
    assert.equal(result.updated, 0);
    assert.equal(result.retired, 0);
    assert.equal(result.links.closed, 0, 'the first derivation of an imported text only adds links');
    const after = await sql('SELECT * FROM documents ORDER BY repo,path');
    const strip = (rows) => rows.map(({ derived_sha256, ...rest }) => rest);
    assert.deepEqual(strip(after), strip(before));
    // Only a document with a problem is not retired: it is parsed again on every sync.
    const problemPaths = new Set(result.problems.map((p) => p.path));
    assert.ok(after.filter((d) => !d.retired_at).every((d) => d.derived_sha256 === d.sha256
      || (d.derived_sha256 === null && problemPaths.has(d.path))));
    // All live links of the old system are alive, new ones may be added.
    const live = await sql("SELECT count(*)::int AS n FROM ticket_documents WHERE repo='acme/apps' AND valid_to IS NULL");
    assert.ok(live[0].n >= liveFrozenLinks, String(live[0].n));
  });

  await t.test('registry: only its repositories; every one is accepted and the first sync takes any history', async () => {
    await rejectUnchanged(snapshot('acme/other', rev('b'), [doc()], [rev('b')]), code('DSREP'), 'docs-other');
    for (const v of [null, 42]) await rejectUnchanged({ ...snapshot('acme/widgets', rev('b'), []), repo: v }, code('DSREP'));
    const empty = await sync(snapshot('acme/gadgets', rev('9'), [], [rev('9')]));
    assert.deepEqual([empty.documents, empty.updated, empty.retired], [0, 0, 0]);
    assert.deepEqual(await sql("SELECT revision, problems FROM repository_document_sync_state WHERE repo='acme/gadgets'"),
      [{ revision: rev('9'), problems: [] }]);
    assert.deepEqual((await sql(`SELECT repo, project, caller FROM document_repository WHERE repo LIKE 'acme/%' ORDER BY repo`)).map((r) => r.caller),
      REGISTRY.map((r) => r.caller));
  });

  await t.test('a caller writes only its own repository; the exec path writes as service:docs-exec', async () => {
    await rejectUnchanged(snapshot('acme/apps', rev('c'), frozenDocs, [rev('c'), rev('a')]), code('DSCAL'), 'docs-widgets');
    const r = await sync(snapshot('acme/widgets', rev('b'), [doc('# Hub\n', [], 'docs/superpowers/specs/hub.md')], [rev('b')]), null);
    assert.equal(r.updated, 1);
    assert.deepEqual(await sql(`SELECT created_by FROM graph.edge e JOIN graph.document d ON d.node_id = e.src
      WHERE d.repo = 'acme/widgets'`), [{ created_by: 'docs-exec' }]);
  });

  await t.test('old and new formats give the same rows', async () => {
    const other = 'docs/superpowers/plans/format.md';
    await sync(snapshot('acme/tools', rev('b'), [oldDoc(), oldDoc('# Format\n', ['z8'], other)], [rev('b')]));
    await sync(snapshot('acme/widgets', rev('c'), [doc(), doc('# Format\n', ['z8'], other)], [rev('c'), rev('b')]));
    const rows = async (repo) => (await sql(`SELECT path, role, title, body, sha256, derived_sha256 FROM documents
      WHERE repo = $1 AND path <> 'docs/superpowers/specs/hub.md' ORDER BY path`, [repo]));
    assert.deepEqual(await rows('acme/widgets'), await rows('acme/tools'));
    const links = async (repo) => sql(`SELECT path, ticket_seq, rel FROM ticket_documents WHERE repo = $1 AND valid_to IS NULL
      ORDER BY path, ticket_seq`, [repo]);
    assert.deepEqual(await links('acme/widgets'), await links('acme/tools'));
    assert.equal((await toolsDoc()).title, 'Probe', 'title from the first # line, not from the client');
  });

  await t.test('same paths coexist; duplicate delivery changes no rows, edges or journal', async () => {
    const before = await all();
    const repeat = await sync(snapshot('acme/tools', rev('b'), [oldDoc(), oldDoc('# Format\n', ['z8'], 'docs/superpowers/plans/format.md')], [rev('b')]));
    assert.deepEqual([repeat.updated, repeat.retired, repeat.restored, repeat.links, repeat.edges],
      [0, 0, 0, { opened: 0, closed: 0 }, { opened: 0, closed: 0, proposed: 0 }]);
    assert.deepEqual(await all(), before);
    assert.equal((await sql('SELECT repo FROM documents WHERE path=$1', [path])).length, 2);
  });

  await t.test('one bad document keeps its previous version; the rest of the snapshot syncs', async () => {
    const fresh = 'docs/superpowers/specs/fresh.md';
    const before = await toolsDoc();
    const r = await sync(snapshot('acme/tools', rev('d'), [
      doc('# Changed\n', ['zzzz']), // unknown ticket
      doc('# Fresh\n', [], fresh),
      { path: 'docs/superpowers/plans/../bad.md', body: 'x' },
      { path: 'docs/superpowers/plans/README.md', body: 'x' },
      { path: 'docs/superpowers/plans/a\\b.md', body: 'x' },
      { path: 42, body: 'x' },
      oldDoc('# Format\n', ['z8'], 'docs/superpowers/plans/format.md')
    ], [rev('d'), rev('b')]));
    assert.deepEqual(r.problems.map((p) => p.kind).sort(), ['bad_path', 'bad_path', 'bad_path', 'bad_path', 'unknown_code']);
    assert.deepEqual(r.problems.find((p) => p.kind === 'unknown_code'), { path, kind: 'unknown_code', detail: 'zzzz' });
    assert.deepEqual(await toolsDoc(), before, 'previous version kept, not retired');
    assert.equal((await sql("SELECT count(*)::int AS n FROM documents WHERE repo='acme/tools' AND path=$1", [fresh]))[0].n, 1);
    assert.deepEqual((await sql("SELECT problems FROM repository_document_sync_state WHERE repo='acme/tools'"))[0].problems, r.problems);
    // The ticket appeared: the same snapshot of the next revision gets through, problems are empty.
    await sql("INSERT INTO tickets(seq, project, code, summary) OVERRIDING SYSTEM VALUE VALUES (5000, 'platform', 'zzzz', 'late')");
    const ok = await sync(snapshot('acme/tools', rev('e'), [doc('# Changed\n', ['zzzz']), doc('# Fresh\n', [], fresh),
      oldDoc('# Format\n', ['z8'], 'docs/superpowers/plans/format.md')], [rev('e'), rev('d')]));
    assert.deepEqual(ok.problems, []);
    assert.equal((await toolsDoc()).title, 'Changed');
  });

  await t.test('stale and unrelated snapshots cannot retire current docs', async () => {
    await rejectUnchanged(snapshot('acme/apps', rev('7'), frozenDocs, [rev('7')]), code('DSSTL'));
    await rejectUnchanged(snapshot('acme/tools', rev('f'), [], [rev('f'), rev('c')]), code('DSSTL'));
  });

  await t.test('snapshot-level errors reject everything', async () => {
    const base = snapshot('acme/tools', rev('f'), [doc()], [rev('f'), rev('e')]);
    for (const patch of [
      { revision: 'bad' }, { ancestors: [] }, { ancestors: [rev('f'), 42] }, { documents: null },
      { documents: [doc(), doc()] }, { documents: [42] }, { documents: Array.from({ length: 5001 }, (_, i) => doc('x', [], `docs/superpowers/plans/${i}.md`)) }
    ]) await rejectUnchanged({ ...base, ...patch }, code('DSINV'));
  });

  await t.test('per-document metadata and bodies fail closed without erasing the document', async () => {
    const before = await toolsDoc();
    for (const patch of [{ role: 'spec' }, { sha: 'f'.repeat(64) }, { bytes: 1 }, { bytes: '12' }, { body: null }, { body: 42 }]) {
      const r = await sync(snapshot('acme/tools', rev('e'), [{ ...oldDoc('# Other\n', ['zzzz']), ...patch },
        doc('# Fresh\n', [], 'docs/superpowers/specs/fresh.md'), oldDoc('# Format\n', ['z8'], 'docs/superpowers/plans/format.md')], [rev('e')]));
      assert.equal(r.problems.length, 1, JSON.stringify(patch));
      assert.deepEqual(await toolsDoc(), before, JSON.stringify(patch));
    }
  });

  await t.test('revision must be a JSON string even when a JSON number has 40 valid hex digits', async () => {
    const before = await all();
    const payload = JSON.stringify(snapshot('acme/tools', '1'.repeat(40), [doc()], ['1'.repeat(40), rev('e')]))
      .replace('"revision":"' + '1'.repeat(40) + '"', '"revision":' + '1'.repeat(40));
    await assert.rejects(sql('SELECT public.sync_repository_documents($1::jsonb)', [payload]), code('DSINV'));
    assert.deepEqual(await all(), before);
  });

  await t.test('UTF-8 body limit is inclusive; a larger body is a problem, not an erasure', async () => {
    const exact = { path, body: 'é'.repeat(524288) };
    const keep = [doc('# Fresh\n', [], 'docs/superpowers/specs/fresh.md'), oldDoc('# Format\n', ['z8'], 'docs/superpowers/plans/format.md')];
    const r = await sync(snapshot('acme/tools', rev('0'), [exact, ...keep], [rev('0'), rev('e')]));
    assert.deepEqual(r.problems, []);
    const big = await sync(snapshot('acme/tools', rev('1'), [{ path, body: exact.body + '!' }, ...keep], [rev('1'), rev('0')]));
    assert.deepEqual(big.problems.map((p) => p.kind), ['too_large']);
    const old = await sync(snapshot('acme/tools', rev('2'), [{ ...oldDoc(), body: null, bytes: 1048577 }, ...keep], [rev('2'), rev('1')]));
    assert.deepEqual(old.problems.map((p) => p.kind), ['too_large']);
    assert.equal((await toolsDoc()).body, exact.body);
    await sync(snapshot('acme/tools', rev('3'), [doc(), ...keep], [rev('3'), rev('2')]));
  });

  await t.test('retirement and restore preserve body, URL and link history in one repository', async () => {
    const frozenBefore = await sql("SELECT * FROM documents WHERE repo='acme/apps' ORDER BY path");
    const ticketRows = await sql('SELECT * FROM tickets ORDER BY seq');
    const result = await sync(snapshot('acme/tools', rev('4'), [], [rev('4'), rev('3')]));
    assert.equal(result.retired, 3);
    assert.equal(result.links.closed, 2);
    const retired = await toolsDoc();
    assert.equal(retired.body, header(['z9']) + '# Probe\n');
    assert.ok(retired.retired_at);
    assert.deepEqual(await sql("SELECT * FROM documents WHERE repo='acme/apps' ORDER BY path"), frozenBefore);
    const restored = await sync(snapshot('acme/tools', rev('5'), [doc('# New\n', ['z8', 'z8'])], [rev('5'), rev('4')]));
    assert.deepEqual([restored.restored, restored.updated, restored.links.opened], [1, 1, 1]);
    const row = await toolsDoc();
    assert.equal(row.retired_at, null);
    assert.equal(row.created_at.getTime(), retired.created_at.getTime());
    assert.equal(row.title, 'New');
    const links = await sql("SELECT * FROM ticket_documents WHERE repo='acme/tools' AND path=$1 ORDER BY valid_from", [path]);
    assert.equal(links.filter((l) => l.valid_to).length, 3, 'z9, zzzz, z9 before');
    assert.equal(links.filter((l) => !l.valid_to).length, 1);
    assert.deepEqual(await sql('SELECT * FROM tickets ORDER BY seq'), ticketRows);
  });

  await t.test('the ticket link set follows the header and keeps intervals', async () => {
    const result = await sync(snapshot('acme/tools', rev('6'), [doc('# New\n', ['z9'])], [rev('6'), rev('5')]));
    assert.deepEqual(result.links, { opened: 1, closed: 1 });
    assert.equal(result.updated, 1);
    assert.equal((await sql("SELECT * FROM ticket_documents WHERE repo='acme/tools' AND rel='describes' AND path=$1 AND valid_to IS NOT NULL", [path])).length, 4);
    await assert.rejects(sql("INSERT INTO ticket_documents SELECT seq,'acme/tools',$1,'references',clock_timestamp(),NULL FROM tickets WHERE code='z9'", [path]),
      /ticket_documents_rel/);
  });

  await t.test('PUBLIC cannot execute the invoker function', async () => {
    await db.exec('RESET ROLE; CREATE ROLE outsider; SET ROLE outsider;');
    await assert.rejects(sql('SELECT public.sync_repository_documents($1::jsonb)', [JSON.stringify(snapshot('acme/tools', rev('f'), []))]),
      /permission denied for function/);
  });
});
