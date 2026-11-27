// Plan calendar as in services/hub/src/ccpm/model.rs (half_day_of, date_of): working half-day from
// Monday 2024-01-01, Mon-Fri; Saturday and Sunday map to the first half-day of the next Monday.
import { shortDate as localShortDate } from '#lib/i18n/index';
import type { HalfDay } from '#lib/plan/types';

const EPOCH_MS = Date.UTC(2024, 0, 1);
const DAY_MS = 86_400_000;

const pad = (n: number) => String(n).padStart(2, '0');

/** First half-day of the day 'YYYY-MM-DD'. */
export function halfDayOf(date: string): HalfDay {
  const [y, m, d] = date.split('-').map(Number);
  const days = Math.round((Date.UTC(y, m - 1, d) - EPOCH_MS) / DAY_MS);
  const weeks = Math.floor(days / 7);
  const weekday = Math.min(((days % 7) + 7) % 7, 5);
  return (weeks * 5 + weekday) * 2;
}

/** Calendar day 'YYYY-MM-DD' the half-day falls on. */
export function dateOf(hd: HalfDay): string {
  const workday = Math.floor(hd / 2);
  const days = Math.floor(workday / 5) * 7 + (((workday % 5) + 5) % 5);
  const t = new Date(EPOCH_MS + days * DAY_MS);
  return `${t.getUTCFullYear()}-${pad(t.getUTCMonth() + 1)}-${pad(t.getUTCDate())}`;
}

/** Weekday of the half-day: 0 Monday .. 4 Friday. */
export function weekdayOf(hd: HalfDay): number {
  return ((Math.floor(hd / 2) % 5) + 5) % 5;
}

/** Short day label in the UI locale (lib/i18n shortDate). */
export function shortDate(date: string): string {
  return localShortDate(date);
}

/** Scope due_date means the end of that day: the first half-day of the next working day (plan.rs config). */
export function dueHalfDay(due: string): HalfDay {
  const [y, m, d] = due.split('-').map(Number);
  const next = new Date(Date.UTC(y, m - 1, d) + DAY_MS);
  return halfDayOf(`${next.getUTCFullYear()}-${pad(next.getUTCMonth() + 1)}-${pad(next.getUTCDate())}`);
}
