import type { PageServerLoad } from './$types';
import { getJson, type DocumentPage } from '#lib/server/hub';

const PAGE = 100;

// ?role, ?repo, ?q, ?retired and ?offset: hub filters and paginates. retired=1 also includes documents retired from repos.
export const load: PageServerLoad = async ({ fetch, url }) => {
  const role = url.searchParams.get('role')?.trim() || '';
  const repo = url.searchParams.get('repo')?.trim() || '';
  const q = url.searchParams.get('q')?.trim() || '';
  const retired = url.searchParams.get('retired') === '1';
  const offset = Math.max(0, Number(url.searchParams.get('offset')) || 0);
  const query = new URLSearchParams({ limit: String(PAGE), offset: String(offset) });
  if (role) query.set('role', role);
  if (repo) query.set('repo', repo);
  if (q) query.set('q', q);
  if (retired) query.set('include_retired', 'true');
  const docs = await getJson<DocumentPage>(fetch, `/api/document-page?${query}`);
  return { docs, role, repo, q, retired, offset, pageSize: PAGE };
};
