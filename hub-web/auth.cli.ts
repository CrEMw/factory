// Only for `auth generate` (SQL of the Better Auth tables): the same config as the server.
import { createAuth } from './src/lib/server/auth-config';
export const auth = createAuth({
  baseURL: 'https://hub.example.org',
  secret: 'generate-only-not-a-secret-000000000000000000',
  databaseUrl: process.env.HUBDB_URI ?? 'postgres://postgres:x@127.0.0.1:55432/postgres'
});
