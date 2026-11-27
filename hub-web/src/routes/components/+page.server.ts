// Directory of business components: project (component_of) and ticket count (in_component).
// The initial list is services/hub/seed/components.toml; only a person with a hub session adds a new component.
import { fail } from '@sveltejs/kit';
import type { Actions, PageServerLoad } from './$types';
import { formActor, getOptionalJson, refused, sendJson, type Component } from '#lib/server/hub';
import { t } from '#lib/i18n/index';

export const load: PageServerLoad = async ({ fetch, locals }) => {
  const components = await getOptionalJson<Component[]>(fetch, '/api/components');
  return {
    components: typeof components === 'string' ? null : components,
    failed: components === 'failed',
    signedIn: !!locals.user
  };
};

export const actions: Actions = {
  create: async ({ request, fetch, locals }) => {
    const actor = formActor(locals);
    if (!actor) return fail(401, { message: t('tickets.components.error.needSession') });
    const form = await request.formData();
    const slug = String(form.get('slug') ?? '').trim().toLowerCase();
    const title = String(form.get('title') ?? '').trim();
    const project = String(form.get('project') ?? '').trim();
    if (!/^[a-z0-9][a-z0-9-]{0,39}$/.test(slug) || !title) return fail(400, { message: t('tickets.components.error.bad') });
    const res = await sendJson(fetch, 'POST', '/api/components', { slug, title, project: project || null }, actor);
    if (res.status === 409) return fail(409, { message: t('tickets.components.error.exists', { slug }) });
    if (!res.ok) return fail(res.status, { message: `hub: ${res.status} ${(await refused('POST /api/components', res)).slice(0, 200)}` });
    return { created: slug };
  }
};
