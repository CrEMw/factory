// dev entrance: a fixed login without an entrance in front of the hub, for local runs (Aspire sets AUTH_DEV_LOGIN).
// It is trusted on any host, so it is refused with NODE_ENV=production: such settings stop the
// pod from starting instead of opening the hub to anyone.
import { asIdentity, envValue, type EdgeProvider, type Env } from '#lib/server/identity/index';

export type DevSettings = { login: string };

function refuseInProduction(nodeEnv: string | undefined) {
  if (nodeEnv === 'production') throw new Error('AUTH_DEV_LOGIN is refused when NODE_ENV=production');
}

export function devSettings(env: Env): DevSettings | undefined {
  const login = envValue(env, 'AUTH_DEV_LOGIN');
  if (!login) return undefined;
  refuseInProduction(env.NODE_ENV);
  return { login };
}

export function devProvider(s: DevSettings, nodeEnv: string | undefined): EdgeProvider {
  refuseInProduction(nodeEnv);
  return {
    id: 'dev',
    hosts: null,
    async identify() {
      return asIdentity(s.login, '', false, 'dev');
    }
  };
}
