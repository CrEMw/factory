// Step 2.12 (a) of the factory plan: link labels of documents from public.instance (migration 0027). The parser
// public.doc_ticket_tokens takes the labels as a parameter; sync_repository_documents reads them from the instance row,
// which `hub migrate` seeds from HUB_DOCUMENT_LINK_LABELS. PGlite in memory, no network. An install that kept the
// words of the parser before 0027 checks them in its own tests.
import assert from 'node:assert/strict';
import test from 'node:test';
import { migrationFiles, migrationSql } from './hub-migrations.mjs';

const { PGlite } = await import(new URL('../.tools/node_modules/@electric-sql/pglite/dist/index.js', import.meta.url));
const code = (c) => (e) => e.code === c;
const S = 'docs/superpowers/';
const ENGLISH = { describes: ['Ticket', 'Tickets'], closes: ['Closes'], and: ['and'] };
// An install with labels in another language (Spanish) next to the English ones.
const CUSTOM = { describes: ['Tique', 'Tiques', 'Ticket', 'Tickets'], closes: ['Cierra', 'Closes'], and: ['y', 'and'] };

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
  const seed = (labels) => labels === undefined
    ? sql("SELECT public.seed_instance('demo', 'Europe/Athens', 20, NULL, ARRAY['docs/superpowers'])")
    : sql("SELECT public.seed_instance('demo', 'Europe/Athens', 20, NULL, ARRAY['docs/superpowers'], $1::jsonb)",
      [labels === null ? null : JSON.stringify(labels)]);
  const labels = async () => (await sql('SELECT document_link_labels AS l FROM public.instance'))[0]?.l;
  return { db, sql, seed, labels };
}

await test('default labels are English: header and closes lines in another language give no links, English ones do', async (t) => {
  const { db, sql, seed, labels } = await fresh(t);
  await seed();
  assert.deepEqual(await labels(), ENGLISH, 'column default');
  await db.exec(`BEGIN; SELECT set_config('hub.actor', 'seed:test', true), set_config('hub.actor_kind', 'seed', true);
    SELECT public.import_document_repositories('[{"repo": "acme/widgets", "project": "widgets", "caller": "docs-widgets"}]');
    INSERT INTO tickets(project, code, summary) VALUES ('widgets', 'k154', 'a'), ('widgets', 'k109', 'b'), ('widgets', 'k15g', 'c'); COMMIT;`);
  await sql(`SELECT set_config('hub.actor', 'docs-widgets', false), set_config('hub.actor_kind', 'service:docs-widgets', false)`);
  let n = 0;
  const history = [];
  const sync = async (docs) => {
    const revision = (++n).toString(16).padStart(40, '0');
    history.unshift(revision);
    const documents = Object.entries(docs).map(([p, body]) => ({ path: S + p, body }));
    return (await sql('SELECT public.sync_repository_documents($1::jsonb) AS r',
      [JSON.stringify({ repo: 'acme/widgets', revision, ancestors: history, documents })]))[0].r;
  };
  const links = (p) => sql(`SELECT l.rel || ':' || t.code AS x FROM ticket_documents l JOIN tickets t ON t.seq = l.ticket_seq
    WHERE l.repo = 'acme/widgets' AND l.path = $1 AND l.valid_to IS NULL ORDER BY 1`, [S + p]).then((r) => r.map((x) => x.x));
  const docs = {
    'specs/es.md': '# S\nTique: `k154` y `k109`\n\nCierra: `k15g`\n',
    'specs/en.md': '# E\nTickets: `k154` and `k109`\n\n- **Closes:** `k15g`\n'
  };
  assert.deepEqual((await sync(docs)).problems, []);
  assert.deepEqual(await links('specs/es.md'), [], 'words of another language are not labels by default');
  assert.deepEqual(await links('specs/en.md'), ['closes:k15g', 'describes:k109', 'describes:k154']);
  // Labels of the install: a change of the text parses again, both documents link.
  await seed(CUSTOM);
  await sync({ 'specs/es.md': docs['specs/es.md'] + '\n', 'specs/en.md': docs['specs/en.md'] + '\n' });
  assert.deepEqual(await links('specs/es.md'), ['closes:k15g', 'describes:k109', 'describes:k154']);
  assert.deepEqual(await links('specs/en.md'), ['closes:k15g', 'describes:k109', 'describes:k154']);
});

await test('label text is escaped in the regular expression', async (t) => {
  const { sql } = await fresh(t);
  const tokens = (text, l) => sql('SELECT rel || \':\' || token AS x FROM public.doc_ticket_tokens($1, $2::jsonb) ORDER BY 1',
    [text, JSON.stringify(l)]).then((r) => r.map((x) => x.x));
  const meta = { describes: ['T.k', 'a|b'], closes: ['C+(x)'], and: ['&*'] };
  assert.deepEqual(await tokens('Tak: `1001`\nC(x): `1002`\n', meta), [], 'dot, plus and parentheses are literal');
  assert.deepEqual(await tokens('a: `1001`\n', meta), [], 'bar is literal');
  assert.deepEqual(await tokens('T.k: `1001` &* `1002`\nC+(x): `1003`\n', meta), ['closes:1003', 'describes:1001', 'describes:1002']);
  assert.deepEqual(await tokens('a|b `1001`, `1002` and `1003`\n', meta), ['describes:1001', 'describes:1002'], 'and is not a label here');
  assert.deepEqual(await tokens('[x]^$ `1001`\n', { describes: ['[x]^$'], closes: ['\\d'], and: ['{1}'] }), ['describes:1001']);
  assert.deepEqual(await tokens('7: `1001`\n', { describes: ['x'], closes: ['\\d'], and: ['{1}'] }), [], 'backslash is literal');
});

await test('seeding: NULL and five arguments keep the labels, a bad shape is refused, only the owner seeds', async (t) => {
  const { db, sql, seed, labels } = await fresh(t);
  await seed(CUSTOM);
  assert.deepEqual(await labels(), CUSTOM);
  await seed(null);
  await seed();
  assert.deepEqual(await labels(), CUSTOM, 'an older hub migrate does not reset the labels');
  for (const bad of [
    [], 'x', {}, { describes: ['a'], closes: ['b'] }, { ...ENGLISH, extra: ['x'] }, { ...ENGLISH, and: [] },
    { ...ENGLISH, and: 'and' }, { ...ENGLISH, and: [1] }, { ...ENGLISH, and: [''] }, { ...ENGLISH, and: [' and'] },
    { ...ENGLISH, and: ['and '] }, { ...ENGLISH, and: ['a`b'] }, { ...ENGLISH, and: ['a\nb'] }, { ...ENGLISH, and: ['x'.repeat(41)] },
    { ...ENGLISH, and: Array(21).fill('x') }
  ]) {
    await assert.rejects(seed(bad), code('23514'), JSON.stringify(bad));
  }
  assert.deepEqual(await labels(), CUSTOM, 'refused seeds wrote nothing');
  await seed({ ...ENGLISH, and: ['x'.repeat(40), 'a b'] });
  await assert.rejects(sql(`UPDATE public.instance SET document_link_labels = '{}'`), code('23514'));
  await db.exec('RESET ROLE; SET ROLE hub');
  try {
    assert.deepEqual((await sql('SELECT public.instance_document_link_labels() AS l'))[0].l.describes, ENGLISH.describes);
    await assert.rejects(seed(ENGLISH), code('42501'), 'hub does not seed');
  } finally {
    await db.exec('RESET ROLE; SET ROLE app');
  }
});

await test('0027 bodies have no Cyrillic, the parser is IMMUTABLE and no index or constraint uses it', async (t) => {
  const { sql } = await fresh(t);
  const fns = ['doc_link_labels_valid', 'instance_document_link_labels', 'doc_link_label_pattern', 'doc_ticket_tokens',
    'doc_extract', 'sync_repository_documents', 'seed_instance'];
  const rows = await sql(`SELECT p.proname, p.prosrc, p.provolatile, pg_get_function_identity_arguments(p.oid) AS args
    FROM pg_proc p WHERE p.pronamespace = 'public'::regnamespace AND p.proname = ANY ($1) ORDER BY 1`, [fns]);
  assert.deepEqual(rows.map((r) => r.proname), [...fns].sort(), 'one function of each name: the old signatures are dropped');
  for (const r of rows) assert.doesNotMatch(r.prosrc, /\p{Script=Cyrillic}/u, r.proname);
  assert.equal(rows.find((r) => r.proname === 'doc_ticket_tokens').provolatile, 'i');
  assert.equal(rows.find((r) => r.proname === 'doc_ticket_tokens').args, 'p_text text, p_labels jsonb');
  assert.deepEqual(await sql(`SELECT c.relname FROM pg_depend d JOIN pg_class c ON c.oid = d.objid
    WHERE d.refobjid = 'public.doc_ticket_tokens(text, jsonb)'::regprocedure`), []);
  assert.deepEqual(await sql(`SELECT c.relname FROM pg_depend d JOIN pg_class c ON c.oid = d.objid
    WHERE d.refobjid = 'public.doc_link_label_pattern(jsonb, text)'::regprocedure`), []);
});
