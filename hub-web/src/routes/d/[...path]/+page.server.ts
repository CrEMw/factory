import { fail } from '@sveltejs/kit';
import type { Actions, PageServerLoad } from './$types';
import { formActor, getJson, missingRoute, refused, sendJson, type Document } from '#lib/server/hub';
import { renderMarkdown } from '#lib/server/markdown';
import { t } from '#lib/i18n/index';

// /d/<path in the repository>?repo=<owner/name>; without repo: documents of the hub default repository.
export const load: PageServerLoad = async ({ fetch, params, url }) => {
  const path = params.path.split('/').map(encodeURIComponent).join('/');
  const repo = url.searchParams.get('repo');
  const qs = repo ? `?repo=${encodeURIComponent(repo)}` : '';
  const document = await getJson<Document>(fetch, `/api/documents/${path}${qs}`);
  return { document, html: renderMarkdown(document.body) };
};

const needSession = () => t('docs.edge.needSession');

async function edgeError(res: Response): Promise<string> {
  if (await missingRoute(res)) return t('docs.edge.rollingOut');
  const text = await refused('edge', res);
  try {
    const e = JSON.parse(text) as { error?: string };
    if (e.error === 'confirm_requires_human') return t('docs.edge.humanOnly');
    if (e.error === 'closed') return t('docs.edge.closed');
    return `hub: ${res.status} ${e.error ?? text.slice(0, 200)}`;
  } catch {
    return `hub: ${res.status} ${text.slice(0, 200)}`;
  }
}

function edgeId(form: FormData): number | undefined {
  const id = Number(form.get('id'));
  return Number.isSafeInteger(id) && id > 0 ? id : undefined;
}

// A link proposed by topic (state proposed): a person confirms or rejects it. Sync from git does not
// re-create a rejected one.
export const actions: Actions = {
  confirm: async ({ request, fetch, locals }) => {
    const actor = formActor(locals);
    if (!actor) return fail(401, { linkMessage: needSession() });
    const id = edgeId(await request.formData());
    if (!id) return fail(400, { linkMessage: t('docs.edge.noId') });
    const res = await sendJson(fetch, 'POST', `/api/edges/${id}/confirm`, {}, actor);
    if (!res.ok) return fail(res.status, { linkMessage: await edgeError(res) });
    return { linked: true };
  },
  reject: async ({ request, fetch, locals }) => {
    const actor = formActor(locals);
    if (!actor) return fail(401, { linkMessage: needSession() });
    const id = edgeId(await request.formData());
    if (!id) return fail(400, { linkMessage: t('docs.edge.noId') });
    const res = await sendJson(fetch, 'POST', `/api/edges/${id}/close`, { reason: 'rejected' }, actor);
    if (!res.ok) return fail(res.status, { linkMessage: await edgeError(res) });
    return { linked: true };
  }
};
