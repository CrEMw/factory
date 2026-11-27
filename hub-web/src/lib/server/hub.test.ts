// hub-web caller headers: node --test, no build (pnpm run test).
import { mock, test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { getOptionalJson, hubFetch, hubHeaders, PREVIOUS_SECRET_MS, SECRET_TTL_MS } from '#lib/server/hub';

test('hubHeaders: no file, file, reread after cache; hubFetch retries with the previous secret on 401', async () => {
  mock.timers.enable({ apis: ['Date'], now: 1_000_000 });
  const dir = mkdtempSync(join(tmpdir(), 'hub-web-caller-'));
  const file = join(dir, 'secret');
  process.env.HUB_CALLER_SECRET_FILE = file;
  process.env.HUB_HTTP = 'http://hub.test';
  const warn = mock.method(console, 'warn', () => {});
  try {
    // No file: only the name, no Authorization.
    assert.deepEqual(hubHeaders({ 'content-type': 'application/json' }), {
      'content-type': 'application/json',
      'x-hub-caller': 'hub-web'
    });
    assert.equal(warn.mock.callCount(), 1);

    // File appears: the cached answer until the cache expires, then the secret is in the header.
    writeFileSync(file, 'old-secret\n');
    assert.equal(hubHeaders().authorization, undefined, 'cached for 30 s');
    mock.timers.tick(SECRET_TTL_MS);
    assert.deepEqual(hubHeaders(), { 'x-hub-caller': 'hub-web', authorization: 'Bearer old-secret' });

    // Rotation: new secret after the cache; a hub with the old one answers 401 to it, the retry uses the previous.
    writeFileSync(file, 'new-secret');
    mock.timers.tick(SECRET_TTL_MS);
    assert.equal(hubHeaders().authorization, 'Bearer new-secret');
    const seen: (string | undefined)[] = [];
    const oldHub = (async (_url: string, init: { headers: Record<string, string> }) => {
      seen.push(init.headers.authorization);
      return new Response(null, { status: init.headers.authorization === 'Bearer old-secret' ? 200 : 401 });
    }) as unknown as typeof fetch;
    assert.equal((await hubFetch(oldHub, '/api/tickets')).status, 200);
    assert.deepEqual(seen, ['Bearer new-secret', 'Bearer old-secret']);

    // The previous secret is forgotten after 15 minutes: 401 is returned without a retry.
    mock.timers.tick(PREVIOUS_SECRET_MS);
    seen.length = 0;
    assert.equal((await hubFetch(oldHub, '/api/tickets')).status, 401);
    assert.deepEqual(seen, ['Bearer new-secret']);

    // File removed: the header is without a secret again.
    rmSync(file);
    mock.timers.tick(SECRET_TTL_MS);
    assert.equal(hubHeaders().authorization, undefined);
  } finally {
    warn.mock.restore();
    mock.timers.reset();
    rmSync(dir, { recursive: true, force: true });
  }
});

test('getOptionalJson: route missing, hub failure and network error do not throw; data passes through', async () => {
  process.env.HUB_HTTP = 'http://hub.test';
  delete process.env.HUB_CALLER_SECRET_FILE;
  const warn = mock.method(console, 'warn', () => {});
  const logged = mock.method(console, 'error', () => {});
  const answer = (res: Response | Error) =>
    (async () => {
      if (res instanceof Error) throw res;
      return res;
    }) as unknown as typeof fetch;
  try {
    assert.equal(await getOptionalJson(answer(new Response('', { status: 404 })), '/l'), 'missing');
    assert.equal(await getOptionalJson(answer(new Response('lock timeout', { status: 500 })), '/l'), 'failed');
    assert.equal(await getOptionalJson(answer(new Error('connect refused')), '/l'), 'failed');
    assert.equal(await getOptionalJson(answer(new Response('not json', { status: 200 })), '/l'), 'failed');
    assert.deepEqual(await getOptionalJson(answer(new Response('{"links":[]}', { status: 200 })), '/l'), { links: [] });
    assert.equal(logged.mock.callCount(), 3, 'every failure is logged');
  } finally {
    logged.mock.restore();
    warn.mock.restore();
  }
});
