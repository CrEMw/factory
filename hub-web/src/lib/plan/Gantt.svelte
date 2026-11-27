<script lang="ts">
  // Scope Gantt as plain SVG without chart libraries: task bars by half-day, umbrella summary bars,
  // external blocker milestones, buffers in zone colour, dependency arrows, dashed resource links, "today" and due
  // date lines, flags, the critical chain highlighted. Geometry in gantt.ts. A wide plan scrolls inside its own
  // block, the page does not scroll horizontally.
  import { t } from '#lib/i18n/index';
  import { BAR_H, ganttLayout, HEAD_H, LABEL_W, ROW_H, type Dep } from '#lib/plan/gantt';
  import { flagName } from '#lib/plan/flags';
  import type { HalfDay, Plan } from '#lib/plan/types';

  let {
    plan,
    due = null,
    deps = [],
    summaries = {},
    group = 'task',
    label = t('plan.gantt.label')
  }: {
    plan: Plan;
    due?: HalfDay | null;
    deps?: Dep[];
    summaries?: Record<string, string>;
    group?: 'task' | 'person';
    label?: string;
  } = $props();

  const g = $derived(ganttLayout({ plan, due, deps, summaries, group }));
  const top = (y: number) => y + (ROW_H - BAR_H) / 2;
</script>

<div class="frame card" role="region" aria-label={label} tabindex="-1">
  <svg width={g.width} height={g.height} viewBox="0 0 {g.width} {g.height}" role="img" aria-label={t('plan.gantt.aria', { label })}>
    <title>{label}</title>
    <defs>
      <pattern id="gantt-hatch" width="6" height="6" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
        <rect width="6" height="6" class="hatch-bg" />
        <line x1="0" y1="0" x2="0" y2="6" class="hatch-line" />
      </pattern>
      <marker id="gantt-arrow" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6" markerHeight="6" orient="auto">
        <path d="M0,0 L8,4 L0,8 z" class="arrowhead" />
      </marker>
    </defs>

    <g class="axis" aria-label={t('plan.gantt.axis')}>
      {#each g.ticks as t, i (i)}
        <line x1={t.x} y1={HEAD_H - 6} x2={t.x} y2={g.height} class="grid" class:major={t.major} />
        {#if t.label}<text x={t.x + 3} y={HEAD_H - 9} class="tick">{t.label}</text>{/if}
      {/each}
    </g>

    {#each g.rows as r, i (i)}
      {#if r.kind === 'group'}
        <text x="6" y={r.y + ROW_H / 2 + 4} class="group">{r.label}</text>
        <line x1="0" y1={r.y + ROW_H - 1} x2={g.width} y2={r.y + ROW_H - 1} class="sep" />
      {:else if r.kind === 'buffer'}
        <g class="buffer zone-{r.buffer.zone ?? 'none'}" class:project={r.buffer.kind === 'project'}>
          <title>{r.title}</title>
          <text x="6" y={r.y + ROW_H / 2 + 4} class="label">{r.buffer.key === 'pb' ? 'PB' : 'FB'}</text>
          <rect x={r.x} y={top(r.y)} width={r.w} height={BAR_H} rx="3" class:compressed={r.buffer.compressed} />
        </g>
      {:else}
        <a href="/t/{encodeURIComponent(r.task.code)}" class="task st-{r.task.status}" class:chain={r.chain} class:umbrella={r.umbrella}>
          <title>{r.title}</title>
          <text x="6" y={r.y + ROW_H / 2 + 4} class="label">{r.task.code}</text>
          {#if r.umbrella}
            <path
              d="M{r.x},{top(r.y) + BAR_H} V{top(r.y) + 4} H{r.x + r.w} V{top(r.y) + BAR_H} L{r.x + r.w - 5},{top(r.y) + 9} H{r.x + 5} Z"
              class="summary"
            />
          {:else}
            <rect x={r.x} y={top(r.y)} width={r.w} height={BAR_H} rx="3" class="bar" />
            {#if r.defaulted}<rect x={r.x} y={top(r.y)} width={r.w} height={BAR_H} rx="3" fill="url(#gantt-hatch)" class="defaulted" />{/if}
          {/if}
          {#if r.flags.length && !r.umbrella}
            <text x={r.x + r.w + 4} y={r.y + ROW_H / 2 + 4} class="flag">⚑{r.flags.length}</text>
          {/if}
        </a>
        {#if r.milestone}
          <g class="milestone">
            <title>{r.milestone.title}</title>
            <path d="M{r.milestone.x},{r.y + 3} l5,{ROW_H / 2 - 3} l-5,{ROW_H / 2 - 3} l-5,{-(ROW_H / 2 - 3)} z" />
          </g>
        {/if}
      {/if}
    {/each}

    {#each g.arrows as a, i (i)}
      <path d={a.d} class="arrow" class:resource={a.resource} marker-end="url(#gantt-arrow)"><title>{a.title}</title></path>
    {/each}

    {#if g.due != null}
      <line x1={g.due} y1={HEAD_H - 6} x2={g.due} y2={g.height} class="due"><title>{t('plan.gantt.due')}</title></line>
    {/if}
    <line x1={g.today} y1={HEAD_H - 6} x2={g.today} y2={g.height} class="today"><title>{t('plan.gantt.todayTitle')}</title></line>
    <text x={g.today + 3} y="12" class="today-label">{t('plan.gantt.today')}</text>
    <line x1={LABEL_W - 4} y1={HEAD_H - 6} x2={LABEL_W - 4} y2={g.height} class="sep" />
  </svg>
</div>
{#if g.rows.some((r) => r.kind === 'task' && r.flags.length)}
  <details class="flags">
    <summary>{t('plan.gantt.flags')}</summary>
    <ul>
      {#each g.rows as r, i (i)}
        {#if r.kind === 'task' && r.flags.length}
          <li><a href="/t/{encodeURIComponent(r.task.code)}"><code>{r.task.code}</code></a>: {r.flags.map(flagName).join(', ')}{#if r.task.cycle?.length}{' '}({r.task.cycle.join(' → ')}){/if}</li>
        {/if}
      {/each}
    </ul>
  </details>
{/if}

<style>
  .frame {
    max-width: 100%;
    overflow-x: auto;
    -webkit-overflow-scrolling: touch;
  }
  svg {
    display: block;
    font: 12px system-ui, sans-serif;
  }
  .grid {
    stroke: var(--line);
    stroke-width: 1;
  }
  .grid.major {
    stroke: var(--muted);
    stroke-opacity: 0.4;
  }
  .sep {
    stroke: var(--line);
  }
  .tick,
  .today-label {
    fill: var(--muted);
    font-size: 11px;
  }
  .group {
    fill: var(--text);
    font-weight: 600;
  }
  .label {
    fill: var(--text);
    font: 12px ui-monospace, SFMono-Regular, Menlo, monospace;
  }
  .task .bar {
    fill: var(--accent);
    fill-opacity: 0.75;
  }
  .task.st-doing .bar {
    fill: var(--doing);
  }
  .task.st-done .bar {
    fill: var(--done);
  }
  .task.chain .bar {
    fill-opacity: 1;
    stroke: var(--text);
    stroke-width: 2;
  }
  .task.chain .label {
    font-weight: 700;
  }
  .hatch-bg {
    fill: transparent;
  }
  .hatch-line {
    stroke: var(--card);
    stroke-width: 2;
    stroke-opacity: 0.8;
  }
  .summary {
    fill: var(--text);
    fill-opacity: 0.7;
  }
  .flag {
    fill: var(--danger);
    font-size: 11px;
  }
  .milestone path {
    fill: var(--card);
    stroke: var(--danger);
    stroke-width: 2;
  }
  .buffer rect {
    fill: var(--muted);
    fill-opacity: 0.25;
    stroke: var(--muted);
    stroke-dasharray: 3 2;
  }
  .buffer.zone-green rect {
    fill: var(--zone-green);
    fill-opacity: 0.6;
    stroke: var(--zone-green);
    stroke-dasharray: none;
  }
  .buffer.zone-yellow rect {
    fill: var(--zone-yellow);
    fill-opacity: 0.6;
    stroke: var(--zone-yellow);
    stroke-dasharray: none;
  }
  .buffer.zone-red rect {
    fill: var(--zone-red);
    fill-opacity: 0.6;
    stroke: var(--zone-red);
    stroke-dasharray: none;
  }
  .buffer.project rect {
    stroke-width: 2;
  }
  .buffer rect.compressed {
    stroke: var(--danger);
  }
  .buffer .label {
    fill: var(--muted);
  }
  .arrow {
    fill: none;
    stroke: var(--muted);
    stroke-width: 1.3;
  }
  .arrow.resource {
    stroke: var(--text);
    stroke-dasharray: 4 3;
  }
  .arrowhead {
    fill: var(--muted);
  }
  .today {
    stroke: var(--danger);
    stroke-width: 2;
  }
  .due {
    stroke: var(--text);
    stroke-width: 2;
    stroke-dasharray: 6 3;
  }
  .flags {
    margin: 8px 0 0;
    font-size: 13px;
  }
  .flags ul {
    margin: 6px 0 0;
    padding-left: 20px;
  }
</style>
