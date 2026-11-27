// Form sign-in freshness; linking and mailbox keys without a fresh session do not reach hub; an old hub
// (empty 404) gives "still rolling out"; Next takes the person from the link. node --test, no build.
import { mock, test } from 'node:test';
import assert from 'node:assert/strict';
import { actorHeaders, formActor, FRESH_MS, isFresh } from '#lib/server/hub';
import { setLocale, t } from '#lib/i18n/index';
import { bindLogin, fingerprint, listLogins, myHandle, reloginHref, retireLogin, reviewKey, safeNext } from '#lib/server/logins';
import { personFor } from '#lib/plan/person';
import type { Person } from '#lib/server/hub';

setLocale('en');

const NOW = Date.parse('2030-01-13T10:00:00Z');
const user = { id: 'u1', email: 'alice@example.org', name: 'Alice' };
const locals = (createdAt: number | null): App.Locals => ({ user, sessionCreatedAt: createdAt });

type Seen = { url: string; method: string; headers: Record<string, string>; body?: string };
function fakeHub(res: () => Response) {
  const seen: Seen[] = [];
  const fetch = (async (url: string, init: { method?: string; headers: Record<string, string>; body?: string }) => {
    seen.push({ url, method: init.method ?? 'GET', headers: init.headers, body: init.body });
    return res();
  }) as unknown as typeof globalThis.fetch;
  return { fetch, seen };
}

function quiet() {
  process.env.HUB_HTTP = 'http://hub.test';
  delete process.env.HUB_CALLER_SECRET_FILE;
  const warn = mock.method(console, 'warn', () => {});
  const err = mock.method(console, 'error', () => {});
  return () => {
    warn.mock.restore();
    err.mock.restore();
  };
}

test('freshness: sign-in no older than 5 minutes, header only for a human with a fresh sign-in', () => {
  assert.equal(isFresh(NOW - FRESH_MS, NOW), true);
  assert.equal(isFresh(NOW - FRESH_MS - 1, NOW), false);
  assert.equal(isFresh(null, NOW), false);
  assert.equal(isFresh(undefined, NOW), false);
  assert.equal(isFresh(NOW + 2 * 60_000, NOW), false, 'session from the future');
  assert.deepEqual(formActor(locals(NOW - 1000), NOW), { kind: 'human', id: 'user:u1', fresh: true });
  assert.deepEqual(formActor(locals(NOW - FRESH_MS - 1), NOW), { kind: 'human', id: 'user:u1', fresh: false });
  assert.equal(formActor({ user: null, sessionCreatedAt: NOW }, NOW), undefined);
  assert.deepEqual(actorHeaders({ kind: 'human', id: 'user:u1', fresh: true }), {
    'x-hub-actor-kind': 'human',
    'x-hub-actor': 'user:u1',
    'x-hub-fresh': '1'
  });
  assert.deepEqual(actorHeaders({ kind: 'human', id: 'user:u1', fresh: false }), { 'x-hub-actor-kind': 'human', 'x-hub-actor': 'user:u1' });
  assert.deepEqual(actorHeaders({ kind: 'agent', id: 'user:u1', fresh: true }), { 'x-hub-actor-kind': 'agent', 'x-hub-actor': 'user:u1' });
});

test('forms without a fresh session are refused before hub; fresh ones reach hub with x-hub-fresh', async () => {
  const restore = quiet();
  try {
    const hub = fakeHub(() => new Response('{}', { status: 201 }));
    for (const out of [
      await bindLogin(hub.fetch, locals(NOW - FRESH_MS - 1), 'alice', NOW),
      await retireLogin(hub.fetch, locals(NOW - FRESH_MS - 1), 'alice', NOW),
      await reviewKey(hub.fetch, locals(NOW - FRESH_MS - 1), 'sha256:ab', 'approved', NOW)
    ]) {
      assert.deepEqual(out, { ok: false, status: 403, message: 'A fresh sign-in is required (no older than 5 minutes): sign in again and retry.', relogin: true });
    }
    const noSession = await bindLogin(hub.fetch, { user: null, sessionCreatedAt: null }, 'alice', NOW);
    assert.equal(noSession.ok, false);
    assert.equal(!noSession.ok && noSession.status, 401);
    assert.equal(hub.seen.length, 0, 'nothing reached hub');

    assert.equal((await bindLogin(hub.fetch, locals(NOW - 1000), ' Alice ', NOW)).ok, true);
    assert.equal(hub.seen[0].url, 'http://hub.test/api/people/logins');
    assert.equal(hub.seen[0].body, '{"handle":"alice"}');
    assert.equal(hub.seen[0].headers['x-hub-fresh'], '1');
    assert.equal(hub.seen[0].headers['x-hub-actor'], 'user:u1');
    await reviewKey(hub.fetch, locals(NOW - 1000), 'sha256:ab', 'revoked', NOW);
    assert.equal(hub.seen[1].method, 'PATCH');
    assert.equal(hub.seen[1].url, 'http://hub.test/api/mailbox/keys/sha256%3Aab');
    await retireLogin(hub.fetch, locals(NOW - 1000), 'bob', NOW);
    assert.equal(hub.seen[2].url, 'http://hub.test/api/people/logins/bob/retire');
    assert.equal((await bindLogin(hub.fetch, locals(NOW - 1000), 'none', NOW)).ok, false);
    assert.equal((await reviewKey(hub.fetch, locals(NOW - 1000), 'x', 'maybe', NOW)).ok, false);
    assert.equal(hub.seen.length, 3, 'bad input does not reach hub');
  } finally {
    restore();
  }
});

test('hub refusals in human words; empty 404 of an old hub = still rolling out', async () => {
  const restore = quiet();
  try {
    const answer = (status: number, body: string) => fakeHub(() => new Response(body, { status })).fetch;
    const fresh = locals(NOW - 1000);
    assert.deepEqual(await bindLogin(answer(404, ''), fresh, 'alice', NOW), { ok: false, status: 503, message: t('auth.logins.rollingOut') });
    assert.deepEqual(await reviewKey(answer(404, ''), fresh, 'sha256:ab', 'approved', NOW), { ok: false, status: 503, message: t('auth.logins.rollingOut') });
    const taken = await bindLogin(answer(409, '{"error":"handle_taken"}'), fresh, 'alice', NOW);
    assert.equal(!taken.ok && taken.message.startsWith('This person is already linked'), true);
    const notOwner = await reviewKey(answer(403, '{"error":"not_key_owner"}'), fresh, 'sha256:ab', 'approved', NOW);
    assert.equal(!notOwner.ok && notOwner.status, 403);
    assert.equal(!notOwner.ok && notOwner.message, "The key belongs to someone else: its owner approves and revokes it; someone else's key can only be revoked while it is pending and its owner has no link.");
    const stale = await reviewKey(answer(403, '{"error":"fresh_session_required"}'), fresh, 'sha256:ab', 'approved', NOW);
    assert.equal(!stale.ok && stale.relogin, true);
    const other = await bindLogin(answer(500, 'internal error'), fresh, 'alice', NOW);
    assert.equal(!other.ok && other.message, 'hub: 500 internal error');

    assert.equal(await listLogins(answer(404, '')), 'missing');
    assert.equal(await listLogins(answer(500, 'x')), 'failed');
    assert.equal(await myHandle(answer(404, ''), fresh), null, 'old hub: no login, page lives');
    const one = '[{"user_id":"u1","handle":"alice","actor":"user:u1","created_at":"2030-01-13T10:00:00Z"}]';
    assert.equal(await myHandle(answer(200, one), fresh), 'alice');
    assert.equal(await myHandle(answer(200, one), { user: null }), null);
  } finally {
    restore();
  }
});

test('re-sign-in: only a hub path; key fingerprint is 12 chars', () => {
  assert.equal(safeNext('/agents'), '/agents');
  assert.equal(safeNext('/people?x=1'), '/people?x=1');
  for (const bad of ['//evil.test', '/\\evil.test', 'https://evil.test', '', null, 'agents', '/a b']) assert.equal(safeNext(bad), '/', String(bad));
  assert.equal(reloginHref('/agents'), '/login/fresh?next=%2Fagents');
  assert.equal(fingerprint('sha256:' + '0123456789ab'.padEnd(64, 'f')), '0123456789ab');
  assert.equal(fingerprint('user:u1'), null);
});

test('Next: the person from the sign-in link first, otherwise by email', () => {
  const people = [
    { handle: 'alice', name: 'Alice', email: 'alice@example.org', aliases: [], is_agent: false, retired: false, tickets: 0, active: 0 }
  ] as Person[];
  assert.equal(personFor(user, people, 'bob'), 'bob');
  assert.equal(personFor(user, people, null), 'alice');
  assert.equal(personFor(user, [], 'bob'), 'bob', 'people list failed, login still works');
  assert.equal(personFor(null, people, 'bob'), null, 'no session, no person');
});
