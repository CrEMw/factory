import adapter from '@sveltejs/adapter-node';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// tracing.server: SvelteKit makes the request, load and action spans itself; src/instrumentation.server.ts starts the SDK.
// csrf: the built-in Origin check is off and repeated in src/hooks.server.ts (csrfHandle). The built-in one
// has no exceptions, and MCP clients send form-urlencoded POSTs to /api/auth/oauth2/token without Origin.
export default defineConfig({
  plugins: [sveltekit({ adapter: adapter(), tracing: { server: true }, csrf: { trustedOrigins: ['*'] } })]
});
