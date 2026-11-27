// Shared types of the UI dictionaries (en.ts is the source of the key set, ru.ts must match it).

export const LOCALES = ['en', 'ru'] as const;
export type Locale = (typeof LOCALES)[number];

/** Plural forms by Intl.PluralRules category; `other` is always required. */
export type PluralForms = Partial<Record<Intl.LDMLPluralRule, string>> & { other: string };

/** A message: plain text with `{name}` placeholders, or plural forms selected by the `count` param. */
export type Message = string | PluralForms;

export type Messages = Record<string, Message>;
