// Parsing of the "estimate" and "WSJF" forms of the ticket page. Bounds match EstimateIn::check and
// WsjfBody::check in services/hub/src/plan.rs (and CHECK in 0013_plan.sql) and the set_estimate, set_wsjf schemas in
// lib/server/mcp.ts. An empty field is not sent: PUT sets the whole estimate, what is missing is empty in the new version.
import { t } from '#lib/i18n/index';
import type { EstimateIn, WsjfIn } from '#lib/plan/types';

export const FIB = [1, 2, 3, 5, 8, 13, 21] as const;

type Field = (name: string) => string;
export type Parsed<T> = { ok: true; body: T } | { ok: false; error: string };

/** Form field value as a string (FormData.get or a record). */
export function fieldsOf(form: FormData | Record<string, string>): Field {
  return (name) => String((form instanceof FormData ? form.get(name) : form[name]) ?? '').trim();
}

/** Integer from a field: empty = undefined, not an integer = NaN. */
function int(v: string): number | undefined {
  if (v === '') return undefined;
  return /^-?\d+$/.test(v) ? Number(v) : NaN;
}

const inRange = (v: number | undefined, lo: number, hi: number) => v === undefined || (Number.isInteger(v) && v >= lo && v <= hi);
const fib = (v: number | undefined) => v !== undefined && (FIB as readonly number[]).includes(v);

export function parseEstimate(form: FormData | Record<string, string>): Parsed<EstimateIn> {
  const f = fieldsOf(form);
  const effort_sp = int(f('effort_sp'));
  const aggressive_hd = int(f('aggressive_hd'));
  const safe_hd = int(f('safe_hd'));
  const remaining_hd = int(f('remaining_hd'));
  const snb = f('start_not_before');
  if (!fib(effort_sp)) return { ok: false, error: t('plan.form.effort') };
  if (!inRange(aggressive_hd, 1, 1000)) return { ok: false, error: t('plan.form.aggressive') };
  if (!inRange(safe_hd, 1, 2000) || (safe_hd !== undefined && aggressive_hd !== undefined && safe_hd < aggressive_hd)) {
    return { ok: false, error: t('plan.form.safe') };
  }
  if (!inRange(remaining_hd, 0, 1000)) return { ok: false, error: t('plan.form.remaining') };
  if (snb !== '') {
    const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(snb);
    const valid = m && !Number.isNaN(Date.parse(`${snb}T00:00:00Z`)) && new Date(`${snb}T00:00:00Z`).toISOString().startsWith(snb);
    if (!m || !valid || Number(m[1]) < 2000 || Number(m[1]) > 2100) {
      return { ok: false, error: t('plan.form.startNotBefore') };
    }
  }
  const body: EstimateIn = { effort_sp: effort_sp! };
  if (aggressive_hd !== undefined) body.aggressive_hd = aggressive_hd;
  if (safe_hd !== undefined) body.safe_hd = safe_hd;
  if (remaining_hd !== undefined) body.remaining_hd = remaining_hd;
  if (snb !== '') body.start_not_before = snb;
  return { ok: true, body };
}

export function parseWsjf(form: FormData | Record<string, string>): Parsed<WsjfIn> {
  const f = fieldsOf(form);
  const business_value = int(f('business_value'));
  const time_criticality = int(f('time_criticality'));
  const risk_opportunity = int(f('risk_opportunity'));
  if (![business_value, time_criticality, risk_opportunity].every(fib)) {
    return { ok: false, error: t('plan.form.wsjf') };
  }
  return { ok: true, body: { business_value: business_value!, time_criticality: time_criticality!, risk_opportunity: risk_opportunity! } };
}
