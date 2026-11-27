import { sveltekitCookies } from 'better-auth/svelte-kit';
import { building } from '$app/env';
import { getRequestEvent } from '$app/server';
import { createAuth, envFlag } from './auth-config';
import { readIdentityConfig, trustedProxyCidrs } from './identity/index';

// During the build SvelteKit loads server modules without the environment: placeholders are used then
// (the pg pool does not connect until asked).
const BUILD_PLACEHOLDER: Record<string, string> = {
  ORIGIN: 'http://localhost:8001',
  BETTER_AUTH_SECRET: 'build-time-placeholder-not-a-secret-000000',
  HUBDB_URI: 'postgres://build@localhost/build'
};

function need(name: string): string {
  const v = process.env[name] || (building ? BUILD_PLACEHOLDER[name] : undefined);
  if (!v) throw new Error(`${name} is not set`);
  return v;
}

// The external hub address: ORIGIN (the same as adapter-node's). BETTER_AUTH_SECRET and HUBDB_URI come from
// secrets: in the cluster ExternalSecret hub-web-auth (ESO generator) and hub-db-hub-web (role hub_web), locally from AppHost.
// Identity providers read env themselves (identity/index.ts): the chart sets AUTH_EXTRA_ORIGINS and AUTH_TAILSCALE_HOSTS,
// AUTH_CF_ACCESS_* come from ExternalSecret hub-web-access.
const baseURL = need('ORIGIN');
const env: Record<string, string | undefined> = process.env;

export const identity = readIdentityConfig(env, baseURL);

/** Hub name on pages (PUBLIC_HUB_NAME), factory hub by default. */
export const hubName = env.PUBLIC_HUB_NAME?.trim() || 'factory hub';

export const auth = createAuth({
  baseURL,
  secret: need('BETTER_AUTH_SECRET'),
  databaseUrl: need('HUBDB_URI'),
  identity,
  nodeEnv: env.NODE_ENV,
  appName: env.HUB_APP_NAME?.trim() || hubName,
  trustedProxies: trustedProxyCidrs(env),
  openClientRegistration: envFlag(env.MCP_OPEN_CLIENT_REGISTRATION, true),
  plugins: [sveltekitCookies(getRequestEvent)]
});

export type Session = typeof auth.$Infer.Session;
