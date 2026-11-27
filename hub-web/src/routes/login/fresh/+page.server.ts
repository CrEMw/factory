// Browser re-sign-in: a new Better Auth session from the identity of the entrance in front of the hub
// (the same signInEdge as the MCP sign-in in routes/login; with oidc via the OIDC provider), then back to ?next=.
// Needed for a fresh sign-in (no older than 5 minutes), which linking on /people and keys on /agents require. Only
// a POST of this page's form (csrfHandle in hooks.server.ts guards it): a link alone does not create a session.
// There is no password: fresh here means "the identity provider just confirmed the sign-in", not a second factor
// (passkey sign-in is discussed separately, shared memory spec "Person identity").
import { fail, redirect } from '@sveltejs/kit';
import { APIError } from 'better-auth/api';
import type { Actions, PageServerLoad } from './$types';
import { identity } from '#lib/server/auth';
import { isFresh } from '#lib/server/hub';
import { entrances } from '#lib/server/identity/index';
import { safeNext } from '#lib/server/logins';
import { signInFresh } from '#lib/server/oauth-flow';

export const load: PageServerLoad = ({ url, locals }) => ({
  next: safeNext(url.searchParams.get('next')),
  signedIn: !!locals.user,
  fresh: isFresh(locals.sessionCreatedAt),
  how: entrances(identity).short
});

export const actions: Actions = {
  default: async ({ request }) => {
    const next = safeNext((await request.formData()).get('next'));
    let provider: string | null;
    try {
      provider = await signInFresh(request.headers, next);
    } catch (e) {
      if (e instanceof APIError) return fail(401, { message: e.message });
      throw e;
    }
    // The OIDC provider has an external address; it returns to next.
    if (provider) redirect(303, provider, { external: true });
    redirect(303, next);
  }
};
