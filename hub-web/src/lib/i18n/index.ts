// UI language: t(key, params) over the en/ru dictionaries, plus locale-aware date helpers.
// The locale is one per process: the server sets it from HUB_LOCALE (lib/server/locale.ts, imported by
// hooks.server.ts), the root layout passes it to the client in layout data and sets it there before rendering.
import { en, type Key } from '#lib/i18n/en';
import { ru } from '#lib/i18n/ru';
import { LOCALES, type Locale, type Message } from '#lib/i18n/types';

export type { Key } from '#lib/i18n/en';
export type { Locale, Message, PluralForms } from '#lib/i18n/types';
export { LOCALES } from '#lib/i18n/types';

export const dictionaries: Record<Locale, Record<Key, Message>> = { en, ru };

// Without HUB_LOCALE the UI is English; an installation sets HUB_LOCALE=ru in its values.
export const DEFAULT_LOCALE: Locale = 'en';

let current: Locale = DEFAULT_LOCALE;

export function isLocale(value: unknown): value is Locale {
  return typeof value === 'string' && (LOCALES as readonly string[]).includes(value);
}

/** HUB_LOCALE value to a locale: empty means the default, anything unknown is a configuration error. */
export function parseLocale(value: string | undefined): Locale {
  const v = value?.trim() ?? '';
  if (!v) return DEFAULT_LOCALE;
  if (!isLocale(v)) throw new Error(`HUB_LOCALE must be one of ${LOCALES.join(', ')}, got "${v}"`);
  return v;
}

export function setLocale(locale: Locale): void {
  current = locale;
}

export function getLocale(): Locale {
  return current;
}

export type Params = Record<string, string | number>;

const pluralRules = new Map<Locale, Intl.PluralRules>();
function pluralOf(locale: Locale, count: number): Intl.LDMLPluralRule {
  let rules = pluralRules.get(locale);
  if (!rules) pluralRules.set(locale, (rules = new Intl.PluralRules(locale)));
  return rules.select(count);
}

/** Fills `{name}` placeholders; a placeholder without a param stays as is. */
export function interpolate(text: string, params?: Params): string {
  if (!params) return text;
  return text.replace(/\{(\w+)\}/g, (whole, name: string) => (name in params ? String(params[name]) : whole));
}

/** Message `key` in the current locale. Plural messages take the form for `params.count`. */
export function t(key: Key, params?: Params): string {
  return translate(current, key, params);
}

/** Same as t() for an explicit locale (tests, rendering both languages). */
export function translate(locale: Locale, key: Key, params?: Params): string {
  const message = dictionaries[locale][key];
  if (message === undefined) throw new Error(`i18n: no message "${key}" in ${locale}`);
  return format(locale, message, params, key);
}

/** Renders one message: picks the plural form for `params.count`, then fills placeholders. */
export function format(locale: Locale, message: Message, params?: Params, key = 'message'): string {
  if (typeof message === 'string') return interpolate(message, params);
  const count = Number(params?.count);
  if (!Number.isFinite(count)) throw new Error(`i18n: plural ${key} needs a numeric count`);
  return interpolate(message[pluralOf(locale, count)] ?? message.other, params);
}

/** Intl.DateTimeFormat in the current locale (BCP 47 tag equals the locale code). */
export function dateFormat(options: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
  return new Intl.DateTimeFormat(current, options);
}

/** Short day label for 'YYYY-MM-DD': 'dd.mm' in ru, 'mm/dd' in en. */
export function shortDate(date: string, locale: Locale = current): string {
  const [, m, d] = date.split('-');
  return locale === 'ru' ? `${d}.${m}` : `${m}/${d}`;
}
