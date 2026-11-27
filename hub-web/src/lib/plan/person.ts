// Who the current person of the "Next" page is: an explicit ?person= from the picker, else the person
// bound to the sign-in on /people (GET /api/people/logins?user_id=), else the person from /api/people
// whose email or alias matches the hub session email (locals.user). Agents and retired people do not qualify.
import type { Person } from '#lib/server/hub';

export const HANDLE = /^(?!none$)[a-z][a-z0-9-]{0,39}$/;

export function personFor(user: { email: string } | null, people: Person[], login: string | null = null): string | null {
  if (user && login && HANDLE.test(login)) return login;
  const email = user?.email.trim().toLowerCase();
  if (!email) return null;
  const match = people.find(
    (p) => !p.is_agent && !p.retired && (p.email?.trim().toLowerCase() === email || p.aliases.some((a) => a.trim().toLowerCase() === email))
  );
  return match?.handle ?? null;
}
