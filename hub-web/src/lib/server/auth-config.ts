// Better Auth configuration without SvelteKit imports: read both by the server (auth.ts) and by the `auth generate` CLI,
// which produces the table SQL (services/hub/migrations/0007_auth.sql). When the config changes, regenerate the SQL.
import { betterAuth, type BetterAuthOptions } from 'better-auth';
import { jwt } from 'better-auth/plugins';
import { mcp } from '@better-auth/mcp';
import { Pool } from 'pg';
import { DEFAULT_TRUSTED_PROXY_CIDRS, edgeLogin, edgeProviders, trustedOrigins, type IdentityConfig } from '#lib/server/identity/index';
import { oidcPlugin } from '#lib/server/identity/oidc';

export type AuthSettings = {
  baseURL: string;
  secret: string;
  databaseUrl: string;
  /** Identity providers (identity/index.ts) and other hub addresses; without them there is no way to sign in. */
  identity?: IdentityConfig;
  /** NODE_ENV for the dev provider: it is refused in production. */
  nodeEnv?: string;
  /** Better Auth app name (HUB_APP_NAME), factory hub by default. */
  appName?: string;
  /** Proxy networks in front of hub-web (TRUSTED_PROXY_CIDRS), k3s networks by default. */
  trustedProxies?: string[];
  /** MCP client registration without sign-in (MCP_OPEN_CLIENT_REGISTRATION), open by default. */
  openClientRegistration?: boolean;
  plugins?: BetterAuthOptions['plugins'];
};

/** Env flag: empty gives the fallback, true/1 and false/0 are explicit, anything else stops startup. */
export function envFlag(v: string | undefined, fallback: boolean): boolean {
  const t = v?.trim().toLowerCase();
  if (!t) return fallback;
  if (t === 'true' || t === '1') return true;
  if (t === 'false' || t === '0') return false;
  throw new Error(`expected true or false, got ${v}`);
}

export function authOptions(s: AuthSettings) {
  return {
    appName: s.appName ?? 'factory hub',
    baseURL: s.baseURL,
    secret: s.secret,
    trustedOrigins: trustedOrigins(s.baseURL, s.identity ?? {}),
    // Better Auth tables live in the auth schema of the same hub-db database; a hub migration creates the schema.
    database: new Pool({ connectionString: s.databaseUrl, options: '-c search_path=auth', max: 5 }),
    // Sign-in only via identity providers (identity/): no passwords and no sign-up.
    emailAndPassword: { enabled: false },
    session: { expiresIn: 60 * 60 * 24 * 30, updateAge: 60 * 60 * 24, cookieCache: { enabled: true, maxAge: 300 } },
    telemetry: { enabled: false },
    // Hub migrations own the schema (0007_auth.sql). The runtime check remembers a mismatch forever:
    // a pod started before the migration would stay broken. `auth generate` checks the match on upgrade.
    // The Better Auth rate limit uses the client IP. Entrances in front of the hub (cloudflared, the
    // Tailscale proxy) append the real client address to X-Forwarded-For; the chain is parsed from the right up to the
    // first address outside the proxy networks (TRUSTED_PROXY_CIDRS), so a left address forged by the client is ignored.
    // Without this all clients shared one bucket per path.
    advanced: {
      database: { validateSchema: false },
      ipAddress: { ipAddressHeaders: ['x-forwarded-for'], trustedProxies: s.trustedProxies ?? DEFAULT_TRUSTED_PROXY_CIDRS }
    },
    plugins: [
      jwt(),
      mcp({
        loginPage: '/login',
        consentPage: '/consent',
        resource: `${s.baseURL}/mcp`,
        // Claude Code and other MCP clients register themselves (RFC 7591). By default registration is open from outside too:
        // a client without a user gets nothing, a token is issued only after sign-in and consent.
        allowDynamicClientRegistration: true,
        allowUnauthenticatedClientRegistration: s.openClientRegistration ?? true,
        // Claude Code registers with redirect http://localhost:<port>/callback and without application_type.
        // By default such a client counts as web, and http loopback is forbidden for it; infer makes it native.
        clientRegistrationDefaultApplicationType: 'infer',
        scopes: ['openid', 'profile', 'email', 'offline_access'],
      }),
      edgeLogin(edgeProviders(s.identity ?? {}, { nodeEnv: s.nodeEnv })),
      ...(s.identity?.oidc ? [oidcPlugin(s.identity.oidc)] : []),
      ...(s.plugins ?? []),
    ],
  } satisfies BetterAuthOptions;
}

export function createAuth(s: AuthSettings) {
  return betterAuth(authOptions(s));
}
