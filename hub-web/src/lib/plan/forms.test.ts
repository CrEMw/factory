// Estimate and WSJF forms (bounds of EstimateIn::check, WsjfBody::check in plan.rs), the baseline
// approval answer and the person of the "Next" page.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { setLocale } from '#lib/i18n/index';
import { approveOutcome } from '#lib/plan/approve';
import { parseEstimate, parseWsjf } from '#lib/plan/forms';
import { personFor } from '#lib/plan/person';
import type { Person } from '#lib/server/hub';

setLocale('en');

test('estimate: points only, all fields, empty ones are not sent', () => {
  assert.deepEqual(parseEstimate({ effort_sp: '5' }), { ok: true, body: { effort_sp: 5 } });
  const fd = new FormData();
  for (const [k, v] of Object.entries({ effort_sp: '8', aggressive_hd: '4', safe_hd: '4', remaining_hd: '0', start_not_before: '2030-01-22' })) fd.set(k, v);
  assert.deepEqual(parseEstimate(fd), {
    ok: true,
    body: { effort_sp: 8, aggressive_hd: 4, safe_hd: 4, remaining_hd: 0, start_not_before: '2030-01-22' }
  });
  assert.deepEqual(parseEstimate({ effort_sp: '3', aggressive_hd: ' ', safe_hd: '' }), { ok: true, body: { effort_sp: 3 } });
});

test('estimate: bounds as in hub', () => {
  const bad = (f: Record<string, string>) => assert.equal(parseEstimate({ effort_sp: '3', ...f }).ok, false, JSON.stringify(f));
  for (const sp of ['', '4', '0', '34', '2.5', 'x']) assert.equal(parseEstimate({ effort_sp: sp }).ok, false, sp);
  bad({ aggressive_hd: '0' });
  bad({ aggressive_hd: '1001' });
  bad({ aggressive_hd: '1.5' });
  bad({ safe_hd: '2001' });
  bad({ safe_hd: '0' });
  bad({ aggressive_hd: '5', safe_hd: '4' });
  bad({ remaining_hd: '-1' });
  bad({ remaining_hd: '1001' });
  bad({ start_not_before: '1999-12-31' });
  bad({ start_not_before: '2101-01-01' });
  bad({ start_not_before: '2030-02-30' });
  bad({ start_not_before: '22.01.2030' });
  assert.equal(parseEstimate({ effort_sp: '21', aggressive_hd: '1000', safe_hd: '2000', remaining_hd: '1000', start_not_before: '2100-12-31' }).ok, true);
  assert.equal(parseEstimate({ effort_sp: '1', safe_hd: '1' }).ok, true);
});

test('WSJF: three Fibonacci numbers', () => {
  assert.deepEqual(parseWsjf({ business_value: '8', time_criticality: '5', risk_opportunity: '3' }), {
    ok: true,
    body: { business_value: 8, time_criticality: 5, risk_opportunity: 3 }
  });
  assert.equal(parseWsjf({ business_value: '8', time_criticality: '5' }).ok, false);
  assert.equal(parseWsjf({ business_value: '4', time_criticality: '5', risk_opportunity: '3' }).ok, false);
});

test('baseline approval: 201, default estimate threshold, human only, nothing to approve, rollout', () => {
  assert.deepEqual(approveOutcome(201, '{"root":"tr00"}'), { kind: 'approved' });
  assert.deepEqual(approveOutcome(409, '{"error":"defaults_over_threshold","defaulted_pct":100,"max_defaulted_pct":20}'), {
    kind: 'defaults',
    defaulted_pct: 100,
    max_defaulted_pct: 20
  });
  const human = approveOutcome(403, '{"error":"approve_requires_human"}');
  assert.ok(human.kind === 'error' && /Only a human/.test(human.message));
  const nothing = approveOutcome(409, '{"error":"nothing_to_approve"}');
  assert.ok(nothing.kind === 'error' && /Nothing to approve/.test(nothing.message));
  const rolling = approveOutcome(404, '');
  assert.ok(rolling.kind === 'error' && /still rolling out/.test(rolling.message));
  const other = approveOutcome(500, 'internal error');
  assert.ok(other.kind === 'error' && other.message === 'hub: 500 internal error');
});

test('person of the "Next" page: by session email or alias, not an agent and not retired', () => {
  const person = (handle: string, extra: Partial<Person>): Person => ({
    handle,
    name: handle,
    email: null,
    aliases: [],
    is_agent: false,
    retired: false,
    tickets: 0,
    active: 0,
    ...extra
  });
  const people = [
    person('bot', { email: 'alice@example.org', is_agent: true }),
    person('old', { email: 'alice@example.org', retired: true }),
    person('alice', { email: 'Alice@Example.org' }),
    person('bob', { aliases: ['bob@example.org'] })
  ];
  assert.equal(personFor({ email: 'alice@example.org' }, people), 'alice');
  assert.equal(personFor({ email: ' BOB@example.org ' }, people), 'bob');
  assert.equal(personFor({ email: 'nobody@example.org' }, people), null);
  assert.equal(personFor(null, people), null);
});
