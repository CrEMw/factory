import { isRedirect, redirect } from '@sveltejs/kit';
import { APIError } from 'better-auth/api';
import { t } from '#lib/i18n/index';
import { identity } from '#lib/server/auth';
import { entrances } from '#lib/server/identity/index';
import { signInAndContinue } from '#lib/server/oauth-flow';
import type { PageServerLoad } from './$types';

// The OAuth provider sends an MCP client without a session here. There is no password: the entrance in front of the
// hub gives the identity (identity/ providers), so sign-in happens as soon as the page opens and the browser goes on
// with the authorization. Without a header sign-in and with oidc enabled, the browser goes to the OIDC provider and
// continues the authorization after it.
export const load: PageServerLoad = async ({ request, url }) => {
  const how = entrances(identity).full;
  const query = url.search.slice(1);
  if (!query.includes('sig=')) return { error: t('auth.login.onlyFromMcp'), how };
  try {
    // Next is /consent or straight the client callback (checked by the provider against its redirect_uris).
    redirect(303, await signInAndContinue(request.headers, query), { external: true });
  } catch (e) {
    if (isRedirect(e)) throw e;
    if (e instanceof APIError) return { error: e.message, how };
    throw e;
  }
};
