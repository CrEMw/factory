// Gantt geometry on a fixed plan (fixtures/plan.json, response shape of GET /api/plan/{root}).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { setLocale } from '#lib/i18n/index';
import { dueHalfDay } from '#lib/plan/calendar';
import { connector, depsFromGraph, ganttLayout, HD_PX, HEAD_H, LABEL_W, makeScale, ROW_H, rowOrder, ticks, type TaskRow } from '#lib/plan/gantt';
import type { PlanResponse } from '#lib/plan/types';

const fixture = (name: string) => JSON.parse(readFileSync(new URL(`./fixtures/${name}`, import.meta.url), 'utf8'));
const p: PlanResponse = fixture('plan.json');
const graph = fixture('graph.json');

setLocale('en');

test('scale: from today to the due date and end of PB, on whole days', () => {
  const s = makeScale(p.plan, dueHalfDay(p.scope.due_date!));
  assert.equal(s.from, 3150);
  // Due 2030-01-29 (Tuesday), end of day = start of 2030-01-30 = 3174, end of PB 3170.
  assert.equal(s.to, 3174);
  assert.equal(s.x(3150), LABEL_W);
  assert.equal(s.x(3154), LABEL_W + 4 * HD_PX);
  assert.equal(s.width, LABEL_W + 24 * HD_PX + 24);
  // An odd edge (half-day) moves to the start of the day.
  const odd = makeScale({ ...p.plan, today: 3151, tasks: [], buffers: [], end: 3153 });
  assert.deepEqual([odd.from, odd.to], [3150, 3154]);
});

test('ticks: labels on Mondays and the first day, the last edge unlabelled', () => {
  const short = ticks(makeScale(p.plan, null));
  // Up to 10 days every day is labelled.
  assert.equal(short.filter((x) => x.label).length, 10);
  const s = makeScale(p.plan, dueHalfDay(p.scope.due_date!));
  const t = ticks(s);
  assert.equal(t.length, (s.to - s.from) / 2 + 1);
  assert.equal(t[0].label, '01/14');
  assert.equal(t[0].major, true);
  assert.equal(t.at(-1)!.label, null);
  // More than 10 days: only Mondays are labelled.
  assert.deepEqual(
    t.filter((x) => x.label).map((x) => x.label),
    ['01/14', '01/21', '01/28']
  );
});

test('rows by task: umbrella on top, tasks by start, a buffer right after its last task', () => {
  const order = rowOrder(p.plan).map((s) => (s.kind === 'task' ? s.task.code : s.kind === 'buffer' ? s.buffer.key : '#' + s.label));
  assert.deepEqual(order, ['tr00', 'ta02', 'ta03', 'ta04', 'fb:105', 'ta06', 'fb:finish', 'ta05', 'pb']);
});

test('rows by person: groups by handle, unassigned last, no umbrella, buffers separately', () => {
  const order = rowOrder(p.plan, 'person').map((s) => (s.kind === 'task' ? s.task.code : s.kind === 'buffer' ? s.buffer.key : '#' + s.label));
  assert.deepEqual(order, ['#@alice', 'ta02', 'ta04', '#@bob', 'ta03', 'ta05', '#unassigned', 'ta06', '#buffers', 'fb:105', 'fb:finish', 'pb']);
});

test('layout: bars, chain, hatching, external blocker milestone, arrows, today and due date', () => {
  const g = ganttLayout({
    plan: p.plan,
    due: dueHalfDay(p.scope.due_date!),
    deps: depsFromGraph(graph.edges),
    summaries: Object.fromEntries(graph.nodes.map((n: { code: string; summary: string }) => [n.code, n.summary]))
  });
  assert.equal(g.rows.length, 9);
  assert.equal(g.height, HEAD_H + 9 * ROW_H + 8);
  const row = (code: string) => g.rows.find((r): r is TaskRow => r.kind === 'task' && r.task.code === code)!;
  assert.equal(row('ta03').x, LABEL_W + 4 * HD_PX);
  assert.equal(row('ta03').w, 6 * HD_PX);
  assert.equal(row('ta03').chain, true);
  assert.equal(row('ta04').chain, false);
  assert.equal(row('ta04').defaulted, true);
  assert.equal(row('tr00').umbrella, true);
  assert.deepEqual(row('tr00').flags, []);
  assert.match(row('ta03').title, /^ta03: Chain computation · open · 01\/16 – 01\/18, 6 half-days · @bob · a 6, s 12 · critical chain$/);
  assert.equal(row('ta06').milestone?.x, LABEL_W + 9 * HD_PX);
  assert.match(row('ta06').milestone!.title, /ta06 waits for tx09: no date/);
  assert.equal(row('ta05').milestone, null);
  // Dependencies from confirmed blocks inside the plan (the proposed edge and tx09 outside the plan are not drawn) and the resource link ta03 -> ta05.
  assert.deepEqual(
    g.arrows.map((a) => [a.title.split(':')[0], a.resource]),
    [
      ['ta02 blocks ta03', false],
      ['ta04 blocks ta05', false],
      ['resource link', true]
    ]
  );
  const buf = g.rows.find((r) => r.kind === 'buffer' && r.buffer.key === 'fb:105');
  assert.ok(buf && buf.kind === 'buffer');
  assert.equal(buf.label, 'FB before ta05');
  assert.match(buf.title, /green zone/);
  assert.equal(g.today, LABEL_W);
  assert.equal(g.due, LABEL_W + 24 * HD_PX);
  assert.equal(g.end, LABEL_W + 20 * HD_PX);
  assert.deepEqual(
    g.unscheduled.map((t) => t.code),
    ['ta01', 'tc01']
  );
});

test('connector: forward with a step, flush from the bar edge, backwards around between rows, straight in the same row', () => {
  assert.equal(connector(100, 13, 140, 39), 'M100,13 H106 V39 H138');
  assert.equal(connector(100, 13, 100, 39), 'M97,21 V39 H98');
  assert.equal(connector(100, 39, 104, 13), 'M97,31 V13 H102');
  assert.equal(connector(100, 13, 80, 39), 'M100,13 H106 V26 H72 V39 H78');
  assert.equal(connector(100, 13, 130, 13), 'M100,13 H128');
});
