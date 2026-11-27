import { Duplex } from 'node:stream';
import { constants, createBrotliCompress, createGzip } from 'node:zlib';
import type { Handle } from '@sveltejs/kit/hooks';
import { sequence } from '@sveltejs/kit/hooks';
import { building } from '$app/env';
import { svelteKitHandler } from 'better-auth/svelte-kit';
import { auth, hubName } from '#lib/server/auth';
import { addressMatcher, trustedProxyCidrs } from '#lib/server/identity/index';
import { locale } from '#lib/server/locale';

// Better Auth: /api/auth/* and OAuth discovery (/.well-known/oauth-authorization-server/...,
// /.well-known/oauth-protected-resource/mcp). Discovery lives at the root, outside basePath, so
// these requests go to auth.handler directly: the mcp plugin answers them in onRequest.
// The Better Auth rate limit uses the client IP from X-Forwarded-For (auth-config.ts). Once per
// process, a client request without a usable chain is logged: host, path, number of addresses, whether the last one
// is internal (in the TRUSTED_PROXY_CIDRS proxy networks), whether CF-Connecting-IP is present. Addresses themselves
// are not logged. Loopback calls of the pod to itself (JWKS keys for /mcp) are skipped: they have no client, and the
// Better Auth WARN about them is expected.
const isInternal = addressMatcher(trustedProxyCidrs(process.env));
const LOOPBACK = /^(127\.|::1$|::ffff:127\.)/;
let ipShapeLogged = false;
function logIpShape(event: Parameters<Handle>[0]['event']) {
  if (ipShapeLogged) return;
  let peer = '';
  try {
    peer = event.getClientAddress();
  } catch {
    // No adapter (build, tests) means no address.
  }
  if (LOOPBACK.test(peer)) return;
  const xff = (event.request.headers.get('x-forwarded-for') ?? '').split(',').map((x) => x.trim()).filter(Boolean);
  if (xff.length > 0 && !isInternal(xff[xff.length - 1])) return;
  ipShapeLogged = true;
  console.warn(
    `client ip: no usable x-forwarded-for host=${event.url.host} path=${event.url.pathname} xff_entries=${xff.length} ` +
      `last_internal=${xff.length > 0} cf_connecting_ip=${event.request.headers.has('cf-connecting-ip')} ` +
      `x_real_ip=${event.request.headers.has('x-real-ip')} forwarded=${event.request.headers.has('forwarded')}`
  );
}

const authHandle: Handle = async ({ event, resolve }) => {
  if (event.url.pathname !== '/health') logIpShape(event);
  if (event.url.pathname.startsWith('/.well-known/oauth-') || event.url.pathname.startsWith('/.well-known/openid-')) {
    return auth.handler(event.request);
  }
  // /health answers without the database; /mcp checks the token itself; the agent mailbox admits by path token and approved key.
  const path = event.url.pathname;
  const needsSession =
    !building && path !== '/health' && path !== '/mcp' && !path.startsWith('/api/auth/') && !path.startsWith('/api/mailbox/');
  const session = needsSession ? await auth.api.getSession({ headers: event.request.headers }) : null;
  event.locals.user = session?.user ?? null;
  // Sign-in freshness: linking on /people and key decisions on /agents need a sign-in no older than 5 minutes.
  event.locals.sessionCreatedAt = session ? new Date(session.session.createdAt).getTime() : null;
  return svelteKitHandler({ event, resolve, auth, building });
};

// Compression of page and JSON responses. Static /_app is precompressed (adapter-node precompress),
// but the ticket list HTML and hydration data are not: uncompressed, the home page weighed hundreds of kilobytes.
const COMPRESSIBLE = /^(text\/html|application\/json|text\/plain|text\/event-stream)/;
const MIN_BYTES = 1024;

/** br or gzip if the client accepts them (q=0 means refused). */
function pickEncoding(header: string): 'br' | 'gzip' | null {
  const accepted = new Set<string>();
  for (const part of header.toLowerCase().split(',')) {
    const [name, ...params] = part.trim().split(';');
    const q = params.map((p) => p.trim()).find((p) => p.startsWith('q='));
    if (q && Number(q.slice(2)) === 0) continue;
    accepted.add(name.trim());
  }
  return accepted.has('br') ? 'br' : accepted.has('gzip') ? 'gzip' : null;
}

const compressHandle: Handle = async ({ event, resolve }) => {
  const res = await resolve(event);
  if (!res.body || event.request.method === 'HEAD' || res.headers.has('content-encoding')) return res;
  const type = res.headers.get('content-type') ?? '';
  if (!COMPRESSIBLE.test(type) || type.startsWith('text/event-stream')) return res;
  const length = Number(res.headers.get('content-length') ?? NaN);
  if (length < MIN_BYTES) return res;
  const encoding = pickEncoding(event.request.headers.get('accept-encoding') ?? '');
  if (!encoding) return res;
  // Brotli quality 4: almost as small as 11 on HTML, but tens of times cheaper in CPU.
  const zip =
    encoding === 'br'
      ? createBrotliCompress({ params: { [constants.BROTLI_PARAM_QUALITY]: 4 } })
      : createGzip({ level: 6 });
  const { readable, writable } = Duplex.toWeb(zip) as unknown as ReadableWritablePair<Uint8Array, Uint8Array>;
  const headers = new Headers(res.headers);
  headers.set('content-encoding', encoding);
  headers.delete('content-length');
  headers.append('vary', 'Accept-Encoding');
  return new Response(res.body.pipeThrough({ readable, writable }), { status: res.status, statusText: res.statusText, headers });
};

// Form CSRF protection instead of the built-in one (vite.config.ts): POST/PUT/PATCH/DELETE with a form body are accepted
// only with the hub's own Origin: the main address (ORIGIN, i.e. url.origin) or another entrance (Better Auth trusted origins:
// AUTH_EXTRA_ORIGINS and entrance hosts, identity/index.ts). Exception: OAuth endpoints called by MCP
// clients themselves (code exchange, revocation, introspection): they have no cookies and no Origin, and Better Auth checks them itself.
const CSRF_EXEMPT = new Set(['/api/auth/oauth2/token', '/api/auth/oauth2/revoke', '/api/auth/oauth2/introspect']);
const FORM_TYPES = ['application/x-www-form-urlencoded', 'multipart/form-data', 'text/plain'];
const EXTRA_ORIGINS = new Set<string>(auth.options.trustedOrigins);

const csrfHandle: Handle = async ({ event, resolve }) => {
  const { request, url } = event;
  const type = request.headers.get('content-type')?.split(';')[0].trim().toLowerCase();
  const isForm = !type || FORM_TYPES.includes(type);
  if (
    isForm &&
    ['POST', 'PUT', 'PATCH', 'DELETE'].includes(request.method) &&
    !CSRF_EXEMPT.has(url.pathname) &&
    request.headers.get('origin') !== url.origin &&
    !EXTRA_ORIGINS.has(request.headers.get('origin') ?? '')
  ) {
    return new Response(`Cross-site ${request.method} form submissions are forbidden`, { status: 403 });
  }
  return resolve(event);
};

// <html lang> from the UI locale and the fallback <title> from the hub name (app.html has the %hub.lang% and
// %hub.name% placeholders).
const hubNameHtml = hubName.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
const langHandle: Handle = ({ event, resolve }) =>
  resolve(event, { transformPageChunk: ({ html }) => html.replace('%hub.lang%', locale).replace('%hub.name%', hubNameHtml) });

export const handle = sequence(csrfHandle, compressHandle, langHandle, authHandle);
