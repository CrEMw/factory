// The plan calendar matches services/hub/src/ccpm/model.rs (test calendar_skips_weekends).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { setLocale } from '#lib/i18n/index';
import { dateOf, dueHalfDay, halfDayOf, shortDate, weekdayOf } from '#lib/plan/calendar';

test('calendar: weekends skipped, epoch, before the epoch, half-days of a day', () => {
  const mon = halfDayOf('2030-01-07');
  const fri = halfDayOf('2030-01-11');
  assert.equal(fri - mon, 8);
  assert.equal(halfDayOf('2030-01-12'), halfDayOf('2030-01-14'));
  assert.equal(halfDayOf('2030-01-14') - fri, 2);
  for (const d of ['2030-01-07', '2030-01-11', '2030-01-14']) {
    assert.equal(dateOf(halfDayOf(d)), d);
    assert.equal(dateOf(halfDayOf(d) + 1), d);
  }
  assert.equal(halfDayOf('2024-01-01'), 0);
  assert.equal(halfDayOf('2023-12-29'), -2);
  assert.equal(dateOf(-2), '2023-12-29');
  // A plan on Sunday 2030-01-13: today 3150, end of PB 3168 = 2030-01-25 (end_date).
  assert.equal(halfDayOf('2030-01-13'), 3150);
  assert.equal(dateOf(3168), '2030-01-25');
  assert.equal(weekdayOf(3150), 0);
  assert.equal(weekdayOf(3159), 4);
  setLocale('en');
  assert.equal(shortDate('2030-01-25'), '01/25');
  setLocale('ru');
  assert.equal(shortDate('2030-01-25'), '25.01');
});

test('scope due date: end of day, that is the first half-day of the next working day', () => {
  assert.equal(dueHalfDay('2030-01-25'), halfDayOf('2030-01-28'));
  assert.equal(dueHalfDay('2030-01-29'), halfDayOf('2030-01-29') + 2);
});
