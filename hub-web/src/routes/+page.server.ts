import { fail, redirect } from '@sveltejs/kit';
import type { Actions, PageServerLoad } from './$types';
import {
  getJson,
  getOptionalJson,
  refused,
  formActor,
  sendJson,
  STATUSES,
  type Component,
  type Person,
  type ProjectCount,
  type TicketItem,
  type TicketPage,
  type TicketRow,
  type TicketStats
} from '#lib/server/hub';
import { t } from '#lib/i18n/index';

const VIEWS = ['board', 'prio', 'list'] as const;
type View = (typeof VIEWS)[number];
const PAGE = 100;
/** The board shows only the latest closed tickets: otherwise the done column is almost the whole archive. */
const DONE_ON_BOARD = 20;
const ACTIVE_CAP = 500;

// View and filters live in the URL: ?view=board|prio|list, ?project, ?status (list), ?priority,
// ?tag, ?q, ?assignee (handle or none), ?component (slug), ?sort, ?offset. hub filters and counts, the page gets only what is shown.
// ?t=<code> is the old Roadmap link to a ticket.
export const load: PageServerLoad = async ({ fetch, url }) => {
  const sp = url.searchParams;
  const legacy = sp.get('t')?.trim();
  if (legacy) redirect(308, `/t/${encodeURIComponent(legacy)}`);
  const view: View = VIEWS.find((v) => v === sp.get('view')) ?? 'board';
  const f = {
    project: sp.get('project')?.trim() || '',
    status: sp.get('status')?.trim() || 'open,doing',
    priority: sp.get('priority')?.trim() || '',
    tag: sp.get('tag')?.trim() || '',
    q: sp.get('q')?.trim() || '',
    assignee: sp.get('assignee')?.trim() || '',
    component: sp.get('component')?.trim() || '',
    sort: sp.get('sort')?.trim() || '',
    offset: Math.max(0, Number(sp.get('offset')) || 0)
  };
  const page = (extra: Record<string, string | number>) => {
    const q = new URLSearchParams();
    const filters = { project: f.project, priority: f.priority, tag: f.tag, q: f.q, assignee: f.assignee, component: f.component };
    for (const [k, v] of Object.entries({ ...filters, ...extra })) {
      if (v !== '' && v !== undefined) q.set(k, String(v));
    }
    return getJson<TicketPage>(fetch, `/api/ticket-page?${q}`);
  };
  const statsQuery = new URLSearchParams();
  for (const k of ['project', 'tag', 'q', 'assignee', 'component'] as const) if (f[k]) statsQuery.set(k, f[k]);

  const columns =
    view === 'board'
      ? await Promise.all([
          page({ status: 'open', sort: 'prio', limit: ACTIVE_CAP }),
          page({ status: 'doing', sort: 'prio', limit: ACTIVE_CAP }),
          page({ status: 'done', sort: 'updated', limit: DONE_ON_BOARD })
        ])
      : null;
  const [list, stats, projects, people, components] = await Promise.all([
    view === 'board'
      ? null
      : view === 'prio'
        ? page({ status: f.status, sort: 'prio', limit: ACTIVE_CAP })
        : page({ status: f.status, sort: f.sort, limit: PAGE, offset: f.offset }),
    getJson<TicketStats>(fetch, `/api/ticket-stats?${statsQuery}`),
    getJson<ProjectCount[]>(fetch, '/api/projects'),
    // Assignee and component filters: the page works without them (hub rollout, failure).
    getOptionalJson<Person[]>(fetch, '/api/people'),
    getOptionalJson<Component[]>(fetch, '/api/components')
  ]);
  return {
    view,
    filters: f,
    pageSize: PAGE,
    cap: ACTIVE_CAP,
    board: columns && { open: columns[0], doing: columns[1], done: columns[2] },
    list,
    stats,
    projects,
    people: typeof people === 'string' ? [] : people.filter((p) => !p.retired),
    components: typeof components === 'string' ? [] : components.filter((c) => !c.retired)
  };
};

export const actions: Actions = {
  create: async ({ request, fetch, locals }) => {
    const form = await request.formData();
    const priority = String(form.get('priority') ?? '');
    const res = await sendJson(fetch, 'POST', '/api/tickets', {
      project: form.get('project'),
      summary: form.get('summary'),
      body: form.get('body') ?? '',
      priority: priority || null
    }, formActor(locals));
    if (!res.ok) return fail(res.status, { message: await refused('POST /api/tickets', res), create: true });
    const ticket = (await res.json()) as TicketRow;
    redirect(303, `/t/${encodeURIComponent(ticket.code)}`);
  },
  // Status change from the board (drag and drop) and from the list (dropdown).
  status: async ({ request, fetch, locals }) => {
    const form = await request.formData();
    const code = String(form.get('code') ?? '');
    const status = String(form.get('status') ?? '');
    if (!code || !STATUSES.some((s) => s === status)) return fail(400, { message: t('tickets.index.needCodeStatus') });
    const res = await sendJson(fetch, 'PATCH', `/api/tickets/${encodeURIComponent(code)}`, { status }, formActor(locals));
    if (!res.ok) return fail(res.status, { message: await refused(`PATCH /api/tickets/${code}`, res) });
    return { moved: code };
  }
};
