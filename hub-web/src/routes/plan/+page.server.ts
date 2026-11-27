// List of plan scopes (/api/scopes) with the project buffer zone and forecast finish of each
// (/api/plan/{root}). A scope plan is optional: without it the row is shown without zone and finish.
import type { PageServerLoad } from './$types';
import { getOptionalJson } from '#lib/server/hub';
import type { PlanResponse, ScopeSummary } from '#lib/plan/types';

export const load: PageServerLoad = async ({ fetch }) => {
  const scopes = await getOptionalJson<ScopeSummary[]>(fetch, '/api/scopes');
  if (typeof scopes === 'string') return { scopes: null, failed: scopes === 'failed', rows: [] };
  const rows = await Promise.all(
    scopes.map(async (s) => {
      const p = await getOptionalJson<PlanResponse>(fetch, `/api/plan/${encodeURIComponent(s.root)}`);
      const plan = typeof p === 'string' ? null : p;
      const pb = plan?.plan.buffers.find((b) => b.key === 'pb') ?? null;
      return {
        scope: s,
        end_date: plan?.end_date ?? null,
        zone: pb?.zone ?? null,
        consumed: pb?.consumed_pct ?? null,
        flags: plan?.plan.flags ?? [],
        late_by: plan?.plan.late_by ?? 0,
        planFailed: plan === null
      };
    })
  );
  return { scopes, failed: false, rows };
};
