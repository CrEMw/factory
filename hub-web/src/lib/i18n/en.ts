// English UI dictionary: the source of truth for the key set. ru.ts must have exactly the same keys (TypeScript
// `satisfies` in ru.ts and the test in i18n.test.ts enforce it).
//
// Adding strings:
// - Each group has one module en/<group>.ts (core, plan, tickets, docs, auth, agents). Keys are flat and start with
//   the group name: 'plan.gantt.today', 'tickets.form.title'. core holds the layout and strings shared by groups.
// - Values are plain text with `{name}` placeholders, or plural forms `{ one, other }` chosen by the `count` param
//   (Intl.PluralRules of the locale; ru needs `one`, `few`, `many`, `other`). ru must use the same placeholders.
// - Russian lives only in ru.ts (the single file allowed to contain Cyrillic, tools/no-cyrillic.sh): add the Russian
//   entry to the matching section of ru.ts. Text taken from old code is copied exactly, so ru renders unchanged.
// - Call t() at render or call time, never at module top level: on the client the locale is set by the root
//   layout, after modules are evaluated.
import type { Messages } from '#lib/i18n/types';
import core from '#lib/i18n/en/core';
import plan from '#lib/i18n/en/plan';
import tickets from '#lib/i18n/en/tickets';
import docs from '#lib/i18n/en/docs';
import auth from '#lib/i18n/en/auth';
import agents from '#lib/i18n/en/agents';

export const en = { ...core, ...plan, ...tickets, ...docs, ...auth, ...agents } satisfies Messages;

export type Key = keyof typeof en;
