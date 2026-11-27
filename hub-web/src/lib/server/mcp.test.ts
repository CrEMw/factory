// MCP over hub: actor agent in the headers, an empty 404 from hub = "feature is still rolling out".
import { mock, test } from 'node:test';
import assert from 'node:assert/strict';
import { agentWriter, call, docRepo, docReposOf, fieldsRollingOut, filterRollingOut, getGraph, knownDocRepos, mcpServerName, NO_USER, refreshDocRepos, ROLLING_OUT } from '#lib/server/mcp';

test('call: agent actor goes to hub; empty 404 is rolling out, 404 with a body is passed on', async () => {
  process.env.HUB_HTTP = 'http://hub.test';
  delete process.env.HUB_CALLER_SECRET_FILE;
  const warn = mock.method(console, 'warn', () => {});
  const seen: Record<string, string>[] = [];
  let reply = new Response('', { status: 404 });
  const fetchMock = mock.method(globalThis, 'fetch', async (_url: string, init: { headers: Record<string, string> }) => {
    seen.push(init.headers);
    return reply;
  });
  try {
    let res = await call('POST', '/api/edges', { rel: 'blocks' }, { kind: 'agent', id: 'user:1' });
    assert.equal(res.isError, true);
    assert.equal(res.content[0].text, ROLLING_OUT);
    assert.equal(ROLLING_OUT, 'feature is still rolling out: hub does not have it yet, retry after the rollout');
    assert.equal(seen[0]['x-hub-actor-kind'], 'agent');
    assert.equal(seen[0]['x-hub-actor'], 'user:1');
    assert.equal(seen[0]['x-hub-caller'], 'hub-web');

    reply = new Response('no such ticket', { status: 404 });
    res = await call('GET', '/api/tickets/zz/links');
    assert.equal(res.content[0].text, 'hub 404: no such ticket');
    assert.equal(seen[1]['x-hub-actor-kind'], undefined, 'reads go without an actor');

    reply = new Response('{"error":"cycle","path":["a","b","a"]}', { status: 409 });
    res = await call('POST', '/api/edges', {}, { kind: 'agent', id: 'user:1' });
    assert.match(res.content[0].text, /^hub 409: .*cycle/);
  } finally {
    fetchMock.mock.restore();
    warn.mock.restore();
  }
});

test('agentWriter: without sub in the token a write is refused before hub; with sub it goes as user:<sub>', async () => {
  process.env.HUB_HTTP = 'http://hub.test';
  delete process.env.HUB_CALLER_SECRET_FILE;
  const warn = mock.method(console, 'warn', () => {});
  const seen: Record<string, string>[] = [];
  const fetchMock = mock.method(globalThis, 'fetch', async (_url: string, init: { headers: Record<string, string> }) => {
    seen.push(init.headers);
    return new Response('{}', { status: 201 });
  });
  try {
    const res = await agentWriter(undefined)('POST', '/api/edges', {});
    assert.equal(res.isError, true);
    assert.equal(res.content[0].text, NO_USER);
    assert.equal(NO_USER, 'cannot write without a user in the token: sign in to the hub with your own account');
    assert.equal(seen.length, 0, 'hub is not called');
    assert.equal((await agentWriter('u1')('POST', '/api/edges', {})).isError, undefined);
    assert.equal(seen[0]['x-hub-actor'], 'user:u1');
    assert.equal(seen[0]['x-hub-actor-kind'], 'agent');
  } finally {
    fetchMock.mock.restore();
    warn.mock.restore();
  }
});

// An old hub silently drops assignee, components and filters on them; MCP notices it from the response.
test('fieldsRollingOut and filterRollingOut: an old hub that ignores the new fields is reported', () => {
  const ok = (text: string) => ({ content: [{ type: 'text' as const, text }] });
  assert.equal(fieldsRollingOut({}, ok('{"code":"a"}')), undefined, 'no fields sent');
  assert.equal(fieldsRollingOut({ assignee: 'alice' }, ok('{"code":"a","assignee":"alice","components":[]}')), undefined);
  const old = fieldsRollingOut({ components: ['hub'] }, ok('{"code":"a"}'));
  assert.equal(old?.isError, true);
  assert.match(old!.content[0].text, /^ticket saved without assignee and components: feature is still rolling out/);
  const err = { isError: true, content: [{ type: 'text' as const, text: 'hub 404' }] };
  assert.equal(fieldsRollingOut({ assignee: 'alice' }, err), undefined, 'errors pass as they are');

  assert.equal(filterRollingOut({ assignee: 'alice' }, ok('{"total":0,"items":[],"assignee":"alice"}')).isError, undefined);
  assert.equal(filterRollingOut({ assignee: undefined }, ok('{"total":0,"items":[]}')).isError, undefined);
  assert.equal(filterRollingOut({ ready: true, assignee: 'alice' }, ok('{"total":0,"items":[],"ready":true}')).content[0].text, ROLLING_OUT);
  assert.equal(filterRollingOut({ ready: true }, ok('{"total":0,"items":[]}')).content[0].text, ROLLING_OUT);
});

// get_graph calls /api/graph with a lowercase code and returns a slimmed graph without ticket summaries.
test('getGraph: needs project or root, sends the query to hub and slims the answer', async () => {
  process.env.HUB_HTTP = 'http://hub.test';
  delete process.env.HUB_CALLER_SECRET_FILE;
  const warn = mock.method(console, 'warn', () => {});
  const urls: string[] = [];
  const graph = {
    root: 'z3',
    layers: 2,
    truncated: false,
    nodes: [
      { code: 'z2', project: 'demo', summary: 'secret summary', status: 'done', priority: 'p1', layer: 0, order: 0 },
      { code: 'z3', project: 'demo', summary: 'summary', status: 'open', priority: null, layer: 1, order: 0 }
    ],
    edges: [{ id: 7, src: 'z2', dst: 'z3', rel: 'blocks', state: 'confirmed' }]
  };
  let reply = () => new Response(JSON.stringify(graph), { status: 200 });
  const fetchMock = mock.method(globalThis, 'fetch', async (url: string) => {
    urls.push(String(url));
    return reply();
  });
  try {
    const none = await getGraph({});
    assert.equal(none.isError, true);
    assert.equal(urls.length, 0, 'hub is not called without project or root');

    const res = await getGraph({ root: 'Z3', depth: 10 });
    assert.equal(res.isError, undefined);
    assert.equal(urls[0], 'http://hub.test/api/graph?root=z3&depth=10');
    assert.deepEqual(JSON.parse(res.content[0].text), {
      layers: 2,
      truncated: false,
      nodes: [
        { code: 'z2', project: 'demo', status: 'done', priority: 'p1', layer: 0, order: 0 },
        { code: 'z3', project: 'demo', status: 'open', priority: null, layer: 1, order: 0 }
      ],
      edges: [{ src: 'z2', dst: 'z3', rel: 'blocks', state: 'confirmed' }]
    });

    await getGraph({ project: 'demo' });
    assert.equal(urls[1], 'http://hub.test/api/graph?project=demo');

    reply = () => new Response('', { status: 404 });
    assert.equal((await getGraph({ project: 'demo' })).content[0].text, ROLLING_OUT);
  } finally {
    fetchMock.mock.restore();
    warn.mock.restore();
  }
});

// Estimate and WSJF are written by PUT on behalf of the agent, plan and next task are read without an actor.
test('set_estimate and set_wsjf go as PUT with the agent; get_plan and next_task read without an actor', async () => {
  process.env.HUB_HTTP = 'http://hub.test';
  delete process.env.HUB_CALLER_SECRET_FILE;
  const warn = mock.method(console, 'warn', () => {});
  const seen: { url: string; method: string; headers: Record<string, string>; body?: string }[] = [];
  const fetchMock = mock.method(globalThis, 'fetch', async (url: string, init: { method: string; headers: Record<string, string>; body?: string }) => {
    seen.push({ url, ...init });
    return new Response('{}', { status: 200 });
  });
  try {
    await agentWriter('u1')('PUT', '/api/tickets/a/estimate', { effort_sp: 3 });
    assert.equal(seen[0].method, 'PUT');
    assert.equal(seen[0].headers['x-hub-actor-kind'], 'agent');
    assert.equal(seen[0].body, '{"effort_sp":3}');
    await call('GET', '/api/next?person=alice');
    assert.equal(seen[1].headers['x-hub-actor-kind'], undefined);
    assert.match(seen[1].url, /\/api\/next\?person=alice$/);
  } finally {
    fetchMock.mock.restore();
    warn.mock.restore();
  }
});

// Step 2.8: the document repo list comes from hub (GET /api/documents); the MCP schema accepts any owner/name repo
// by the hub rule (query.rs valid_repo), not a list built into hub-web.
test('docRepo: a repo from the hub response passes the schema, garbage does not; the repo list comes from GET /api/documents', async () => {
  process.env.HUB_HTTP = 'http://hub.test';
  delete process.env.HUB_CALLER_SECRET_FILE;
  const warn = mock.method(console, 'warn', () => {});
  const rows = [
    { repo: 'acme/docs', path: 'docs/a.md', role: 'spec', title: 'A', created_at: '2030-01-01T00:00:00Z', retired_at: null, tickets: 0 },
    { repo: 'Acme-2/site.v2_x', path: 'docs/b.md', role: 'plan', title: 'B', created_at: '2030-01-01T00:00:00Z', retired_at: null, tickets: 1 },
    { repo: 'acme/docs', path: 'docs/c.md', role: 'intent', title: 'C', created_at: '2030-01-01T00:00:00Z', retired_at: null, tickets: 0 }
  ];
  const seen: string[] = [];
  const fetchMock = mock.method(globalThis, 'fetch', async (url: string) => {
    seen.push(url);
    return new Response(JSON.stringify(rows), { status: 200 });
  });
  try {
    assert.deepEqual(docReposOf(rows), ['Acme-2/site.v2_x', 'acme/docs']);
    assert.deepEqual(docReposOf({ not: 'a list' }), []);
    for (const r of docReposOf(rows)) assert.equal(docRepo.safeParse(r).success, true, r);
    for (const bad of ['acme', 'acme/', '/docs', 'a/b/c', 'ac me/docs', 'acme/do;cs', `${'a'.repeat(40)}/x`]) {
      assert.equal(docRepo.safeParse(bad).success, false, bad);
    }

    assert.deepEqual(knownDocRepos(), []);
    await refreshDocRepos(0);
    assert.equal(seen.length, 1);
    assert.equal(seen[0], 'http://hub.test/api/documents');
    assert.deepEqual(knownDocRepos(), ['Acme-2/site.v2_x', 'acme/docs']);
    // A fresh list is not reread; an old hub with an empty 404 keeps the previous list.
    await refreshDocRepos(1000);
    assert.equal(seen.length, 1);
    fetchMock.mock.mockImplementation(async (url: string) => {
      seen.push(url);
      return new Response('', { status: 404 });
    });
    await refreshDocRepos(0);
    assert.equal(seen.length, 2);
    assert.deepEqual(knownDocRepos(), ['Acme-2/site.v2_x', 'acme/docs']);
  } finally {
    fetchMock.mock.restore();
    warn.mock.restore();
  }
});

test('mcpServerName: MCP_SERVER_NAME, factory-hub by default', () => {
  assert.equal(mcpServerName({}), 'factory-hub');
  assert.equal(mcpServerName({ MCP_SERVER_NAME: 'demo-hub' }), 'demo-hub');
});
