// Identity providers (step 2.8): node --test, no build (pnpm run test). Each entrance in front of the hub is trusted only
// on its own host, the Cloudflare Access JWT is checked by signature, issuer and audience, the dev login is refused in production.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createLocalJWKSet, exportJWK, generateKeyPair, SignJWT } from 'jose';
import { setLocale, translate } from '#lib/i18n/index';
import {
  addressMatcher,
  DEFAULT_TRUSTED_PROXY_CIDRS,
  edgeIdentity,
  edgeProviders,
  entrances,
  readIdentityConfig,
  trustedOrigins,
  trustedProxyCidrs
} from '#lib/server/identity/index';
import { tailscaleProvider } from '#lib/server/identity/tailscale';
import { cloudflareAccessProvider } from '#lib/server/identity/cloudflare-access';
import { devProvider, devSettings } from '#lib/server/identity/dev';
import { oidcSettings } from '#lib/server/identity/oidc';

const BASE = 'https://hub.example';
const TS_HOST = 'hub.tail.example';

const h = (o: Record<string, string>) => new Headers(o);

test('settings come from AUTH_* names; the old names are ignored', () => {
  const fresh = readIdentityConfig(
    {
      AUTH_TAILSCALE_HOSTS: TS_HOST,
      AUTH_CF_ACCESS_TEAM_DOMAIN: 'team.cloudflareaccess.com',
      AUTH_CF_ACCESS_AUD: 'aud-1',
      AUTH_EXTRA_ORIGINS: `https://${TS_HOST}`
    },
    BASE
  );
  const old = readIdentityConfig(
    {
      HUB_TAILNET_HOST: TS_HOST,
      CF_ACCESS_TEAM_DOMAIN: 'team.cloudflareaccess.com',
      CF_ACCESS_AUD: 'aud-1',
      HUB_EXTRA_ORIGINS: `https://${TS_HOST}`
    },
    BASE
  );
  assert.deepEqual(old, { extraOrigins: [] });
  assert.deepEqual(fresh.tailscale, { hosts: [TS_HOST] });
  // Cloudflare is bound to the ORIGIN host unless hosts are set explicitly.
  assert.deepEqual(fresh.cloudflareAccess, { teamDomain: 'team.cloudflareaccess.com', aud: 'aud-1', hosts: ['hub.example'] });
  assert.equal(fresh.oidc, undefined);
  assert.deepEqual(readIdentityConfig({ AUTH_DEV_LOGIN: 'dev@local' }, BASE).dev, { login: 'dev@local' });

  const both = readIdentityConfig({ AUTH_TAILSCALE_HOSTS: 'a.example, B.example' }, BASE);
  assert.deepEqual(both.tailscale, { hosts: ['a.example', 'b.example'] });
  const cfHosts = readIdentityConfig(
    { AUTH_CF_ACCESS_TEAM_DOMAIN: 't.cloudflareaccess.com', AUTH_CF_ACCESS_AUD: 'a', AUTH_CF_ACCESS_HOSTS: 'cf.example' },
    BASE
  );
  assert.deepEqual(cfHosts.cloudflareAccess?.hosts, ['cf.example']);
  // Without audience or team domain the Cloudflare entrance is off.
  assert.equal(readIdentityConfig({ AUTH_CF_ACCESS_TEAM_DOMAIN: 't.cloudflareaccess.com' }, BASE).cloudflareAccess, undefined);
  assert.deepEqual(readIdentityConfig({}, BASE), { extraOrigins: [] });
});

test('trusted origins: ORIGIN, explicit ones and entrance hosts; the prod set matches today', () => {
  const c = readIdentityConfig({ AUTH_TAILSCALE_HOSTS: TS_HOST, AUTH_EXTRA_ORIGINS: `https://${TS_HOST}` }, BASE);
  assert.deepEqual(trustedOrigins(BASE, c), [BASE, `https://${TS_HOST}`]);
  const derived = readIdentityConfig({ AUTH_TAILSCALE_HOSTS: TS_HOST }, BASE);
  assert.deepEqual(trustedOrigins(BASE, derived), [BASE, `https://${TS_HOST}`]);
});

test('tailscale: headers are trusted only on the configured host', async () => {
  const p = tailscaleProvider({ hosts: [TS_HOST] });
  const who = await p.identify(h({ host: TS_HOST, 'tailscale-user-login': 'Alice@Example.org', 'tailscale-user-name': '=?utf-8?q?Zo=C3=AB?=' }));
  assert.deepEqual(who, { email: 'alice@example.org', name: 'Zo\u00eb', named: true, provider: 'tailscale' });
  assert.equal((await p.identify(h({ host: `${TS_HOST}:443`, 'tailscale-user-login': 'a@example.org' })))?.email, 'a@example.org');
  // Foreign host: anyone could have sent the headers.
  assert.equal(await p.identify(h({ host: 'hub.example', 'tailscale-user-login': 'a@example.org' })), null);
  assert.equal(await p.identify(h({ 'tailscale-user-login': 'a@example.org' })), null);
  // A tagged device comes without a login.
  assert.equal(await p.identify(h({ host: TS_HOST })), null);
  assert.throws(() => tailscaleProvider({ hosts: [] }), /host/);
});

async function cfFixture() {
  const { privateKey, publicKey } = await generateKeyPair('RS256');
  const jwk = { ...(await exportJWK(publicKey)), kid: 'k1', alg: 'RS256' };
  const jwks = createLocalJWKSet({ keys: [jwk] });
  const sign = (claims: Record<string, unknown>, o: { iss?: string; aud?: string; key?: CryptoKey; exp?: string } = {}) =>
    new SignJWT(claims)
      .setProtectedHeader({ alg: 'RS256', kid: 'k1' })
      .setIssuer(o.iss ?? 'https://team.cloudflareaccess.com')
      .setAudience(o.aud ?? 'aud-1')
      .setIssuedAt()
      .setExpirationTime(o.exp ?? '5m')
      .sign(o.key ?? privateKey);
  return { jwks, sign };
}

test('cloudflare-access: the JWT is checked by signature, issuer and audience, and only on its own host', async () => {
  const { jwks, sign } = await cfFixture();
  const p = cloudflareAccessProvider({ teamDomain: 'team.cloudflareaccess.com', aud: 'aud-1', hosts: ['hub.example'] }, jwks);
  const ok = await sign({ email: 'Bob@Example.org' });
  assert.deepEqual(await p.identify(h({ host: 'hub.example', 'cf-access-jwt-assertion': ok })), {
    email: 'bob@example.org',
    name: 'Bob',
    named: false,
    provider: 'cloudflare-access'
  });
  // The same valid JWT on a foreign host is rejected.
  assert.equal(await p.identify(h({ host: TS_HOST, 'cf-access-jwt-assertion': ok })), null);
  assert.equal(await p.identify(h({ 'cf-access-jwt-assertion': ok })), null);
  // Foreign audience, foreign issuer, foreign signature, expired token.
  for (const bad of [
    await sign({ email: 'b@example.org' }, { aud: 'other' }),
    await sign({ email: 'b@example.org' }, { iss: 'https://evil.cloudflareaccess.com' }),
    await sign({ email: 'b@example.org' }, { key: (await generateKeyPair('RS256')).privateKey }),
    await sign({ email: 'b@example.org' }, { exp: '-1m' })
  ]) {
    assert.equal(await p.identify(h({ host: 'hub.example', 'cf-access-jwt-assertion': bad })), null);
  }
  // An Access service token without email gives no identity.
  assert.equal(await p.identify(h({ host: 'hub.example', 'cf-access-jwt-assertion': await sign({}) })), null);
  assert.throws(() => cloudflareAccessProvider({ teamDomain: 't', aud: 'a', hosts: [] }, jwks), /host/);
});

test('dev: the development login is refused with NODE_ENV=production', async () => {
  assert.throws(() => devSettings({ AUTH_DEV_LOGIN: 'dev@local', NODE_ENV: 'production' }), /production/);
  assert.throws(() => readIdentityConfig({ AUTH_DEV_LOGIN: 'dev@local', NODE_ENV: 'production' }, BASE), /production/);
  assert.throws(() => devProvider({ login: 'dev@local' }, 'production'), /production/);
  assert.equal(devSettings({ NODE_ENV: 'production' }), undefined);
  assert.deepEqual(devSettings({ AUTH_DEV_LOGIN: 'dev@local', NODE_ENV: 'development' }), { login: 'dev@local' });
  const p = devProvider({ login: 'dev@local' }, undefined);
  assert.deepEqual(await p.identify(h({})), { email: 'dev@local', name: 'dev', named: false, provider: 'dev' });
});

test('edgeIdentity: sign-in on the tailnet host does not fall back to Cloudflare or dev, as today', async () => {
  const { jwks, sign } = await cfFixture();
  const providers = edgeProviders(
    {
      tailscale: { hosts: [TS_HOST] },
      cloudflareAccess: { teamDomain: 'team.cloudflareaccess.com', aud: 'aud-1', hosts: ['hub.example'] },
      extraOrigins: []
    },
    { nodeEnv: 'production', jwks }
  );
  assert.deepEqual(providers.map((p) => p.id), ['tailscale', 'cloudflare-access']);
  const token = await sign({ email: 'c@example.org' });
  assert.equal((await edgeIdentity(h({ host: 'hub.example', 'cf-access-jwt-assertion': token }), providers))?.provider, 'cloudflare-access');
  assert.equal((await edgeIdentity(h({ host: TS_HOST, 'tailscale-user-login': 't@example.org' }), providers))?.provider, 'tailscale');
  // Tailscale headers on the Cloudflare host give nobody.
  assert.equal(await edgeIdentity(h({ host: 'hub.example', 'tailscale-user-login': 't@example.org' }), providers), null);
  assert.equal(await edgeIdentity(h({ host: TS_HOST, 'cf-access-jwt-assertion': token }), providers), null);
  const dev = edgeProviders({ dev: { login: 'dev@local' }, extraOrigins: [] }, { nodeEnv: 'development' });
  assert.equal((await edgeIdentity(h({ host: 'localhost' }), dev))?.email, 'dev@local');
  // With a dev login, a failed sign-in on the tailnet or Cloudflare host does not turn into dev: the entrance host decides alone.
  const all = edgeProviders(
    {
      tailscale: { hosts: [TS_HOST] },
      cloudflareAccess: { teamDomain: 'team.cloudflareaccess.com', aud: 'aud-1', hosts: ['hub.example'] },
      dev: { login: 'dev@local' },
      extraOrigins: []
    },
    { nodeEnv: 'development', jwks }
  );
  assert.equal(await edgeIdentity(h({ host: TS_HOST }), all), null);
  assert.equal(await edgeIdentity(h({ host: 'hub.example', 'cf-access-jwt-assertion': 'x.y.z' }), all), null);
  assert.equal(await edgeIdentity(h({ host: 'hub.example' }), all), null);
  assert.equal((await edgeIdentity(h({ host: 'localhost' }), all))?.provider, 'dev');
});

test('entrances: sign-in text for pages from the settings, the prod text in ru is assembled from the same messages', () => {
  const c = readIdentityConfig({ AUTH_TAILSCALE_HOSTS: TS_HOST, AUTH_CF_ACCESS_TEAM_DOMAIN: 't.cloudflareaccess.com', AUTH_CF_ACCESS_AUD: 'a' }, BASE);
  try {
    setLocale('en');
    assert.deepEqual(entrances(c), {
      full: 'a Cloudflare Access account (https://hub.example) or tailnet (from a user device, not a tagged one)',
      short: 'Cloudflare Access or tailnet'
    });
    setLocale('ru');
    const or = translate('ru', 'auth.entrance.or');
    assert.deepEqual(entrances(c), {
      full: translate('ru', 'auth.entrance.cloudflare', { host: 'hub.example' }) + or + translate('ru', 'auth.entrance.tailnet'),
      short: `Cloudflare Access${or}tailnet`
    });
    setLocale('en');
    assert.deepEqual(entrances(readIdentityConfig({}, BASE)), { full: 'an entrance that is not configured', short: 'no entrance configured' });
  } finally {
    setLocale('en');
  }
});

test('oidc: off until issuer, client and secret are set', () => {
  assert.equal(oidcSettings({ AUTH_OIDC_ISSUER: 'https://idp.example' }), undefined);
  assert.equal(oidcSettings({ AUTH_OIDC_ISSUER: 'https://idp.example', AUTH_OIDC_CLIENT_ID: 'c' }), undefined);
  assert.deepEqual(oidcSettings({ AUTH_OIDC_ISSUER: 'https://idp.example/', AUTH_OIDC_CLIENT_ID: 'c', AUTH_OIDC_CLIENT_SECRET: 's' }), {
    issuer: 'https://idp.example',
    clientId: 'c',
    clientSecret: 's',
    scopes: ['openid', 'profile', 'email'],
    name: 'OIDC'
  });
  assert.deepEqual(
    oidcSettings({ AUTH_OIDC_ISSUER: 'https://idp.example', AUTH_OIDC_CLIENT_ID: 'c', AUTH_OIDC_CLIENT_SECRET: 's', AUTH_OIDC_SCOPES: 'openid email', AUTH_OIDC_NAME: 'Keycloak' })?.scopes,
    ['openid', 'email']
  );
  assert.throws(() => oidcSettings({ AUTH_OIDC_ISSUER: 'http://idp.example', AUTH_OIDC_CLIENT_ID: 'c', AUTH_OIDC_CLIENT_SECRET: 's' }), /https/);
});

test('TRUSTED_PROXY_CIDRS: k3s networks by default, addresses are checked by CIDR', () => {
  assert.deepEqual(DEFAULT_TRUSTED_PROXY_CIDRS, ['10.42.0.0/16', '10.43.0.0/16']);
  assert.deepEqual(trustedProxyCidrs({}), DEFAULT_TRUSTED_PROXY_CIDRS);
  assert.deepEqual(trustedProxyCidrs({ TRUSTED_PROXY_CIDRS: '192.168.0.0/24, fd00::/8' }), ['192.168.0.0/24', 'fd00::/8']);
  assert.throws(() => trustedProxyCidrs({ TRUSTED_PROXY_CIDRS: '10.0.0.0' }), /CIDR/);
  const internal = addressMatcher(DEFAULT_TRUSTED_PROXY_CIDRS);
  assert.equal(internal('10.42.4.124'), true);
  assert.equal(internal('10.43.0.1'), true);
  assert.equal(internal('10.44.0.1'), false);
  assert.equal(internal('203.0.113.7'), false);
  assert.equal(internal('garbage'), false);
  assert.equal(addressMatcher(['fd00::/8'])('fd00::1'), true);
});
