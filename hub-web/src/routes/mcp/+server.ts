import { requireMcpAuth } from '@better-auth/mcp';
import { auth } from '#lib/server/auth';
import { mcpHandler } from '#lib/server/mcp';
import type { RequestHandler } from './$types';

// MCP over stateless Streamable HTTP: POST only. The token is a Better Auth JWT bound to this address
// (audience). Without a token the client gets 401 with WWW-Authenticate and finds the authorization server
// via /.well-known/oauth-protected-resource/mcp.
let protectedHandler: ((req: Request) => Promise<Response>) | undefined;

function handler() {
  protectedHandler ??= requireMcpAuth(
    auth,
    // sub (the user) reaches the tools via authInfo: mailbox writes on their behalf.
    (req, claims) =>
      mcpHandler.fetch(req, {
        authInfo: { token: '', clientId: String(claims.azp ?? claims.client_id ?? ''), scopes: [], extra: { sub: claims.sub } }
      }),
    {
    resource: `${auth.options.baseURL}/mcp`,
    // Keys come from the pod itself: the external hub address (tailnet) is unreachable from the cluster.
    jwksUrl: `http://127.0.0.1:${process.env.PORT ?? '8001'}/api/auth/jwks`
  });
  return protectedHandler;
}

export const POST: RequestHandler = ({ request }) => handler()(request);

const notAllowed: RequestHandler = () => new Response(null, { status: 405, headers: { allow: 'POST' } });
export const GET = notAllowed;
export const DELETE = notAllowed;
