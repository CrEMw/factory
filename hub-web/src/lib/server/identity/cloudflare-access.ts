// cloudflare-access entrance: Access admits people by its policy and signs the request with a JWT in Cf-Access-Jwt-Assertion.
// Here the signature (keys at https://<team domain>/cdn-cgi/access/certs), issuer https://<team domain> and the app
// audience are checked; the email comes from the JWT. Settings: AUTH_CF_ACCESS_TEAM_DOMAIN and AUTH_CF_ACCESS_AUD; without both the entrance is off. The JWT is accepted only on
// AUTH_CF_ACCESS_HOSTS hosts, by default on the ORIGIN host: the Cloudflare tunnel brings the request with that Host.
import { createRemoteJWKSet, jwtVerify, type JWTVerifyGetKey } from 'jose';
import { asIdentity, envValue, hostList, onHost, type EdgeProvider, type Env } from '#lib/server/identity/index';

const ACCESS_JWT = 'cf-access-jwt-assertion';

export type CloudflareAccessSettings = { teamDomain: string; aud: string; hosts: string[] };

export function cloudflareAccessSettings(env: Env, baseURL: string): CloudflareAccessSettings | undefined {
  const teamDomain = envValue(env, 'AUTH_CF_ACCESS_TEAM_DOMAIN');
  const aud = envValue(env, 'AUTH_CF_ACCESS_AUD');
  if (!teamDomain || !aud) return undefined;
  const hosts = hostList(env.AUTH_CF_ACCESS_HOSTS);
  return { teamDomain, aud, hosts: hosts.length ? hosts : [new URL(baseURL).hostname.toLowerCase()] };
}

const jwksByIssuer = new Map<string, JWTVerifyGetKey>();

function remoteJwks(issuer: string): JWTVerifyGetKey {
  let jwks = jwksByIssuer.get(issuer);
  if (!jwks) {
    jwks = createRemoteJWKSet(new URL(`${issuer}/cdn-cgi/access/certs`));
    jwksByIssuer.set(issuer, jwks);
  }
  return jwks;
}

/** jwks replaces the Access keys in tests; the server takes the keys from the team domain. */
export function cloudflareAccessProvider(s: CloudflareAccessSettings, jwks?: JWTVerifyGetKey): EdgeProvider {
  if (s.hosts.length === 0) throw new Error('cloudflare-access: at least one host is required');
  const hosts = s.hosts.map((x) => x.toLowerCase());
  const issuer = `https://${s.teamDomain}`;
  return {
    id: 'cloudflare-access',
    hosts,
    async identify(headers) {
      if (!onHost(headers, hosts)) return null;
      const token = headers.get(ACCESS_JWT);
      if (!token) return null;
      try {
        // Access signs with RS256; any other algorithm is rejected before the key check.
        const { payload } = await jwtVerify(token, jwks ?? remoteJwks(issuer), { issuer, audience: s.aud, algorithms: ['RS256'] });
        return typeof payload.email === 'string' ? asIdentity(payload.email, '', false, 'cloudflare-access') : null;
      } catch {
        return null;
      }
    }
  };
}
