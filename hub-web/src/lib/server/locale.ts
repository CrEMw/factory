// UI locale of this process from HUB_LOCALE (en or ru; empty means the default in lib/i18n/index.ts).
// Importing this module sets the locale for server rendering; hooks.server.ts imports it at startup.
import { parseLocale, setLocale } from '#lib/i18n/index';

export const locale = parseLocale(process.env.HUB_LOCALE);
setLocale(locale);
