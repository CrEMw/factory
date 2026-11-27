import { fail } from '@sveltejs/kit';
import type { Actions, PageServerLoad } from './$types';
import {
  formActor,
  getJson,
  getOptionalJson,
  missingRoute,
  refused,
  sendJson,
  type Component,
  type Person,
  type ProjectCount,
  type Ticket,
  type TicketLinks
} from '#lib/server/hub';
import { renderMarkdown } from '#lib/server/markdown';
import { parseEstimate, parseWsjf } from '#lib/plan/forms';
import type { TicketPlanFields } from '#lib/plan/types';
import { t } from '#lib/i18n/index';

export const load: PageServerLoad = async ({ fetch, params, locals }) => {
  const code = encodeURIComponent(params.code);
  const [ticket, projects, got, people, components, planGot] = await Promise.all([
    getJson<Ticket>(fetch, `/api/tickets/${code}`),
    getJson<ProjectCount[]>(fetch, '/api/projects'),
    // Links are optional: hub without the graph yet (rollout) or a graph failure does not break the ticket page.
    getOptionalJson<TicketLinks>(fetch, `/api/tickets/${code}/links`),
    // People and components likewise: without them only the assignee and components form is missing.
    getOptionalJson<Person[]>(fetch, '/api/people'),
    getOptionalJson<Component[]>(fetch, '/api/components'),
    // Current estimate and WSJF for the forms; without them the forms are empty.
    getOptionalJson<TicketPlanFields>(fetch, `/api/tickets/${code}/plan`)
  ]);
  const links = typeof got === 'string' ? null : got;
  const fields =
    typeof people === 'string' || typeof components === 'string' || ticket.components === undefined
      ? null
      : { people: people.filter((p) => !p.retired || p.handle === ticket.assignee), components: components.filter((c) => !c.retired || ticket.components?.includes(c.slug)) };
  return {
    ticket,
    projects,
    links,
    linksFailed: got === 'failed',
    fields,
    fieldsFailed: people === 'failed' || components === 'failed',
    planFields: typeof planGot === 'string' ? null : planGot,
    html: renderMarkdown(ticket.body),
    signedIn: !!locals.user
  };
};

// Link from the form: select value "rel:dir". out: this ticket is src (blocks, part of, duplicate), in: the other one is src.
const LINK_KINDS: Record<string, { rel: string; out: boolean }> = {
  'blocks:in': { rel: 'blocks', out: false },
  'blocks:out': { rel: 'blocks', out: true },
  'part_of:out': { rel: 'part_of', out: true },
  'part_of:in': { rel: 'part_of', out: false },
  'relates_to:out': { rel: 'relates_to', out: true },
  'duplicates:out': { rel: 'duplicates', out: true }
};

/** hub answer to an estimate or WSJF: 400 with the validation text, 404 without the ticket, empty 404 before rollout. */
async function planError(res: Response): Promise<string> {
  if (await missingRoute(res)) return t('tickets.error.rollingOut');
  const text = await refused('plan field', res);
  if (res.status === 404) return t('tickets.error.noTicket');
  return `hub: ${res.status} ${text.slice(0, 200)}`;
}

/** hub answer about an edge (link, assignee, component) in human words. */
async function edgeError(res: Response): Promise<string> {
  if (await missingRoute(res)) return t('tickets.error.rollingOut');
  const text = await refused('edge', res);
  try {
    const e = JSON.parse(text) as { error?: string; path?: string[]; kind?: string; key?: string };
    switch (e.error) {
      case 'cycle':
        return t('tickets.error.cycle', { path: (e.path ?? []).join(' → ') });
      case 'single_violation':
        return t('tickets.error.singleViolation');
      case 'exists':
        return t('tickets.error.exists');
      case 'self_link':
        return t('tickets.error.selfLink');
      case 'unknown_node':
        return t('tickets.error.unknownNode', { key: e.key ?? '' });
      case 'confirm_requires_human':
        return t('tickets.error.confirmRequiresHuman');
      case 'close_requires_human':
        return t('tickets.error.closeRequiresHuman');
      case 'closed':
        return t('tickets.error.closed');
      case 'unknown_person':
        return t('tickets.error.unknownPerson', { key: e.key ?? '' });
      case 'unknown_component':
        return t('tickets.error.unknownComponent', { key: e.key ?? '' });
      case 'retired':
        return t('tickets.error.retired', { key: e.key ?? t('tickets.error.node') });
      default:
        return `hub: ${res.status} ${e.error ?? text.slice(0, 200)}`;
    }
  } catch {
    return `hub: ${res.status} ${text.slice(0, 200)}`;
  }
}

export const actions: Actions = {
  // Status, priority and project change in one form; an empty priority clears it.
  update: async ({ request, fetch, params, locals }) => {
    const form = await request.formData();
    const priority = String(form.get('priority') ?? '');
    const res = await sendJson(
      fetch,
      'PATCH',
      `/api/tickets/${encodeURIComponent(params.code)}`,
      { status: form.get('status'), project: form.get('project'), priority: priority || null },
      formActor(locals)
    );
    if (!res.ok) return fail(res.status, { message: await refused('PATCH ticket', res) });
    return { saved: true };
  },

  // Assignee and components: the whole component set from the checkboxes, an empty assignee clears it.
  // Like a status edit, without a session it goes as service:hub-web; an assignee set by a person is cleared only by a person.
  fields: async ({ request, fetch, params, locals }) => {
    const form = await request.formData();
    const assignee = String(form.get('assignee') ?? '').trim();
    const components = form
      .getAll('component')
      .map((c) => String(c).trim())
      .filter(Boolean);
    const res = await sendJson(
      fetch,
      'POST',
      `/api/tickets/${encodeURIComponent(params.code)}/fields`,
      { assignee: assignee || null, components },
      formActor(locals)
    );
    if (!res.ok) return fail(res.status, { fieldsMessage: await edgeError(res) });
    return { fieldsSaved: true };
  },

  link: async ({ request, fetch, params, locals }) => {
    const actor = formActor(locals);
    if (!actor) return fail(401, { linkMessage: t('tickets.error.needSession') });
    const form = await request.formData();
    const kind = LINK_KINDS[String(form.get('kind') ?? '')];
    const other = String(form.get('other') ?? '').trim().toLowerCase();
    if (!kind || !/^[0-9a-z]{1,20}$/.test(other)) return fail(400, { linkMessage: t('tickets.error.needKindCode') });
    const self = { kind: 'ticket', key: params.code };
    const that = { kind: 'ticket', key: other };
    const res = await sendJson(
      fetch,
      'POST',
      '/api/edges',
      { src: kind.out ? self : that, rel: kind.rel, dst: kind.out ? that : self },
      actor
    );
    if (!res.ok) return fail(res.status, { linkMessage: await edgeError(res) });
    return { linked: true };
  },

  confirm: async ({ request, fetch, locals }) => {
    const actor = formActor(locals);
    if (!actor) return fail(401, { linkMessage: t('tickets.error.needSession') });
    const id = Number((await request.formData()).get('id'));
    if (!Number.isSafeInteger(id) || id <= 0) return fail(400, { linkMessage: t('tickets.error.noLinkId') });
    const res = await sendJson(fetch, 'POST', `/api/edges/${id}/confirm`, {}, actor);
    if (!res.ok) return fail(res.status, { linkMessage: await edgeError(res) });
    return { linked: true };
  },

  close: async ({ request, fetch, locals }) => {
    const actor = formActor(locals);
    if (!actor) return fail(401, { linkMessage: t('tickets.error.needSession') });
    const form = await request.formData();
    const id = Number(form.get('id'));
    const reason = String(form.get('reason') ?? '');
    if (!Number.isSafeInteger(id) || id <= 0 || !['removed', 'rejected'].includes(reason)) {
      return fail(400, { linkMessage: t('tickets.error.needIdReason') });
    }
    const res = await sendJson(fetch, 'POST', `/api/edges/${id}/close`, { reason }, actor);
    if (!res.ok) return fail(res.status, { linkMessage: await edgeError(res) });
    return { linked: true };
  },

  // Estimate and WSJF: a whole new row version, validated as in plan.rs and mcp.ts (lib/plan/forms.ts).
  estimate: async ({ request, fetch, params, locals }) => {
    const actor = formActor(locals);
    if (!actor) return fail(401, { estimateMessage: t('tickets.error.needSessionPlan') });
    const parsed = parseEstimate(await request.formData());
    if (!parsed.ok) return fail(400, { estimateMessage: parsed.error });
    const res = await sendJson(fetch, 'PUT', `/api/tickets/${encodeURIComponent(params.code)}/estimate`, parsed.body, actor);
    if (!res.ok) return fail(res.status, { estimateMessage: await planError(res) });
    return { estimateSaved: true };
  },

  wsjf: async ({ request, fetch, params, locals }) => {
    const actor = formActor(locals);
    if (!actor) return fail(401, { wsjfMessage: t('tickets.error.needSessionPlan') });
    const parsed = parseWsjf(await request.formData());
    if (!parsed.ok) return fail(400, { wsjfMessage: parsed.error });
    const res = await sendJson(fetch, 'PUT', `/api/tickets/${encodeURIComponent(params.code)}/wsjf`, parsed.body, actor);
    if (!res.ok) return fail(res.status, { wsjfMessage: await planError(res) });
    const out = (await res.json()) as { wsjf: number | null };
    return { wsjfSaved: true, wsjfValue: out.wsjf };
  }
};
