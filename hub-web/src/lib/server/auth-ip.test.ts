// Client IP for the Better Auth rate limit: node --test, no build (pnpm run test).
// Checks the real Better Auth getIP function with settings from authOptions.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { authOptions, envFlag } from '#lib/server/auth-config';

const ip = (await import(createRequire(import.meta.resolve('better-auth')).resolve('@better-auth/core/utils/ip'))) as {
  getIP: (req: { headers: Headers }, options: unknown) => string | null;
};

test('X-Forwarded-For is parsed from the right: a forged left entry and cluster proxies are ignored', async () => {
  const options = authOptions({ baseURL: 'https://hub.test', secret: 'x'.repeat(32), databaseUrl: 'postgres://u@127.0.0.1:1/db' });
  const got = (xff: string | null) => ip.getIP({ headers: new Headers(xff === null ? {} : { 'x-forwarded-for': xff }) }, options);
  assert.equal(got('203.0.113.7'), '203.0.113.7');
  assert.equal(got('6.6.6.6, 203.0.113.7'), '203.0.113.7');
  assert.equal(got('100.64.1.2'), '100.64.1.2');
  assert.equal(got('203.0.113.7, 10.42.4.124'), '203.0.113.7');
  assert.equal(got('10.42.4.124'), null);
  assert.equal(got(null), null);
  await options.database.end();
});

test('TRUSTED_PROXY_CIDRS: other proxy networks come from the setting', async () => {
  const options = authOptions({
    baseURL: 'https://hub.test',
    secret: 'x'.repeat(32),
    databaseUrl: 'postgres://u@127.0.0.1:1/db',
    trustedProxies: ['192.168.7.0/24']
  });
  const got = (xff: string) => ip.getIP({ headers: new Headers({ 'x-forwarded-for': xff }) }, options);
  assert.equal(got('203.0.113.7, 192.168.7.9'), '203.0.113.7');
  assert.equal(got('203.0.113.7, 10.42.4.124'), '10.42.4.124');
  await options.database.end();
});

test('authOptions: app name and open MCP client registration by default as today', async () => {
  const base = { baseURL: 'https://hub.test', secret: 'x'.repeat(32), databaseUrl: 'postgres://u@127.0.0.1:1/db' };
  const mcpOptions = (o: ReturnType<typeof authOptions>) =>
    (o.plugins.find((p) => p.id === 'oauth-provider') as unknown as { options: { allowUnauthenticatedClientRegistration?: boolean } }).options;
  const def = authOptions(base);
  assert.equal(def.appName, 'factory hub');
  assert.deepEqual(def.trustedOrigins, ['https://hub.test']);
  assert.equal(mcpOptions(def).allowUnauthenticatedClientRegistration, true);
  const closed = authOptions({ ...base, appName: 'demo hub', openClientRegistration: false });
  assert.equal(closed.appName, 'demo hub');
  assert.equal(mcpOptions(closed).allowUnauthenticatedClientRegistration, false);
  await def.database.end();
  await closed.database.end();
});

test('envFlag: MCP_OPEN_CLIENT_REGISTRATION defaults to true, false and 0 turn it off', () => {
  assert.equal(envFlag(undefined, true), true);
  assert.equal(envFlag('', true), true);
  assert.equal(envFlag('false', true), false);
  assert.equal(envFlag('0', true), false);
  assert.equal(envFlag('true', false), true);
  assert.throws(() => envFlag('yes please', true), /true or false/);
});
