// Gantt geometry without DOM: half-day scale, row layout (by task or by person), buffers, dependency
// arrows and dashed resource links, external blocker milestones, "today" and due date lines. The Gantt.svelte
// component only draws what is computed here; tests in gantt.test.ts.
import { t as i18n } from '#lib/i18n/index';
import { dateOf, shortDate, weekdayOf } from '#lib/plan/calendar';
import { bufferName, flagName, STATUS_NAMES, visibleFlags, ZONE_NAMES } from '#lib/plan/flags';
import type { BufferPlan, HalfDay, Plan, TaskPlan } from '#lib/plan/types';

/** Pixels per half-day, row and bar height, label column width, scale height. */
export const HD_PX = 14;
export const ROW_H = 26;
export const BAR_H = 16;
export const LABEL_W = 76;
export const HEAD_H = 36;
const PAD_R = 24;
const PAD_B = 8;

/** Dependency "src blocks dst" by code (live confirmed blocks edges from /api/graph). */
export type Dep = { src: string; dst: string };

export type GanttInput = {
  plan: Plan;
  /** Scope due date (dueHalfDay), if set. */
  due?: HalfDay | null;
  deps?: Dep[];
  /** Ticket summary by code (from /api/graph), for tooltips. */
  summaries?: Record<string, string>;
  group?: 'task' | 'person';
};

export type Scale = { from: HalfDay; to: HalfDay; x: (hd: HalfDay) => number; width: number };

export type TaskRow = {
  kind: 'task';
  y: number;
  task: TaskPlan;
  x: number;
  w: number;
  chain: boolean;
  umbrella: boolean;
  defaulted: boolean;
  flags: string[];
  title: string;
  /** External blocker milestone before the task. */
  milestone: { x: number; title: string } | null;
};
export type BufferRow = { kind: 'buffer'; y: number; buffer: BufferPlan; x: number; w: number; label: string; title: string };
export type GroupRow = { kind: 'group'; y: number; label: string };
export type Row = TaskRow | BufferRow | GroupRow;

export type Arrow = { d: string; title: string; resource: boolean };
export type Tick = { x: number; label: string | null; major: boolean };

export type GanttLayout = {
  width: number;
  height: number;
  scale: Scale;
  rows: Row[];
  arrows: Arrow[];
  ticks: Tick[];
  today: number;
  due: number | null;
  end: number;
  /** Tasks without an interval (done, cancelled, in a cycle, duplicates). */
  unscheduled: TaskPlan[];
};

const even = (n: number) => n - (((n % 2) + 2) % 2);

/** Scale over all intervals, buffers, today, the due date and the plan end; bounds on whole days. */
export function makeScale(plan: Plan, due: HalfDay | null = null): Scale {
  const points: number[] = [plan.today, plan.end];
  for (const t of plan.tasks) if (t.start != null && t.end != null) points.push(t.start, t.end);
  for (const b of plan.buffers) points.push(b.start, b.end);
  if (due != null) points.push(due);
  const from = even(Math.min(...points));
  let to = even(Math.max(...points) + 1);
  if (to - from < 4) to = from + 4;
  return { from, to, x: (hd) => LABEL_W + (hd - from) * HD_PX, width: LABEL_W + (to - from) * HD_PX + PAD_R };
}

/** Daily ticks; labels on Mondays and on the first day, on every day of a short plan. */
export function ticks(scale: Scale): Tick[] {
  const out: Tick[] = [];
  const days = (scale.to - scale.from) / 2;
  for (let hd = scale.from; hd <= scale.to; hd += 2) {
    const monday = weekdayOf(hd) === 0;
    const label = days <= 10 || monday || hd === scale.from ? shortDate(dateOf(hd)) : null;
    out.push({ x: scale.x(hd), label: hd === scale.to ? null : label, major: monday });
  }
  return out;
}

const halfDays = (count: number) => i18n('plan.halfDays', { count });
const dates = (start: HalfDay, end: HalfDay) => `${shortDate(dateOf(start))} – ${shortDate(dateOf(end - 1))}`;

function taskTitle(t: TaskPlan, summary: string | undefined): string {
  const parts = [`${t.code}${summary ? ': ' + summary : ''}`, STATUS_NAMES[t.status] ?? t.status];
  if (t.start != null && t.end != null) parts.push(`${dates(t.start, t.end)}, ${halfDays(t.end - t.start)}`);
  if (t.assignee) parts.push('@' + t.assignee);
  if (t.a != null) parts.push(`a ${t.a}, s ${t.s ?? '—'}`);
  if (t.on_chain) parts.push(i18n('plan.chain'));
  const flags = visibleFlags(t.flags);
  if (flags.length) parts.push(flags.map(flagName).join(', '));
  return parts.join(' · ');
}

function bufferTitle(b: BufferPlan, label: string): string {
  const parts = [label, halfDays(b.size), dates(b.start, b.end)];
  if (b.zone) parts.push(i18n('plan.zoneOf', { zone: ZONE_NAMES[b.zone] }));
  if (b.consumed_pct != null) parts.push(i18n('plan.gantt.consumed', { pct: b.consumed_pct }));
  if (b.chain_complete_pct != null) parts.push(i18n('plan.gantt.chainComplete', { pct: b.chain_complete_pct }));
  if (b.compressed) parts.push(flagName('compressed'));
  return parts.join(' · ');
}

const byInterval = (a: TaskPlan, b: TaskPlan) => a.start! - b.start! || a.end! - b.end! || a.seq - b.seq;

type Slot = { kind: 'task'; task: TaskPlan } | { kind: 'buffer'; buffer: BufferPlan } | { kind: 'group'; label: string };

/** Row order. By task: umbrellas, then tasks by start, a buffer right after its last task.
 * By person: groups by assignee (unassigned last), no umbrellas, buffers as a separate group. */
export function rowOrder(plan: Plan, group: 'task' | 'person' = 'task'): Slot[] {
  const placed = plan.tasks.filter((t) => t.start != null && t.end != null);
  const umbrellas = placed.filter((t) => t.flags.includes('umbrella')).sort(byInterval);
  const work = placed.filter((t) => !t.flags.includes('umbrella')).sort(byInterval);
  const buffers = [...plan.buffers].sort((a, b) => a.start - b.start || a.key.localeCompare(b.key));
  if (group === 'person') {
    const people = new Map<string | null, TaskPlan[]>();
    for (const t of work) people.set(t.assignee, [...(people.get(t.assignee) ?? []), t]);
    const keys = [...people.keys()].sort((a, b) => (a === null ? 1 : b === null ? -1 : a.localeCompare(b)));
    const out: Slot[] = [];
    for (const k of keys) {
      out.push({ kind: 'group', label: k === null ? i18n('plan.gantt.unassigned') : '@' + k });
      for (const task of people.get(k)!) out.push({ kind: 'task', task });
    }
    if (buffers.length) out.push({ kind: 'group', label: i18n('plan.gantt.buffers') }, ...buffers.map((buffer) => ({ kind: 'buffer' as const, buffer })));
    return out;
  }
  const out: Slot[] = [...umbrellas, ...work].map((task) => ({ kind: 'task' as const, task }));
  const tail: Slot[] = [];
  for (const buffer of buffers) {
    // Anchor: the buffer's task with the latest end among the rows (for PB, the last chain link).
    let anchor = -1;
    let best = -Infinity;
    out.forEach((s, i) => {
      if (s.kind === 'task' && buffer.tasks.includes(s.task.seq) && s.task.end! >= best) {
        best = s.task.end!;
        anchor = i;
      }
    });
    if (anchor < 0) {
      tail.push({ kind: 'buffer', buffer });
      continue;
    }
    // After the anchor and the buffers already inserted after it.
    let at = anchor + 1;
    while (at < out.length && out[at].kind === 'buffer') at++;
    out.splice(at, 0, { kind: 'buffer', buffer });
  }
  return [...out, ...tail];
}

const mid = (y: number) => y + ROW_H / 2;

/** Polyline from the end of one bar to the start of another. */
export function connector(x1: number, y1: number, x2: number, y2: number): string {
  if (y1 === y2) return `M${x1},${y1} H${x2 - 2}`;
  if (x2 - x1 >= 12) return `M${x1},${y1} H${x1 + 6} V${y2} H${x2 - 2}`;
  // One bar ends almost where the other starts (usually flush): down from the bar edge, then right to the start.
  if (x2 >= x1) return `M${x1 - 3},${y1 + (y2 > y1 ? BAR_H / 2 : -BAR_H / 2)} V${y2} H${x2 - 2}`;
  // Backwards on the scale (resource or overlapping dependency): detour through the middle between rows.
  const yMid = y2 > y1 ? y2 - ROW_H / 2 : y2 + ROW_H / 2;
  return `M${x1},${y1} H${x1 + 6} V${yMid} H${x2 - 8} V${y2} H${x2 - 2}`;
}

export function ganttLayout(input: GanttInput): GanttLayout {
  const { plan, due = null, deps = [], summaries = {}, group = 'task' } = input;
  const scale = makeScale(plan, due);
  const codeOf = (seq: number) => plan.tasks.find((t) => t.seq === seq)?.code;
  const own = new Set(plan.tasks.map((t) => t.code));
  const rows: Row[] = rowOrder(plan, group).map((s, i) => {
    const y = HEAD_H + i * ROW_H;
    if (s.kind === 'group') return { kind: 'group', y, label: s.label };
    if (s.kind === 'buffer') {
      const b = s.buffer;
      const label = bufferName(b.key, codeOf);
      return { kind: 'buffer', y, buffer: b, x: scale.x(b.start), w: Math.max(2, (b.end - b.start) * HD_PX), label, title: bufferTitle(b, label) };
    }
    const t = s.task;
    const external = deps.filter((d) => d.dst === t.code && !own.has(d.src)).map((d) => d.src);
    const waits = t.flags.includes('external_blocker') || t.flags.includes('external_milestone');
    const milestone = waits
      ? {
          x: scale.x(t.start!),
          title: i18n(t.flags.includes('external_milestone') ? 'plan.gantt.milestoneForecast' : 'plan.gantt.milestoneNoDate', {
            code: t.code,
            blockers: external.length ? external.join(', ') : i18n('plan.gantt.externalBlocker')
          })
        }
      : null;
    return {
      kind: 'task',
      y,
      task: t,
      x: scale.x(t.start!),
      w: Math.max(2, (t.end! - t.start!) * HD_PX),
      chain: t.on_chain,
      umbrella: t.flags.includes('umbrella'),
      defaulted: t.flags.includes('estimate_defaulted'),
      flags: visibleFlags(t.flags),
      title: taskTitle(t, summaries[t.code]),
      milestone
    };
  });
  const taskRows = new Map<string, TaskRow>();
  for (const r of rows) if (r.kind === 'task') taskRows.set(r.task.code, r);
  const arrows: Arrow[] = [];
  for (const d of deps) {
    const a = taskRows.get(d.src);
    const b = taskRows.get(d.dst);
    if (!a || !b || a.umbrella || b.umbrella) continue;
    arrows.push({ d: connector(a.x + a.w, mid(a.y), b.x, mid(b.y)), title: i18n('plan.gantt.blocks', { src: d.src, dst: d.dst }), resource: false });
  }
  for (let i = 1; i < plan.chain.length; i++) {
    if (!plan.chain[i].resource_link) continue;
    const a = taskRows.get(codeOf(plan.chain[i - 1].seq) ?? '');
    const b = taskRows.get(codeOf(plan.chain[i].seq) ?? '');
    if (!a || !b) continue;
    arrows.push({
      d: connector(a.x + a.w, mid(a.y), b.x, mid(b.y)),
      title: i18n('plan.gantt.resourceLink', { a: a.task.code, b: b.task.code }) + (b.task.assignee ? ' @' + b.task.assignee : ''),
      resource: true
    });
  }
  return {
    width: scale.width,
    height: HEAD_H + rows.length * ROW_H + PAD_B,
    scale,
    rows,
    arrows,
    ticks: ticks(scale),
    today: scale.x(plan.today),
    due: due == null ? null : scale.x(due),
    end: scale.x(plan.end),
    unscheduled: plan.tasks.filter((t) => t.start == null || t.end == null)
  };
}

/** Dependencies from the /api/graph response: live confirmed blocks. */
export function depsFromGraph(edges: { src: string; dst: string; rel: string; state: string }[]): Dep[] {
  return edges.filter((e) => e.rel === 'blocks' && e.state === 'confirmed').map((e) => ({ src: e.src, dst: e.dst }));
}
