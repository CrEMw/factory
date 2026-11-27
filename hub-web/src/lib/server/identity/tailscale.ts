// tailscale entrance: the Tailscale proxy (ingress) strips incoming Tailscale-User-* and sets its own for user
// devices: Tailscale-User-Login, Tailscale-User-Name. Tagged devices do not get them and cannot sign in.
// The headers are trusted only on AUTH_TAILSCALE_HOSTS hosts: on any other host anyone
// could have sent them. Without hosts the entrance is off.
import { asIdentity, envValue, hostList, onHost, type EdgeProvider, type Env } from '#lib/server/identity/index';

const LOGIN = 'tailscale-user-login';
const NAME = 'tailscale-user-name';

export type TailscaleSettings = { hosts: string[] };

export function tailscaleSettings(env: Env): TailscaleSettings | undefined {
  const hosts = hostList(envValue(env, 'AUTH_TAILSCALE_HOSTS'));
  return hosts.length ? { hosts } : undefined;
}

/** The name in the Tailscale header is RFC 2047 encoded (=?utf-8?q?...?=) when it is not ASCII. */
export function decodeHeaderWord(v: string): string {
  const m = /^=\?utf-8\?([qb])\?(.*)\?=$/i.exec(v.trim());
  if (!m) return v;
  if (m[1].toLowerCase() === 'b') return Buffer.from(m[2], 'base64').toString('utf8');
  const bytes = m[2].replace(/_/g, ' ').replace(/=([0-9a-f]{2})/gi, (_, h: string) => String.fromCharCode(parseInt(h, 16)));
  return Buffer.from(bytes, 'latin1').toString('utf8');
}

export function tailscaleProvider(s: TailscaleSettings): EdgeProvider {
  if (s.hosts.length === 0) throw new Error('tailscale: at least one host is required');
  const hosts = s.hosts.map((x) => x.toLowerCase());
  return {
    id: 'tailscale',
    hosts,
    // The host is checked here too, not only in edgeIdentity: the provider does not trust headers whoever calls it.
    async identify(headers) {
      if (!onHost(headers, hosts)) return null;
      const header = headers.get(NAME);
      return asIdentity(headers.get(LOGIN) ?? undefined, decodeHeaderWord(header ?? ''), !!header, 'tailscale');
    }
  };
}
