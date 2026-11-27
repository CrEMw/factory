// Agent mailbox: GET /api/mailbox/<token>?agent=&key=&msg=, text/plain response.
// Public path (Access lets it through, no session needed): the gates here are the path token and key approval in hub.
// The path token is not an access secret: agents get it from the secret store and it rotates; key approval is what lets callers in.
// The key is not logged.
import { hubFetch } from '#lib/server/hub';
import { gateOpen, pathTokens } from '#lib/server/mailbox-gate';
import type { RequestHandler } from './$types';

// The path tokens come from MAILBOX_PATH_TOKENS (lib/server/mailbox-gate.ts); an empty list closes the mailbox.
const TOKENS = pathTokens();

const TEXT = { 'content-type': 'text/plain; charset=utf-8', 'cache-control': 'no-store' };

export const GET: RequestHandler = async ({ params, url, fetch }) => {
  if (!gateOpen(params.gate, TOKENS)) return new Response('not found\n', { status: 404, headers: TEXT });
  const [agent, key, msg] = ['agent', 'key', 'msg'].map((name) => url.searchParams.get(name) ?? '');
  // The key goes to hub in the body: the outgoing request URL ends up in the span.
  const res = await hubFetch(fetch, '/api/mailbox/exchange', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ agent, key, msg })
  });
  if (res.status >= 500 && res.status !== 503) {
    console.error(`hub mailbox exchange: ${res.status}`);
    return new Response('mailbox unavailable\n', { status: 502, headers: TEXT });
  }
  return new Response(await res.text(), { status: res.status, headers: TEXT });
};
