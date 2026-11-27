// oidc entrance: an external OpenID Connect provider via the Better Auth genericOAuth plugin. The browser goes to the provider
// and returns to /api/auth/callback/oidc (the redirect address registered at the provider); the identity comes from a verified
// ID token (issuer and keys from discovery).
// Settings: AUTH_OIDC_ISSUER (https), AUTH_OIDC_CLIENT_ID, AUTH_OIDC_CLIENT_SECRET or AUTH_OIDC_CLIENT_SECRET_FILE,
// AUTH_OIDC_SCOPES (space or comma separated, openid profile email by default), AUTH_OIDC_NAME (name on pages).
// Without issuer, client and secret the entrance is off. A user is created on first sign-in, as with other entrances:
// the provider policy decides whom to admit. Better Auth links the OIDC account to an existing hub user by email if the
// provider verified it (email_verified). So only a single-owner provider the hub trusts fits: any of its users signs in
// to the hub, and with their email signs in as the existing user with that email.
import { readFileSync } from 'node:fs';
import { genericOAuth } from 'better-auth/plugins';
import { envValue, type Env } from '#lib/server/identity/index';

export const OIDC_PROVIDER_ID = 'oidc';

export type OidcSettings = { issuer: string; clientId: string; clientSecret: string; scopes: string[]; name: string };

export function oidcSettings(env: Env): OidcSettings | undefined {
  const issuer = envValue(env, 'AUTH_OIDC_ISSUER')?.replace(/\/+$/, '');
  const clientId = envValue(env, 'AUTH_OIDC_CLIENT_ID');
  const file = envValue(env, 'AUTH_OIDC_CLIENT_SECRET_FILE');
  const clientSecret = envValue(env, 'AUTH_OIDC_CLIENT_SECRET') ?? (file ? readFileSync(file, 'utf8').trim() : undefined);
  if (!issuer || !clientId || !clientSecret) return undefined;
  if (new URL(issuer).protocol !== 'https:') throw new Error('AUTH_OIDC_ISSUER must be an https URL');
  const scopes = (envValue(env, 'AUTH_OIDC_SCOPES') ?? 'openid profile email').split(/[\s,]+/).filter(Boolean);
  return { issuer, clientId, clientSecret, scopes, name: envValue(env, 'AUTH_OIDC_NAME') ?? 'OIDC' };
}

export function oidcPlugin(s: OidcSettings) {
  return genericOAuth({
    config: [
      {
        providerId: OIDC_PROVIDER_ID,
        name: s.name,
        discoveryUrl: `${s.issuer}/.well-known/openid-configuration`,
        // Without issuer and keys from discovery the provider is not registered: identity only from a verified ID token.
        requireIdTokenVerification: true,
        clientId: s.clientId,
        clientSecret: s.clientSecret,
        scopes: s.scopes,
        pkce: true
      }
    ]
  });
}
