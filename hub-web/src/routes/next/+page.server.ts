// "Next": a person's next task from /api/next?person= (the same answer as MCP next_task).
// Person: ?person= from the picker, else the login binding on /people, else the one whose email matches the session (personFor).
import type { PageServerLoad } from './$types';
import { getOptionalJson, type Person, type Ticket } from '#lib/server/hub';
import { HANDLE, personFor } from '#lib/plan/person';
import { myHandle } from '#lib/server/logins';
import type { NextOut } from '#lib/plan/types';

export const load: PageServerLoad = async ({ fetch, url, locals }) => {
  // Login binding first; old hub without the route or a failure: by email, as before.
  const [got, login] = await Promise.all([getOptionalJson<Person[]>(fetch, '/api/people'), myHandle(fetch, locals)]);
  const people = typeof got === 'string' ? [] : got.filter((p) => !p.is_agent && !p.retired);
  const asked = url.searchParams.get('person')?.trim() ?? '';
  const mine = personFor(locals.user, typeof got === 'string' ? [] : got, login);
  const person = asked || mine || '';
  const base = { people, peopleFailed: got === 'failed', person, mine, bad: false, next: null as NextOut | null, nextFailed: false, missing: false };
  if (!person) return { ...base, summary: null };
  if (!HANDLE.test(person)) return { ...base, bad: true, summary: null };
  const next = await getOptionalJson<NextOut>(fetch, `/api/next?person=${encodeURIComponent(person)}`);
  if (typeof next === 'string') return { ...base, nextFailed: next === 'failed', missing: next === 'missing', summary: null };
  // The ticket summary is optional: without it only the code is shown.
  const t = next.code ? await getOptionalJson<Ticket>(fetch, `/api/tickets/${encodeURIComponent(next.code)}`) : null;
  return { ...base, next, summary: t && typeof t !== 'string' ? t.summary : null };
};
