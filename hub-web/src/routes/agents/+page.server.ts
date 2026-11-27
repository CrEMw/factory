// Agent mailbox: message feed and keys. Access as for the whole hub.
// A key is approved and revoked only by its owner through the sign-in binding on /people, with a fresh
// sign-in (at most 5 minutes old); only the owner sees the buttons, a curl key shows its fingerprint (first 12 chars
// of sha256). A pending key whose owner has no binding (owner_bound = false) can be revoked by any bound person:
// otherwise junk keys would hold the shared limit of 20 pending forever.
import { fail } from '@sveltejs/kit';
import type { Actions, PageServerLoad } from './$types';
import { getJson, isFresh } from '#lib/server/hub';
import { myHandle, reloginHref, reviewKey } from '#lib/server/logins';

export type MailboxKey = {
  id: string;
  owner: string;
  status: 'pending' | 'approved' | 'revoked';
  created_at: string;
  approved_at: string | null;
  last_seen_at: string;
  owner_bound: boolean;
};
export type MailboxMessage = { id: number; agent: string; text: string; created_at: string };

export const load: PageServerLoad = async ({ fetch, locals }) => {
  const [messages, keys, me] = await Promise.all([
    getJson<MailboxMessage[]>(fetch, '/api/mailbox/feed'),
    getJson<MailboxKey[]>(fetch, '/api/mailbox/keys'),
    myHandle(fetch, locals)
  ]);
  return { messages, keys, me, signedIn: !!locals.user, fresh: isFresh(locals.sessionCreatedAt), relogin: reloginHref('/agents') };
};

export const actions: Actions = {
  status: async ({ request, fetch, locals }) => {
    const form = await request.formData();
    const out = await reviewKey(fetch, locals, String(form.get('id') ?? ''), String(form.get('status') ?? ''));
    if (!out.ok) return fail(out.status, { message: out.message, relogin: out.relogin ? reloginHref('/agents') : null });
    return { saved: true };
  }
};
