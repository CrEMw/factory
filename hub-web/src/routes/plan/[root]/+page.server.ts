// Scope plan: tabs "Gantt", "Fever chart", "By person" (?tab=, works without JS too). Plan from
// /api/plan/{root}; dependencies and ticket summaries for arrows and tooltips from /api/graph?root= (optional:
// without the graph the Gantt has no arrows); fever points from /api/plan/{root}/fever. The "approve plan" button
// sends POST /api/plan/{root}/baseline on behalf of the human at the form; hub itself answers 403
// approve_requires_human to an agent and to a form without a session.
import { error, fail } from '@sveltejs/kit';
import type { Actions, PageServerLoad } from './$types';
import { formActor, getJson, getOptionalJson, sendJson, type Graph } from '#lib/server/hub';
import { approveOutcome } from '#lib/plan/approve';
import type { FeverResponse, PlanResponse } from '#lib/plan/types';

const CODE = /^[0-9a-z]{1,20}$/;
const TABS = ['gantt', 'fever', 'people'] as const;
type Tab = (typeof TABS)[number];

export const load: PageServerLoad = async ({ fetch, params, url, locals }) => {
  const root = params.root.trim().toLowerCase();
  if (!CODE.test(root)) error(400, 'bad scope root');
  const want = url.searchParams.get('tab') ?? 'gantt';
  const tab: Tab = (TABS as readonly string[]).includes(want) ? (want as Tab) : 'gantt';
  const r = encodeURIComponent(root);
  const [plan, fever, graph] = await Promise.all([
    getJson<PlanResponse>(fetch, `/api/plan/${r}`),
    tab === 'fever' ? getOptionalJson<FeverResponse>(fetch, `/api/plan/${r}/fever`) : Promise.resolve(null),
    tab === 'fever' ? Promise.resolve(null) : getOptionalJson<Graph>(fetch, `/api/graph?root=${r}&depth=10`)
  ]);
  return {
    root,
    tab,
    plan,
    fever: fever && typeof fever !== 'string' ? fever : null,
    feverFailed: fever === 'failed',
    graph: graph && typeof graph !== 'string' ? graph : null,
    graphFailed: graph === 'failed',
    signedIn: !!locals.user
  };
};

export const actions: Actions = {
  approve: async ({ request, fetch, params, locals }) => {
    const accept = (await request.formData()).get('accept_defaults') === 'true';
    const res = await sendJson(
      fetch,
      'POST',
      `/api/plan/${encodeURIComponent(params.root.trim().toLowerCase())}/baseline`,
      { accept_defaults: accept },
      formActor(locals)
    );
    const text = await res.text();
    const out = approveOutcome(res.status, text);
    if (out.kind === 'approved') return { approved: true };
    if (!res.ok) console.error(`hub POST baseline: ${res.status} ${text.slice(0, 200)}`);
    if (out.kind === 'defaults') return fail(409, { defaults: { defaulted_pct: out.defaulted_pct, max_defaulted_pct: out.max_defaulted_pct } });
    return fail(res.status, { approveMessage: out.message });
  }
};
