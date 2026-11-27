// hub-web identity providers (step 2.8): who signed in to the hub. No password and no sign-up; the identity comes from one of:
// - oidc (oidc.ts): sign-in at an external OpenID Connect provider via Better Auth genericOAuth, with a redirect to it;
// - cloudflare-access (cloudflare-access.ts): a Cloudflare Access JWT checked by signature, issuer and audience;
// - tailscale (tailscale.ts): Tailscale proxy headers;
// - dev (dev.ts): a fixed login for local runs, refused in production.
// The last three take the identity from headers of the request sent by the entrance in front of the hub (edgeLogin
// plugin below). Each such entrance is trusted only on its own hosts (Host header, not ORIGIN): anyone could send the
// headers to another host. Trust boundary: a pod that reaches hub-web directly (e.g. a neighbor pod in the same
// namespace, if network policy lets it) can name the right host itself and send Tailscale headers.
//
// Settings come from env, names AUTH_*.
import { BlockList, isIP } from 'node:net';
import type { BetterAuthPlugin } from 'better-auth';
import { APIError, createAuthEndpoint } from 'better-auth/api';
import { setSessionCookie } from 'better-auth/cookies';
import type { JWTVerifyGetKey } from 'jose';
import * as z from 'zod/v4';
import { t } from '#lib/i18n/index';
import { cloudflareAccessProvider, cloudflareAccessSettings, type CloudflareAccessSettings } from '#lib/server/identity/cloudflare-access';
import { devProvider, devSettings, type DevSettings } from '#lib/server/identity/dev';
import { oidcSettings, type OidcSettings } from '#lib/server/identity/oidc';
import { tailscaleProvider, tailscaleSettings, type TailscaleSettings } from '#lib/server/identity/tailscale';

export type ProviderId = 'oidc' | 'cloudflare-access' | 'tailscale' | 'dev';

export type Identity = {
  email: string;
  name: string;
  /** the name came from the entrance and should be updated */
  named: boolean;
  provider: ProviderId;
};

/** A provider that recognizes a person by request headers from the entrance in front of the hub. */
export interface EdgeProvider {
  id: Exclude<ProviderId, 'oidc'>;
  /** Hosts on which the provider is trusted; null: any host (dev only, and only outside production). */
  hosts: readonly string[] | null;
  identify(headers: Headers): Promise<Identity | null>;
}

export type IdentityConfig = {
  tailscale?: TailscaleSettings;
  cloudflareAccess?: CloudflareAccessSettings;
  dev?: DevSettings;
  oidc?: OidcSettings;
  /** Other addresses of the same hub (besides ORIGIN): forms and Better Auth calls with cookies are trusted from them. */
  extraOrigins?: string[];
};

export type Env = Record<string, string | undefined>;

/** Trimmed env value, undefined when empty. */
export function envValue(env: Env, name: string): string | undefined {
  return env[name]?.trim() || undefined;
}

export function list(v: string | undefined): string[] {
  return (v ?? '').split(',').map((x) => x.trim()).filter(Boolean);
}

/** Host list without scheme and port, lowercase. */
export function hostList(v: string | undefined): string[] {
  return list(v).map((x) => x.toLowerCase());
}

/** Request host without port, as the entrance sent it. */
export function requestHost(headers: Headers): string {
  return (headers.get('host') ?? '').toLowerCase().replace(/:\d+$/, '');
}

export function onHost(headers: Headers, hosts: readonly string[] | null): boolean {
  return hosts === null || hosts.includes(requestHost(headers));
}

export function asIdentity(login: string | undefined, name: string, named: boolean, provider: ProviderId): Identity | null {
  login = login?.trim();
  if (!login || login.length > 200 || !login.includes('@')) return null;
  return { email: login.toLowerCase(), name: (name.trim() || login.split('@')[0]).slice(0, 200), named, provider };
}

/** Settings of all providers from env; baseURL (ORIGIN) gives the default Cloudflare host. */
export function readIdentityConfig(env: Env, baseURL: string): IdentityConfig {
  const c: IdentityConfig = { extraOrigins: list(envValue(env, 'AUTH_EXTRA_ORIGINS')) };
  const tailscale = tailscaleSettings(env);
  if (tailscale) c.tailscale = tailscale;
  const cloudflareAccess = cloudflareAccessSettings(env, baseURL);
  if (cloudflareAccess) c.cloudflareAccess = cloudflareAccess;
  const dev = devSettings(env);
  if (dev) c.dev = dev;
  const oidc = oidcSettings(env);
  if (oidc) c.oidc = oidc;
  return c;
}

/**
 * Header entrances in check order: tailscale, cloudflare-access, dev. nodeEnv is for dev (refused in production),
 * jwks replaces the Cloudflare keys in tests.
 */
export function edgeProviders(c: IdentityConfig, o: { nodeEnv?: string; jwks?: JWTVerifyGetKey } = {}): EdgeProvider[] {
  const out: EdgeProvider[] = [];
  if (c.tailscale) out.push(tailscaleProvider(c.tailscale));
  if (c.cloudflareAccess) out.push(cloudflareAccessProvider(c.cloudflareAccess, o.jwks));
  if (c.dev) out.push(devProvider(c.dev, o.nodeEnv));
  return out;
}

/**
 * Identity from the first provider whose host matched. A provider with its own hosts decides for its host alone: if it
 * recognized nobody (no headers, JWT failed the check), the answer is null, not the next provider's sign-in, the
 * development login included. The development login (hosts: null) works only on hosts with no other entrance.
 */
export async function edgeIdentity(headers: Headers, providers: readonly EdgeProvider[]): Promise<Identity | null> {
  for (const p of providers) {
    if (!onHost(headers, p.hosts)) continue;
    return p.identify(headers);
  }
  return null;
}

/** ORIGIN, explicit other addresses and https addresses of entrance hosts, without duplicates. */
export function trustedOrigins(baseURL: string, c: IdentityConfig): string[] {
  const hosts = [...(c.tailscale?.hosts ?? []), ...(c.cloudflareAccess?.hosts ?? [])];
  return [...new Set([baseURL, ...(c.extraOrigins ?? []), ...hosts.map((x) => `https://${x}`)])];
}

/** How to sign in to the hub, for the sign-in pages: full text and a short list. Order as in the old text. */
export function entrances(c: IdentityConfig): { full: string; short: string } {
  const full: string[] = [];
  const short: string[] = [];
  for (const host of c.cloudflareAccess?.hosts ?? []) full.push(t('auth.entrance.cloudflare', { host }));
  if (c.cloudflareAccess) short.push('Cloudflare Access');
  if (c.tailscale) {
    full.push(t('auth.entrance.tailnet'));
    short.push('tailnet');
  }
  if (c.oidc) {
    full.push(t('auth.entrance.oidc', { name: c.oidc.name }));
    short.push(c.oidc.name);
  }
  if (c.dev) {
    full.push(t('auth.entrance.dev', { login: c.dev.login }));
    short.push(t('auth.entrance.devShort'));
  }
  const or = t('auth.entrance.or');
  return { full: full.join(or) || t('auth.entrance.none'), short: short.join(or) || t('auth.entrance.noneShort') };
}

/** k3s pod and service networks: addresses of proxies in front of hub-web, not of clients. */
export const DEFAULT_TRUSTED_PROXY_CIDRS = ['10.42.0.0/16', '10.43.0.0/16'];

/** TRUSTED_PROXY_CIDRS (comma-separated), otherwise the k3s networks. A bad entry stops startup instead of being silently dropped. */
export function trustedProxyCidrs(env: Env): string[] {
  const got = list(env.TRUSTED_PROXY_CIDRS);
  if (got.length === 0) return [...DEFAULT_TRUSTED_PROXY_CIDRS];
  for (const c of got) {
    const [addr, bits, ...rest] = c.split('/');
    const v = isIP(addr ?? '');
    const n = Number(bits);
    if (!v || rest.length || !/^\d+$/.test(bits ?? '') || n > (v === 4 ? 32 : 128)) throw new Error(`TRUSTED_PROXY_CIDRS: ${c} is not a CIDR`);
  }
  return got;
}

/** Checks an address against a CIDR list (address inside the proxy networks). */
export function addressMatcher(cidrs: readonly string[]): (ip: string) => boolean {
  const block = new BlockList();
  for (const c of cidrs) {
    const [addr, bits] = c.split('/');
    block.addSubnet(addr, Number(bits), isIP(addr) === 6 ? 'ipv6' : 'ipv4');
  }
  return (ip) => {
    const v = isIP(ip);
    return v !== 0 && block.check(ip, v === 6 ? 'ipv6' : 'ipv4');
  };
}

/**
 * Better Auth plugin: sign-in by the identity from the entrance in front of the hub, path /sign-in/edge.
 * oauth_query: the signed authorization request the OAuth
 * provider sent to /login with. The provider hook parses it: after setting the session it continues the authorization
 * itself and answers { redirect, url } instead of the body below.
 */
export function edgeLogin(providers: readonly EdgeProvider[]) {
  const signInEdge = createAuthEndpoint(
    '/sign-in/edge',
    { method: 'POST', requireHeaders: true, body: z.object({ oauth_query: z.string().max(8000).optional() }) },
    async (ctx) => {
      const who = await edgeIdentity(ctx.headers!, providers);
      if (!who) throw new APIError('UNAUTHORIZED', { message: 'No identity from the hub entrance (tailnet user device or Cloudflare Access)' });
      const adapter = ctx.context.internalAdapter;
      const found = await adapter.findUserByEmail(who.email);
      let user = found?.user;
      if (!user) {
        user = await adapter.createUser({ email: who.email, name: who.name, emailVerified: true }, { method: who.provider });
      } else if (who.named && user.name !== who.name) {
        user = (await adapter.updateUser(user.id, { name: who.name })) ?? user;
      }
      const session = await adapter.createSession(user.id);
      if (!session) throw new APIError('INTERNAL_SERVER_ERROR', { message: 'session was not created' });
      await setSessionCookie(ctx, { session, user });
      return ctx.json({ user: { id: user.id, email: user.email, name: user.name } });
    }
  );
  return { id: 'edge', endpoints: { signInEdge } } satisfies BetterAuthPlugin;
}
