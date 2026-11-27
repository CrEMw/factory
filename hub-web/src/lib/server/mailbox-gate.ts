// Path tokens of the agent mailbox (GET /api/mailbox/<token>). MAILBOX_PATH_TOKENS is a comma separated list from
// the secret store (ExternalSecret hub-web-mailbox): during a rotation it holds the old and the new token, so both
// work until the agents have the new one. An empty list closes the mailbox: every path answers 404.
import { timingSafeEqual } from 'node:crypto';

export function pathTokens(env: Record<string, string | undefined> = process.env): string[] {
  return (env.MAILBOX_PATH_TOKENS ?? '')
    .split(',')
    .map((t) => t.trim())
    .filter(Boolean);
}

/** True when given equals one of the tokens; each comparison is constant time for equal lengths. */
export function gateOpen(given: string, tokens: readonly string[]): boolean {
  const a = Buffer.from(given);
  let open = false;
  for (const t of tokens) {
    const b = Buffer.from(t);
    if (a.length === b.length && timingSafeEqual(a, b)) open = true;
  }
  return open;
}
