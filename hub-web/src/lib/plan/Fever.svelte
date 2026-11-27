<script lang="ts">
  // Fever chart as plain SVG: zones by the fever.rs rule, points by day and buffer. Geometry in
  // fever.ts; the SVG scales to the block width (viewBox), so on a phone it is no wider than the screen.
  import { t } from '#lib/i18n/index';
  import { feverLayout, type ZoneCfg } from '#lib/plan/fever';
  import type { FeverPoint } from '#lib/plan/types';

  let { points, cfg, codeOf = () => undefined }: { points: FeverPoint[]; cfg: ZoneCfg; codeOf?: (seq: number) => string | undefined } = $props();

  const f = $derived(feverLayout(points, cfg, codeOf));
</script>

<figure class="fever card">
  <svg viewBox="0 0 {f.width} {f.height}" role="img" aria-label={t('plan.fever.aria')}>
    <title>{t('plan.fever.title')}</title>
    {#each f.bands as b (b.zone)}
      <path d={b.d} class="band {b.zone}"><title>{b.title}</title></path>
    {/each}
    <g class="axis">
      {#each f.xTicks as t (t.label)}
        <line x1={t.x} y1={f.plot.y0} x2={t.x} y2={f.plot.y0 + 4} />
        <text x={t.x} y={f.plot.y0 + 16} text-anchor="middle">{t.label}</text>
      {/each}
      {#each f.yTicks as t (t.label)}
        <line x1={f.plot.x0 - 4} y1={t.y} x2={f.plot.x0} y2={t.y} />
        <text x={f.plot.x0 - 6} y={t.y + 4} text-anchor="end">{t.label}</text>
      {/each}
      <line x1={f.plot.x0} y1={f.plot.y0} x2={f.plot.x1} y2={f.plot.y0} />
      <line x1={f.plot.x0} y1={f.plot.y0} x2={f.plot.x0} y2={f.plot.y1} />
      <text x={(f.plot.x0 + f.plot.x1) / 2} y={f.height - 6} text-anchor="middle" class="name">{t('plan.fever.x')}</text>
      <text x="12" y={(f.plot.y0 + f.plot.y1) / 2} text-anchor="middle" transform="rotate(-90 12 {(f.plot.y0 + f.plot.y1) / 2})" class="name">{t('plan.fever.y')}</text>
    </g>
    {#each f.series as s (s.key)}
      <g class="series" class:project={s.project}>
        <title>{s.label}</title>
        {#if s.d}<path d={s.d} class="line" />{/if}
        {#each s.points as p, i (i)}
          <circle cx={p.cx} cy={p.cy} r={s.project ? 5 : 4} class="dot {p.zone}"><title>{p.title}</title></circle>
        {/each}
      </g>
    {/each}
  </svg>
  {#if f.series.length}
    <figcaption class="muted small">
      {#each f.series as s (s.key)}<span class="key" class:project={s.project}>{s.label}</span>{/each}
    </figcaption>
  {/if}
</figure>

<style>
  .fever {
    margin: 0;
    padding: 8px;
  }
  svg {
    display: block;
    width: 100%;
    height: auto;
    max-width: 640px;
    font: 12px system-ui, sans-serif;
  }
  .band {
    fill-opacity: 0.22;
    stroke: none;
  }
  .band.green {
    fill: var(--zone-green);
  }
  .band.yellow {
    fill: var(--zone-yellow);
  }
  .band.red {
    fill: var(--zone-red);
  }
  .axis line {
    stroke: var(--muted);
  }
  .axis text {
    fill: var(--muted);
  }
  .axis .name {
    fill: var(--text);
    font-size: 12px;
  }
  .line {
    fill: none;
    stroke: var(--muted);
    stroke-width: 1.5;
  }
  .series.project .line {
    stroke: var(--text);
    stroke-width: 2;
  }
  .dot {
    stroke: var(--card);
    stroke-width: 1.5;
  }
  .dot.green {
    fill: var(--zone-green);
  }
  .dot.yellow {
    fill: var(--zone-yellow);
  }
  .dot.red {
    fill: var(--zone-red);
  }
  figcaption {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    font-size: 12px;
  }
  .key.project {
    color: var(--text);
    font-weight: 600;
  }
</style>
