// Better Auth on a real Postgres as role hub_web: sign-in, session, JWT keys and MCP client registration
// write and read the auth schema, while hub tables are closed to the role. tools/test-graph-pg.sh provides the database
// (HUB_TEST_AUTH_DB_URI: role hub_web with a password, migrations applied); without the variable the test is skipped
// (pnpm run test).
import { createServer } from 'node:http';
import type { AddressInfo } from 'node:net';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { betterAuth } from 'better-auth';
import { exportJWK, generateKeyPair } from 'jose';
import pg from 'pg';
import { authOptions } from '#lib/server/auth-config';

/** Fake OIDC provider: only discovery and keys, the test does not reach the code exchange. */
async function fakeIdp() {
  const jwk = { ...(await exportJWK((await generateKeyPair('RS256')).publicKey)), kid: 'k1', alg: 'RS256' };
  const server = createServer((req, res) => {
    const base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
    const body =
      req.url === '/.well-known/openid-configuration'
        ? {
            issuer: base,
            authorization_endpoint: `${base}/authorize`,
            token_endpoint: `${base}/token`,
            userinfo_endpoint: `${base}/userinfo`,
            jwks_uri: `${base}/jwks`,
            id_token_signing_alg_values_supported: ['RS256']
          }
        : req.url === '/jwks'
          ? { keys: [jwk] }
          : null;
    res.writeHead(body ? 200 : 404, { 'content-type': 'application/json' }).end(body ? JSON.stringify(body) : '');
  });
  await new Promise<void>((ok) => server.listen(0, '127.0.0.1', ok));
  return { issuer: `http://127.0.0.1:${(server.address() as AddressInfo).port}`, close: () => server.close() };
}

const uri = process.env.HUB_TEST_AUTH_DB_URI;
const BASE = 'http://localhost:8001';
const EMAIL = `dev15k-${process.pid}@test`;

test('Better Auth as hub_web: the auth schema works, public.tickets is closed', { skip: !uri && 'no HUB_TEST_AUTH_DB_URI' }, async () => {
  const opts = authOptions({
    baseURL: BASE,
    secret: 'test-only-not-a-secret-0000000000000000000000000',
    databaseUrl: uri!,
    identity: { dev: { login: EMAIL }, extraOrigins: [] }
  });
  const auth = betterAuth(opts);
  const db = new pg.Pool({ connectionString: uri, max: 1 });
  const req = (path: string, init: RequestInit = {}) =>
    auth.handler(new Request(`${BASE}/api/auth${path}`, { ...init, headers: { origin: BASE, ...init.headers } }));
  let clientId: string | undefined;
  try {
    const { rows: [who] } = await db.query('select current_user as u');
    assert.equal(who.u, 'hub_web');

    // Step 2.8: sign-in by the identity from the entrance in front of the hub via /sign-in/edge, its only path.
    const signIn = await req('/sign-in/edge', { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{}' });
    assert.equal(signIn.status, 200, await signIn.clone().text());
    const cookie = signIn.headers.getSetCookie().map((c) => c.split(';')[0]).join('; ');
    assert.ok(cookie.includes('session_token'), cookie);

    const session = await req('/get-session', { headers: { cookie } });
    assert.equal(session.status, 200);
    assert.equal((await session.json())?.user?.email, EMAIL);

    const jwks = await req('/jwks');
    assert.equal(jwks.status, 200);
    assert.ok((await jwks.json()).keys.length >= 1);

    const reg = await req('/oauth2/register', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ client_name: 'test 15k', redirect_uris: ['http://localhost:3999/callback'], token_endpoint_auth_method: 'none' })
    });
    const client = await reg.json();
    assert.ok(reg.status === 200 || reg.status === 201, JSON.stringify(client));
    clientId = client.client_id;
    assert.ok(clientId);

    await assert.rejects(
      db.query("insert into public.tickets(project, code, summary) values ('demo', 'x15k', 'x')"),
      (e: { code?: string }) => e.code === '42501'
    );
    await assert.rejects(db.query('select count(*) from public.tickets'), (e: { code?: string }) => e.code === '42501');

    // Step 2.8: the oidc provider (genericOAuth) takes addresses from discovery and sends the browser to the provider
    // with PKCE; the sign-in state is written to the auth schema under the same role.
    const idp = await fakeIdp();
    const oidcStarted = new Date(Date.now() - 1000);
    const oidcOpts = authOptions({
      baseURL: BASE,
      secret: 'test-only-not-a-secret-0000000000000000000000000',
      databaseUrl: uri!,
      identity: { oidc: { issuer: idp.issuer, clientId: 'hub-test', clientSecret: 's', scopes: ['openid', 'email'], name: 'Test IdP' } }
    });
    try {
      const oidcAuth = betterAuth(oidcOpts);
      const start = await oidcAuth.handler(
        new Request(`${BASE}/api/auth/sign-in/social`, {
          method: 'POST',
          headers: { origin: BASE, 'content-type': 'application/json' },
          body: JSON.stringify({ provider: 'oidc', callbackURL: '/', disableRedirect: true })
        })
      );
      const body = await start.json();
      assert.equal(start.status, 200, JSON.stringify(body));
      const to = new URL(body.url);
      assert.equal(`${to.origin}${to.pathname}`, `${idp.issuer}/authorize`);
      assert.equal(to.searchParams.get('client_id'), 'hub-test');
      assert.equal(to.searchParams.get('redirect_uri'), `${BASE}/api/auth/callback/oidc`);
      assert.equal(to.searchParams.get('code_challenge_method'), 'S256');
      // Without a hub-web session the header sign-in refuses: oidc does not enable dev.
      const edge = await oidcAuth.handler(
        new Request(`${BASE}/api/auth/sign-in/edge`, { method: 'POST', headers: { origin: BASE, 'content-type': 'application/json' }, body: '{}' })
      );
      assert.equal(edge.status, 401);
    } finally {
      idp.close();
      await oidcOpts.database.end();
      // The state of the started OIDC sign-in (state and code_verifier) is cleaned up right away.
      await db.query('delete from auth.verification where "createdAt" >= $1', [oidcStarted]);
    }
  } finally {
    // Cleanup under the same role: sessions go with the user (on delete cascade).
    if (clientId) await db.query('delete from auth."oauthClient" where "clientId" = $1', [clientId]);
    await db.query('delete from auth."user" where email = $1', [EMAIL]);
    await db.end();
    await opts.database.end();
  }
});
