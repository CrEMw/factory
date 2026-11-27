// Hub people directory: ticket assignees (assigned_to edges) with ticket counts.
// Only a person with a hub session adds people; changes are additions only, seed services/hub/seed/people.toml.
// The "this is me" form binds a hub sign-in to a graph person (one live binding per sign-in and per
// person); any binding can be retired (mistaken or someone else's). Both forms need a fresh sign-in (at most
// 5 minutes old); with an old session the page links to a new sign-in (/login/fresh).
import { fail } from '@sveltejs/kit';
import type { Actions, PageServerLoad } from './$types';
import { formActor, getOptionalJson, isFresh, refused, sendJson, type Person } from '#lib/server/hub';
import { bindLogin, listLogins, reloginHref, retireLogin, type FormOutcome } from '#lib/server/logins';
import { t } from '#lib/i18n/index';

export const load: PageServerLoad = async ({ fetch, locals }) => {
  const [people, logins] = await Promise.all([getOptionalJson<Person[]>(fetch, '/api/people'), listLogins(fetch)]);
  const list = typeof logins === 'string' ? null : logins;
  return {
    people: typeof people === 'string' ? null : people,
    failed: people === 'failed',
    signedIn: !!locals.user,
    fresh: isFresh(locals.sessionCreatedAt),
    relogin: reloginHref('/people'),
    // Bindings: null while hub lacks the route (rollout) or on failure; the page works without them.
    logins: list?.map(({ handle, created_at }) => ({ handle, created_at })) ?? null,
    loginsMissing: logins === 'missing',
    mine: list?.find((l) => l.user_id === locals.user?.id)?.handle ?? null
  };
};

function refusedForm(out: Extract<FormOutcome, { ok: false }>, field: 'bindMessage' | 'retireMessage') {
  return fail(out.status, { [field]: out.message, relogin: out.relogin ? reloginHref('/people') : null });
}

export const actions: Actions = {
  create: async ({ request, fetch, locals }) => {
    const actor = formActor(locals);
    if (!actor) return fail(401, { message: t('agents.people.errNoSession') });
    const form = await request.formData();
    const handle = String(form.get('handle') ?? '').trim().toLowerCase();
    const name = String(form.get('name') ?? '').trim();
    if (!/^[a-z][a-z0-9-]{0,39}$/.test(handle) || handle === 'none' || !name)
      return fail(400, { message: t('agents.people.errInvalid') });
    const res = await sendJson(fetch, 'POST', '/api/people', { handle, name }, actor);
    if (res.status === 409) return fail(409, { message: t('agents.people.errExists', { handle }) });
    if (!res.ok) return fail(res.status, { message: `hub: ${res.status} ${(await refused('POST /api/people', res)).slice(0, 200)}` });
    return { created: handle };
  },
  bind: async ({ request, fetch, locals }) => {
    const handle = String((await request.formData()).get('handle') ?? '');
    const out = await bindLogin(fetch, locals, handle);
    return out.ok ? { bound: handle } : refusedForm(out, 'bindMessage');
  },
  retire: async ({ request, fetch, locals }) => {
    const handle = String((await request.formData()).get('handle') ?? '');
    const out = await retireLogin(fetch, locals, handle);
    return out.ok ? { retired: handle } : refusedForm(out, 'retireMessage');
  }
};
