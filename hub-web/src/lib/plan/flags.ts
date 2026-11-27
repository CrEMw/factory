// Task and plan flags (services/hub/src/ccpm/*.rs, "Critical chain" spec) in human words.
// An unknown flag is shown as is: a new hub may roll out before the page.
import { t, type Key } from '#lib/i18n/index';

const FLAGS = [
  'estimate_defaulted',
  'remaining_unknown',
  'wsjf_missing',
  'external_blocker',
  'external_milestone',
  'chain_drift',
  'chain_gap',
  'compressed',
  'cross_scope_overlap',
  'duplicate',
  'in_cycle',
  'late',
  'multitasking',
  'placement_unconverged',
  'scope_change',
  'started_while_blocked',
  'umbrella',
  'unassigned'
] as const;

/** Name lookup by id that translates on every read (t() must not run at module load). Unknown id: undefined. */
function names(prefix: string, ids: readonly string[]): Record<string, string> {
  const known = (k: string | symbol): k is string => typeof k === 'string' && ids.includes(k);
  return new Proxy({} as Record<string, string>, {
    get: (_, k) => (known(k) ? t(`${prefix}.${k}` as Key) : undefined),
    has: (_, k) => known(k)
  });
}

export function flagName(flag: string): string {
  return (FLAGS as readonly string[]).includes(flag) ? t(`plan.flag.${flag as (typeof FLAGS)[number]}`) : flag;
}

/** Flags worth showing on the bar (an umbrella is already visible as a summary bar). */
export function visibleFlags(flags: string[]): string[] {
  return flags.filter((f) => f !== 'umbrella');
}

export const ZONE_NAMES = names('plan.zone', ['green', 'yellow', 'red']);

export const WARNING_NAMES = names('plan.warning', ['prev_half_done', 'starts_soon']);

export const STATUS_NAMES = names('plan.status', ['open', 'doing', 'done', 'cancelled']);

/** WSJF in hundredths: 200 -> '2.00'. */
export function wsjfText(v: number | null | undefined): string {
  return v == null ? '—' : (v / 100).toFixed(2);
}

/** Buffer name by key: pb, fb:finish, fb:<seq>. */
export function bufferName(key: string, codeOf: (seq: number) => string | undefined = () => undefined): string {
  if (key === 'pb') return t('plan.buffer.project');
  if (key === 'fb:finish') return t('plan.buffer.finish');
  const seq = Number(key.slice(3));
  return t('plan.buffer.before', { code: codeOf(seq) ?? key.slice(3) });
}
