import { error, redirect } from '@sveltejs/kit';
import { t } from '#lib/i18n/index';
import { clientName, consent } from '#lib/server/oauth-flow';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async ({ request, url, locals }) => {
  if (!locals.user) redirect(303, `/login${url.search}`);
  const clientId = url.searchParams.get('client_id');
  if (!clientId || !url.searchParams.has('sig')) error(400, t('auth.consent.noRequest'));
  return {
    client: await clientName(request.headers, clientId),
    scopes: (url.searchParams.get('scope') ?? '').split(' ').filter(Boolean),
    // The client picks its own name at registration; only the redirect address shows where the code goes.
    redirectUri: url.searchParams.get('redirect_uri') ?? '',
    user: locals.user
  };
};

export const actions: Actions = {
  default: async ({ request, url }) => {
    const form = await request.formData();
    // The provider returns the address: the client callback, checked against its registered redirect_uris.
    redirect(303, await consent(request.headers, url.search.slice(1), form.get('accept') === 'yes'), { external: true });
  }
};
