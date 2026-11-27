import { test } from 'node:test';
import assert from 'node:assert/strict';
import { dictionaries, format, interpolate, LOCALES, parseLocale, setLocale, shortDate, t, translate, DEFAULT_LOCALE, type Key } from '#lib/i18n/index';
import { en } from '#lib/i18n/en';

const keys = Object.keys(en) as Key[];
const placeholders = (text: string) => [...text.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
const texts = (m: unknown) => (typeof m === 'string' ? [m] : Object.values(m as object));

test('every locale has exactly the keys of en', () => {
  for (const locale of LOCALES) {
    assert.deepEqual(Object.keys(dictionaries[locale]).sort(), [...keys].sort(), locale);
  }
});

test('keys carry a group prefix and messages are not empty', () => {
  for (const key of keys) assert.match(key, /^(core|plan|tickets|docs|auth|agents)\.[\w.-]+$/, key);
  for (const locale of LOCALES) {
    for (const key of keys) for (const text of texts(dictionaries[locale][key])) assert.ok(text.length > 0, `${locale} ${key}`);
  }
});

test('placeholders and plural shape match en in every locale', () => {
  for (const locale of LOCALES) {
    const categories = new Intl.PluralRules(locale).resolvedOptions().pluralCategories;
    for (const key of keys) {
      const source = en[key] as unknown;
      const message = dictionaries[locale][key];
      assert.equal(typeof message, typeof source, `${locale} ${key}: plural in one locale only`);
      const want = placeholders(texts(source).join(' '));
      for (const text of texts(message)) {
        for (const p of placeholders(text)) assert.ok(want.includes(p) || p === 'count', `${locale} ${key}: unknown {${p}}`);
      }
      if (typeof message !== 'string') {
        for (const c of categories) assert.ok(c in message, `${locale} ${key}: no plural form "${c}"`);
      }
    }
  }
});

test('t interpolates params and follows the current locale', () => {
  setLocale('en');
  assert.equal(t('core.nav.tickets'), 'Tickets');
  setLocale('ru');
  assert.equal(t('core.nav.tickets'), translate('ru', 'core.nav.tickets'));
  setLocale(DEFAULT_LOCALE);
});

test('parseLocale: empty is the default, unknown fails', () => {
  assert.equal(DEFAULT_LOCALE, 'en');
  assert.equal(parseLocale(undefined), DEFAULT_LOCALE);
  assert.equal(parseLocale(' '), DEFAULT_LOCALE);
  assert.equal(parseLocale('en'), 'en');
  assert.equal(parseLocale('ru'), 'ru');
  assert.throws(() => parseLocale('de'), /HUB_LOCALE/);
});

test('shortDate by locale', () => {
  assert.equal(shortDate('2030-01-05', 'ru'), '05.01');
  assert.equal(shortDate('2030-01-05', 'en'), '01/05');
});

test('interpolate and plural format', () => {
  assert.equal(interpolate('{n} of {total}, {missing}', { n: 2, total: 5 }), '2 of 5, {missing}');
  const files = { one: '{count} file', other: '{count} files' };
  assert.equal(format('en', files, { count: 1 }), '1 file');
  assert.equal(format('en', files, { count: 3 }), '3 files');
  const ruForms = { one: '{count} one', few: '{count} few', many: '{count} many', other: '{count} other' };
  assert.deepEqual([1, 3, 5, 21, 1.5].map((count) => format('ru', ruForms, { count })), ['1 one', '3 few', '5 many', '21 one', '1.5 other']);
  assert.throws(() => format('en', files), /count/);
});
