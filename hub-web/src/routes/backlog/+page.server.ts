// Backlog by WSJF from /api/backlog: open and doing leaves by descending WSJF, those without an estimate
// or WSJF last; scopes by root WSJF. Project filter and limit as in hub (limit 1..500).
import type { PageServerLoad } from './$types';
import { getOptionalJson, type ProjectCount } from '#lib/server/hub';
import type { Backlog } from '#lib/plan/types';

const PROJECT = /^[a-z0-9][a-z0-9-]{0,39}$/;
const LIMITS = ['50', '100', '200', '500'];

export const load: PageServerLoad = async ({ fetch, url }) => {
  const project = url.searchParams.get('project')?.trim() || '';
  const limit = url.searchParams.get('limit')?.trim() || '100';
  const projects = await getOptionalJson<ProjectCount[]>(fetch, '/api/projects');
  const base = { project, limit, projects: typeof projects === 'string' ? [] : projects, limits: LIMITS };
  if ((project && !PROJECT.test(project)) || !LIMITS.includes(limit)) return { ...base, backlog: null, bad: true, failed: false, missing: false };
  const q = new URLSearchParams({ limit });
  if (project) q.set('project', project);
  const backlog = await getOptionalJson<Backlog>(fetch, `/api/backlog?${q}`);
  return {
    ...base,
    backlog: typeof backlog === 'string' ? null : backlog,
    bad: false,
    failed: backlog === 'failed',
    missing: backlog === 'missing'
  };
};
