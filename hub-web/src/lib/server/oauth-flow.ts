// OAuth authorization steps on the SvelteKit server: sign-in via an identity provider and consent.
// auth.api is called with the browser headers (session cookie, Host, Tailscale-User-* or Cf-Access-Jwt-Assertion), but as a JSON request:
// then the provider answers { redirect, url } instead of an HTTP redirect, and the page picks the next step.
import { APIError } from 'better-auth/api';
import { OIDC_PROVIDER_ID } from '#lib/server/identity/oidc';
import { auth, identity } from './auth';

function asJsonCall(headers: Headers): Headers {
  const h = new Headers(headers);
  h.delete('sec-fetch-mode');
  h.set('accept', 'application/json');
  return h;
}

function redirectUrl(r: unknown): string | null {
  if (r && typeof r === 'object' && 'url' in r && typeof r.url === 'string') return r.url;
  return null;
}

/** No header sign-in, but oidc is enabled: then the browser goes to the OIDC provider. */
function toOidc(e: unknown): boolean {
  return !!identity.oidc && e instanceof APIError && e.status === 'UNAUTHORIZED';
}

/**
 * Starts sign-in at the OIDC provider: returns the provider address. callbackURL: where to return after sign-in.
 * oauthQuery: the signed MCP authorization request; the OAuth provider remembers it and continues the authorization
 * itself after sign-in.
 */
async function startOidc(headers: Headers, callbackURL: string, oauthQuery?: string): Promise<string> {
  let r: unknown;
  try {
    r = await auth.api.signInSocial({
      headers: asJsonCall(headers),
      body: { provider: OIDC_PROVIDER_ID, callbackURL, disableRedirect: true, ...(oauthQuery ? { oauth_query: oauthQuery } : {}) } as {
        provider: string;
        callbackURL: string;
      }
    });
  } catch (e) {
    // Provider unavailable (discovery did not answer): the sign-in page shows the error, not a 500.
    if (e instanceof APIError) throw e;
    throw new APIError('BAD_GATEWAY', { message: `oidc: provider unavailable (${e instanceof Error ? e.message : String(e)})` });
  }
  const url = redirectUrl(r);
  if (!url) throw new APIError('BAD_GATEWAY', { message: 'oidc: sign-in returned no provider url' });
  return url;
}

/** Sign-in (by headers of the entrance in front of the hub, otherwise OIDC) and continuing the authorization. Returns the next address. */
export async function signInAndContinue(headers: Headers, oauthQuery: string): Promise<string> {
  try {
    const r = await auth.api.signInEdge({ headers: asJsonCall(headers), body: { oauth_query: oauthQuery } });
    return redirectUrl(r) ?? '/';
  } catch (e) {
    if (toOidc(e)) return startOidc(headers, '/', oauthQuery);
    throw e;
  }
}

/**
 * Re-sign-in (routes/login/fresh): a new session from the entrance headers and null, or the OIDC provider address,
 * after which the browser returns to next with a new session.
 */
export async function signInFresh(headers: Headers, next: string): Promise<string | null> {
  try {
    await auth.api.signInEdge({ headers, body: {} });
    return null;
  } catch (e) {
    if (toOidc(e)) return startOidc(headers, next);
    throw e;
  }
}

export async function consent(headers: Headers, oauthQuery: string, accept: boolean): Promise<string> {
  const r = await auth.api.oauth2Consent({ headers: asJsonCall(headers), body: { accept, oauth_query: oauthQuery } });
  const url = redirectUrl(r);
  if (!url) throw new Error('consent: provider returned no redirect');
  return url;
}

export async function clientName(headers: Headers, clientId: string): Promise<string> {
  try {
    const c = (await auth.api.getOAuthClientPublic({ headers, query: { client_id: clientId } })) as { client_name?: string } | null;
    return c?.client_name || clientId;
  } catch {
    return clientId;
  }
}
