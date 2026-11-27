import { readFileSync } from 'node:fs';
import { error } from '@sveltejs/kit';

export type TicketRow = {
  seq: number;
  project: string;
  code: string;
  summary: string;
  status: string;
  priority: string | null;
  tags: string[];
  created_at: string;
  updated_at: string;
};
/** Ticket assignee (handle) and components (slug) from graph edges. The old hub does not return them. */
export type TicketFields = { assignee?: string | null; components?: string[] };
/** List row from /api/ticket-page: without the ticket text. */
export type TicketItem = Pick<TicketRow, 'code' | 'project' | 'summary' | 'status' | 'priority' | 'tags' | 'updated_at'> &
  TicketFields;
export type TicketPage = { total: number; items: TicketItem[] };
export type TicketStats = {
  cells: { status: string; priority: string | null; tickets: number }[];
  active_tags: { tag: string; tickets: number }[];
};
/** A document registry repo and its latest sync from git. */
export type DocumentRepo = {
  repo: string;
  project: string;
  revision: string | null;
  synced_at: string | null;
  problems: { path: string; kind: string; detail: string }[];
  documents: number;
};
export type DocumentPage = {
  total: number;
  roles: { role: string; documents: number }[];
  /** Missing in an older hub (hub-web may roll out earlier). */
  repos?: DocumentRepo[];
  items: DocumentRow[];
};
export type DocumentRef ={ repo: string; path: string; role: string; title: string; rel: string };
export type Ticket = TicketRow & TicketFields & { body: string; transcript_ref: string | null; documents: DocumentRef[] };
/** Person from /api/people: tickets and active count live assignments (all, and open or doing). */
export type Person = {
  handle: string;
  name: string;
  email: string | null;
  aliases: string[];
  is_agent: boolean;
  retired: boolean;
  tickets: number;
  active: number;
};
/** Business component from /api/components. */
export type Component = {
  slug: string;
  title: string;
  description: string | null;
  project: string | null;
  retired: boolean;
  tickets: number;
  active: number;
};
/** The Graph view from /api/graph: hub computes node layer and order. */
export type GraphNode = {
  code: string;
  project: string;
  summary: string;
  status: string;
  priority: string | null;
  layer: number;
  order: number;
};
export type GraphEdge = { id: number; src: string; dst: string; rel: string; state: string };
export type Graph = { project?: string; root?: string; layers: number; truncated: boolean; nodes: GraphNode[]; edges: GraphEdge[] };
export type ProjectCount = { project: string; tickets: number };
export type DocumentRow = {
  repo: string;
  path: string;
  role: string;
  title: string;
  created_at: string;
  retired_at: string | null;
  tickets: number;
};
export type TicketRef = { code: string; project: string; summary: string; status: string; rel: string };
/** Ticket edge from /api/tickets/{code}/links: dir out = the ticket is src, in = dst. */
export type TicketLink = {
  id: number;
  rel: string;
  state: 'proposed' | 'confirmed';
  dir: 'in' | 'out';
  kind: string;
  key: string;
  summary: string | null;
  status: string | null;
  project: string | null;
  created_by: string;
  created_at: string;
};
export type TicketLinks = {
  code: string;
  open_blockers: number;
  open_children: number;
  links: TicketLink[];
  /** Documents that describe the ticket (describes) or close it (closes). */
  documents: DocumentRef[];
};
/** Live edge from a document to another document: dir out = this document is src. */
export type DocumentLink = {
  edge_id: number;
  rel: string;
  dir: 'in' | 'out';
  state: 'proposed' | 'confirmed';
  repo: string;
  path: string;
  title: string;
  role: string;
  retired: boolean;
};
export type Document = Omit<DocumentRow, 'tickets'> & {
  body: string;
  sha256: string;
  source_revision: string | null;
  synced_at: string;
  /** Project from the repo registry (in_project edge). */
  project?: string | null;
  /** Missing in an older hub. */
  documents?: DocumentLink[];
  tickets: TicketRef[];
};

// The hub address comes from AppHost in the HUB_HTTP environment variable.
// process.env rather than the SvelteKit env module: the value is needed at startup, not at build time.
export function hubUrl(path: string): string {
  const hub = process.env.HUB_HTTP;
  if (!hub) error(500, 'HUB_HTTP is not set');
  return `${hub}${path}`;
}

// hub lets only known callers into /api: name in X-Hub-Caller, secret in Authorization.
// The secret is a file (HUB_CALLER_SECRET_FILE, volume from ExternalSecret hub-caller-hub-web, secret store key
// from the chart values) reread every 30 seconds: a rotated secret arrives without a restart.
// The previous secret is kept for 15 minutes: if hub has not reread its catalog yet and answers 401, the request
// is retried with the previous one (hubFetch). No file (local run): the request goes without a secret, and a hub
// without secrets lets it in.
const CALLER = 'hub-web';
export const SECRET_TTL_MS = 30_000;
export const PREVIOUS_SECRET_MS = 15 * 60_000;
let secretCache: { at: number; value: string; previous?: { value: string; until: number } } | undefined;
let warnedNoSecret = false;

function readSecret(): string {
  const file = process.env.HUB_CALLER_SECRET_FILE;
  if (!file) return '';
  try {
    return readFileSync(file, 'utf8').trim();
  } catch {
    return '';
  }
}

function callerSecret(): { value: string; previous?: string } {
  const now = Date.now();
  if (!secretCache || now - secretCache.at >= SECRET_TTL_MS) {
    const value = readSecret();
    if (!value && !warnedNoSecret) {
      warnedNoSecret = true;
      console.warn('hub caller secret is not set (HUB_CALLER_SECRET_FILE): requests to hub go without it');
    }
    let previous = secretCache?.previous;
    if (secretCache?.value && secretCache.value !== value) {
      previous = { value: secretCache.value, until: now + PREVIOUS_SECRET_MS };
    }
    secretCache = { at: now, value, previous };
  }
  const prev = secretCache.previous;
  return { value: secretCache.value, previous: prev && now < prev.until ? prev.value : undefined };
}

function withSecret(extra: Record<string, string>, secret: string): Record<string, string> {
  return { ...extra, 'x-hub-caller': CALLER, ...(secret ? { authorization: `Bearer ${secret}` } : {}) };
}

/** Headers of a request to hub: the caller name and its current secret. */
export function hubHeaders(extra: Record<string, string> = {}): Record<string, string> {
  return withSecret(extra, callerSecret().value);
}

/** Request to hub with caller headers; on 401 one retry with the previous secret if the secret changed recently. */
export async function hubFetch(
  fetch: typeof globalThis.fetch,
  path: string,
  init: { method?: string; headers?: Record<string, string>; body?: string } = {}
): Promise<Response> {
  const url = hubUrl(path);
  const secret = callerSecret();
  const res = await fetch(url, { ...init, headers: withSecret(init.headers ?? {}, secret.value) });
  if (res.status !== 401 || !secret.previous) return res;
  await res.body?.cancel();
  return fetch(url, { ...init, headers: withSecret(init.headers ?? {}, secret.previous) });
}

// Write actor (spec "Actor and trust"): hub writes it to the log, and the database uses it to decide who
// may confirm an edge. The kind is derived by the hub-web server, not the browser or the MCP client: human only for a
// form action with a live page session (formActor), agent for any MCP call. Without an actor (form without a
// session) hub records the write as service:hub-web.
// Freshness (shared memory spec "Human identity"): fresh for a person whose session was created at
// most FRESH_MS ago (session.createdAt); header x-hub-fresh: 1. Only hub-web knows the session, so it sets the flag;
// hub requires it to link a sign-in on /people and for mailbox key decisions on /agents, other forms work as before.
export type Actor = { kind: 'human' | 'agent'; id: string; fresh?: boolean };

export const FRESH_MS = 5 * 60_000;

/** Sign-in no older than FRESH_MS (and no more than a minute in the future: pod clocks may drift). */
export function isFresh(createdAt: number | null | undefined, now = Date.now()): boolean {
  return typeof createdAt === 'number' && Number.isFinite(createdAt) && now - createdAt <= FRESH_MS && createdAt - now <= 60_000;
}

/** The person behind a form: only with a live Better Auth session (locals.user), otherwise undefined. */
export function formActor(locals: App.Locals, now = Date.now()): Actor | undefined {
  return locals.user ? { kind: 'human', id: `user:${locals.user.id}`, fresh: isFresh(locals.sessionCreatedAt, now) } : undefined;
}

export function actorHeaders(actor?: Actor): Record<string, string> {
  if (!actor) return {};
  const headers: Record<string, string> = { 'x-hub-actor-kind': actor.kind, 'x-hub-actor': actor.id };
  if (actor.kind === 'human' && actor.fresh) headers['x-hub-fresh'] = '1';
  return headers;
}

/** 404 with an empty body: hub has no such route, i.e. the new hub is not rolled out yet ("no ticket" has a body). */
export async function missingRoute(res: Response): Promise<boolean> {
  return res.status === 404 && (await res.clone().text()).trim() === '';
}

// A non-2xx hub response goes to the log: status and the start of the body.
export async function refused(what: string, res: Response): Promise<string> {
  const text = await res.text();
  console.error(`hub ${what}: ${res.status} ${text.slice(0, 200)}`);
  return text;
}

// 400 and 404 from hub pass through as is, the rest becomes 502: the page is not to blame for a hub failure.
export async function getJson<T>(fetch: typeof globalThis.fetch, path: string): Promise<T> {
  const res = await hubFetch(fetch, path);
  if (!res.ok) {
    await refused(`GET ${path}`, res);
    error(res.status === 400 || res.status === 404 ? res.status : 502, `hub answered ${res.status}`);
  }
  return (await res.json()) as T;
}

/**
 * Optional part of a page (ticket links): the page lives without it. Route not in hub yet (rollout):
 * 'missing'; any other failure (5xx, lock timeout, network, bad JSON): 'failed' and a log line.
 */
export async function getOptionalJson<T>(fetch: typeof globalThis.fetch, path: string): Promise<T | 'missing' | 'failed'> {
  try {
    const res = await hubFetch(fetch, path);
    if (await missingRoute(res)) return 'missing';
    if (!res.ok) {
      await refused(`GET ${path}`, res);
      return 'failed';
    }
    return (await res.json()) as T;
  } catch (e) {
    console.error(`hub GET ${path}: ${e instanceof Error ? e.message : String(e)}`);
    return 'failed';
  }
}

export async function sendJson(
  fetch: typeof globalThis.fetch,
  method: 'POST' | 'PATCH' | 'PUT',
  path: string,
  body: unknown,
  actor?: Actor
): Promise<Response> {
  return hubFetch(fetch, path, {
    method,
    headers: { 'content-type': 'application/json', ...actorHeaders(actor) },
    body: JSON.stringify(body)
  });
}

export const STATUSES = ['open', 'doing', 'done', 'cancelled'] as const;
export const PRIORITIES = ['p0', 'p1', 'p2', 'p3'] as const;
