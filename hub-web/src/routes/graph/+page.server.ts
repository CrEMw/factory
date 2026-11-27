// "Graph" view: a project's tickets or a ticket's neighbourhood with blocks and part_of links. hub
// computes the layered layout (/api/graph), the page only draws the SVG.
import type { PageServerLoad } from './$types';
import { getOptionalJson, type Graph, type ProjectCount } from '#lib/server/hub';

const PROJECT = /^[a-z0-9][a-z0-9-]{0,39}$/;
const CODE = /^[0-9a-z]{1,20}$/;

export const load: PageServerLoad = async ({ fetch, url }) => {
  const project = url.searchParams.get('project')?.trim() || '';
  const root = url.searchParams.get('root')?.trim().toLowerCase() || '';
  const depth = url.searchParams.get('depth')?.trim() || '';
  const projects = await getOptionalJson<ProjectCount[]>(fetch, '/api/projects');
  const filters = { project, root, depth };
  const list = typeof projects === 'string' ? [] : projects;
  if ((project && !PROJECT.test(project)) || (root && !CODE.test(root)) || (depth && !/^([1-9]|10)$/.test(depth)))
    return { graph: null, failed: false, missing: false, bad: true, projects: list, filters };
  if (!project && !root) return { graph: null, failed: false, missing: false, bad: false, projects: list, filters };
  const q = new URLSearchParams();
  if (project) q.set('project', project);
  if (root) q.set('root', root);
  if (depth) q.set('depth', depth);
  const graph = await getOptionalJson<Graph>(fetch, `/api/graph?${q}`);
  return {
    graph: typeof graph === 'string' ? null : graph,
    failed: graph === 'failed',
    missing: graph === 'missing',
    bad: false,
    projects: list,
    filters
  };
};
