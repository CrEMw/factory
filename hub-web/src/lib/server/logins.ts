// Linking a sign-in to a graph person (the "this is me" form on /people) and mailbox key decisions (/agents):
// form logic without SvelteKit, so node --test checks it (logins.test.ts). Rules and invariants live in hub and the
// database (services/hub/src/logins.rs, 0022_person_login.sql); here only the human behind the form, sign-in freshness
// and hub answers in human words. hub-web rolls out before hub: an empty 404 means "still rolling out".
import { t, type Key } from '#lib/i18n/index';
import { formActor, hubFetch, missingRoute, refused, sendJson } from '#lib/server/hub';

/** A live link from GET /api/people/logins. */
export type Login = { user_id: string; handle: string; actor: string; created_at: string };

/** Where to return after re-sign-in: only a path of this hub. */
export function safeNext(next: unknown): string {
  const s = typeof next === 'string' ? next.trim() : '';
  return /^\/(?![/\\])[^\s]*$/.test(s) ? s : '/';
}

/** Re-sign-in link (routes/login/fresh): a new session from the entrance in front of the hub, then back. */
export function reloginHref(next: string): string {
  return `/login/fresh?next=${encodeURIComponent(safeNext(next))}`;
}

/** curl key fingerprint: the first 12 chars of sha256; the agent script prints it locally too. */
export function fingerprint(id: string): string | null {
  return /^sha256:[0-9a-f]{64}$/.test(id) ? id.slice(7, 19) : null;
}

export type FormOutcome = { ok: true; body: unknown } | { ok: false; status: number; message: string; relogin?: boolean };

// hub error codes to messages; relogin: the page offers to sign in again.
const MESSAGES: Record<string, { key: Key; relogin?: boolean }> = {
  fresh_session_required: { key: 'auth.logins.freshRequired', relogin: true },
  requires_human: { key: 'auth.logins.requiresHuman', relogin: true },
  already_bound: { key: 'auth.logins.alreadyBound' },
  handle_taken: { key: 'auth.logins.handleTaken' },
  unknown_person: { key: 'auth.logins.unknownPerson' },
  agent_person: { key: 'auth.logins.agentPerson' },
  retired: { key: 'auth.logins.retired' },
  not_your_login: { key: 'auth.logins.notYourLogin' },
  no_login: { key: 'auth.logins.noLogin' },
  not_key_owner: { key: 'auth.logins.notKeyOwner' },
  no_such_key: { key: 'auth.logins.noSuchKey' }
};

/** hub answer to a form write. */
export async function outcome(what: string, res: Response): Promise<FormOutcome> {
  if (await missingRoute(res)) return { ok: false, status: 503, message: t('auth.logins.rollingOut') };
  if (res.ok) return { ok: true, body: await res.json().catch(() => null) };
  const text = await refused(what, res);
  let code = '';
  try {
    code = String((JSON.parse(text) as { error?: unknown }).error ?? '');
  } catch {
    code = '';
  }
  const known = MESSAGES[code];
  if (known) return { ok: false, status: res.status, message: t(known.key), relogin: known.relogin };
  return { ok: false, status: res.status, message: `hub: ${res.status} ${text.slice(0, 200)}` };
}

/** A human with a fresh sign-in, otherwise a refusal without calling hub (hub and the database check the same). */
function freshHuman(locals: App.Locals, now: number) {
  const actor = formActor(locals, now);
  if (!actor) return { refused: { ok: false, status: 401, message: t('auth.logins.needSession'), relogin: true } as FormOutcome };
  if (!actor.fresh) return { refused: { ok: false, status: 403, message: t('auth.logins.freshRequired'), relogin: true } as FormOutcome };
  return { actor };
}

export async function bindLogin(fetch: typeof globalThis.fetch, locals: App.Locals, handle: string, now = Date.now()): Promise<FormOutcome> {
  const h = handle.trim().toLowerCase();
  if (!/^(?!none$)[a-z][a-z0-9-]{0,39}$/.test(h)) return { ok: false, status: 400, message: t('auth.logins.pickPerson') };
  const who = freshHuman(locals, now);
  if (who.refused) return who.refused;
  return outcome('POST /api/people/logins', await sendJson(fetch, 'POST', '/api/people/logins', { handle: h }, who.actor));
}

export async function retireLogin(fetch: typeof globalThis.fetch, locals: App.Locals, handle: string, now = Date.now()): Promise<FormOutcome> {
  const h = handle.trim().toLowerCase();
  if (!/^(?!none$)[a-z][a-z0-9-]{0,39}$/.test(h)) return { ok: false, status: 400, message: t('auth.logins.noSuchPerson') };
  const who = freshHuman(locals, now);
  if (who.refused) return who.refused;
  const path = `/api/people/logins/${encodeURIComponent(h)}/retire`;
  return outcome(`POST ${path}`, await sendJson(fetch, 'POST', path, {}, who.actor));
}

export async function reviewKey(
  fetch: typeof globalThis.fetch,
  locals: App.Locals,
  id: string,
  status: string,
  now = Date.now()
): Promise<FormOutcome> {
  if (!id || !['approved', 'revoked'].includes(status)) return { ok: false, status: 400, message: t('auth.logins.idAndStatus') };
  const who = freshHuman(locals, now);
  if (who.refused) return who.refused;
  return outcome('PATCH mailbox key', await sendJson(fetch, 'PATCH', `/api/mailbox/keys/${encodeURIComponent(id)}`, { status }, who.actor));
}

/**
 * Live links (all or of one account). 'missing': hub has no route yet; 'failed': an error (the page lives without them).
 */
export async function listLogins(fetch: typeof globalThis.fetch, userId?: string): Promise<Login[] | 'missing' | 'failed'> {
  const path = `/api/people/logins${userId ? `?user_id=${encodeURIComponent(userId)}` : ''}`;
  try {
    const res = await hubFetch(fetch, path);
    if (await missingRoute(res)) return 'missing';
    if (!res.ok) {
      await refused(`GET ${path}`, res);
      return 'failed';
    }
    return (await res.json()) as Login[];
  } catch (e) {
    console.error(`hub GET ${path}: ${e instanceof Error ? e.message : String(e)}`);
    return 'failed';
  }
}

/** handle linked to the session account, or null (no session, no link, old hub, error). */
export async function myHandle(fetch: typeof globalThis.fetch, locals: App.Locals): Promise<string | null> {
  if (!locals.user) return null;
  const got = await listLogins(fetch, locals.user.id);
  return typeof got === 'string' ? null : (got.find((l) => l.user_id === locals.user?.id)?.handle ?? null);
}
