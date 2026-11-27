<script lang="ts">
  // Graph of blocks and part_of links in plain SVG, no chart libraries. A column is a layer (hub
  // computes the longest path), a row is the order within the layer. A wide graph scrolls inside its frame, the page
  // does not scroll horizontally.
  import { t } from '#lib/i18n/index';

  let { data } = $props();

  const COL = 200;
  const ROW = 66;
  const W = 168;
  const H = 48;
  const PAD = 12;
  const STATUS_NAMES: Record<string, string> = {
    open: t('tickets.graph.status.open'),
    doing: t('tickets.graph.status.doing'),
    done: t('tickets.graph.status.done'),
    cancelled: t('tickets.graph.status.cancelled')
  };

  const graph = $derived(data.graph);
  const pos = $derived(
    new Map((graph?.nodes ?? []).map((n) => [n.code, { x: PAD + n.layer * COL, y: PAD + n.order * ROW }]))
  );
  const rows = $derived(Math.max(1, ...(graph?.nodes ?? []).map((n) => n.order + 1)));
  const width = $derived(PAD * 2 + Math.max(0, (graph?.layers ?? 1) - 1) * COL + W);
  const height = $derived(PAD * 2 + (rows - 1) * ROW + H);

  function path(src: string, dst: string): string {
    const a = pos.get(src);
    const b = pos.get(dst);
    if (!a || !b) return '';
    const [x1, y1, x2, y2] = [a.x + W, a.y + H / 2, b.x, b.y + H / 2];
    const dx = Math.max(24, (x2 - x1) / 2);
    return `M${x1},${y1} C${x1 + dx},${y1} ${x2 - dx},${y2} ${x2 - 4},${y2}`;
  }

  const short = (s: string, n = 24) => (s.length > n ? s.slice(0, n - 1) + '…' : s);
</script>

<svelte:head><title>{t('tickets.graph.title')} · {data.hubName}</title></svelte:head>

<main>
  <h1>{t('tickets.graph.title')}</h1>
  <p class="muted">{t('tickets.graph.intro')}</p>

  <form class="card filters" method="GET">
    <label>
      {t('tickets.field.project')}
      <!-- Changing the project clears the ticket: with root hub looks only at the ticket's neighbourhood, the project is ignored. -->
      <select
        name="project"
        onchange={(e) => {
          const root = e.currentTarget.form?.elements.namedItem('root');
          if (root instanceof HTMLInputElement) root.value = '';
        }}
      >
        <option value="">{t('tickets.graph.allProjects')}</option>
        {#each data.projects as p (p.project)}
          <option value={p.project} selected={data.filters.project === p.project}>{p.project}</option>
        {/each}
      </select>
    </label>
    <label>{t('tickets.field.ticket')} <input name="root" value={data.filters.root} placeholder={t('tickets.graph.rootPlaceholder')} size="8" autocomplete="off" /></label>
    <label>
      {t('tickets.graph.depth')}
      <select name="depth">
        {#each ['', '1', '2', '3', '5', '10'] as d (d)}
          <option value={d} selected={data.filters.depth === d}>{d || t('tickets.graph.defaultDepth')}</option>
        {/each}
      </select>
    </label>
    <button>{t('tickets.common.show')}</button>
  </form>
  {#if data.filters.root && !data.bad}
    <p class="muted small mode">
      {t('tickets.graph.neighbourhood')} <b>{data.filters.root}</b>{#if data.filters.project}; {t('tickets.graph.projectIgnored', { project: data.filters.project })}{/if}.
      {#if data.filters.project}<a href="?project={encodeURIComponent(data.filters.project)}">{t('tickets.graph.wholeProject', { project: data.filters.project })}</a>{/if}
    </p>
  {/if}

  {#if data.bad}
    <p role="alert">{t('tickets.graph.bad')}</p>
  {:else if data.missing}
    <p class="muted">{t('tickets.graph.missing')}</p>
  {:else if data.failed}
    <p role="alert">{t('tickets.graph.failed')}</p>
  {:else if !graph}
    <p class="muted">{t('tickets.graph.pick')}</p>
  {:else if graph.nodes.length === 0}
    <p class="muted">{t('tickets.graph.noLinks', { project: graph.project ?? '' })}</p>
  {:else}
    <p class="legend muted small">
      {#each Object.entries(STATUS_NAMES) as [s, name] (s)}<span class="chip st-{s}">{name}</span>{/each}
      <span>{t('tickets.graph.lines')}</span>
      {t('tickets.graph.nodes', { count: graph.nodes.length })}, {t('tickets.graph.edges', { count: graph.edges.length })}{#if graph.truncated}, {t('tickets.graph.truncated', { count: graph.nodes.length })}{/if}
    </p>
    <div class="frame card" role="region" aria-label={t('tickets.graph.region')} tabindex="-1">
      <svg {width} {height} viewBox="0 0 {width} {height}" role="img" aria-label={t('tickets.graph.image')}>
        <defs>
          <marker id="arrow" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto">
            <path d="M0,0 L8,4 L0,8 z" class="arrowhead" />
          </marker>
        </defs>
        {#each graph.edges as e (e.id)}
          <path
            d={path(e.src, e.dst)}
            class="edge"
            class:dashed={e.rel === 'part_of' || e.state === 'proposed'}
            marker-end="url(#arrow)"
          ><title>{e.src} {e.rel === 'blocks' ? t('tickets.links.kind.blocks') : t('tickets.links.kind.partOf')} {e.dst}{e.state === 'proposed' ? ` (${t('tickets.links.proposed')})` : ''}</title></path>
        {/each}
        {#each graph.nodes as n (n.code)}
          {@const p = pos.get(n.code)!}
          <a href="/t/{encodeURIComponent(n.code)}" class="node st-{n.status}" class:root={n.code === graph.root}>
            <title>{n.code}: {n.summary} ({STATUS_NAMES[n.status] ?? n.status}, {n.project})</title>
            <rect x={p.x} y={p.y} width={W} height={H} rx="8" />
            <rect x={p.x} y={p.y} width="5" height={H} rx="2" class="bar" />
            <text x={p.x + 12} y={p.y + 19} class="code">{n.code}{n.priority ? ' · ' + n.priority : ''}{n.project !== graph.project && graph.project ? ' · ' + n.project : ''}</text>
            <text x={p.x + 12} y={p.y + 37} class="sum">{short(n.summary)}</text>
          </a>
        {/each}
      </svg>
    </div>
  {/if}
</main>

<style>
  h1 {
    margin: 12px 0 4px;
  }
  .filters {
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    gap: 8px 16px;
    margin: 16px 0;
    padding: 12px 16px;
  }
  .filters label {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 13px;
    color: var(--muted);
  }
  .filters input,
  .filters select {
    max-width: 100%;
  }
  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 10px;
    align-items: center;
  }
  .chip {
    padding: 0 6px;
    border-left: 4px solid var(--line);
  }
  .chip.st-open {
    border-color: var(--accent);
  }
  .chip.st-doing {
    border-color: var(--doing);
  }
  .chip.st-done {
    border-color: var(--done);
  }
  .chip.st-cancelled {
    border-color: var(--muted);
  }
  /* The frame scrolls by itself, so on a phone the page is no wider than the screen. */
  .frame {
    max-width: 100%;
    overflow: auto;
    max-height: 75vh;
    -webkit-overflow-scrolling: touch;
  }
  svg {
    display: block;
  }
  .edge {
    fill: none;
    stroke: var(--muted);
    stroke-width: 1.4;
  }
  .edge.dashed {
    stroke-dasharray: 5 4;
  }
  .arrowhead {
    fill: var(--muted);
  }
  .node rect {
    fill: var(--card);
    stroke: var(--line);
  }
  .node.root rect:first-of-type {
    stroke: var(--text);
    stroke-width: 2;
  }
  .node .bar {
    stroke: none;
  }
  .node.st-open .bar {
    fill: var(--accent);
  }
  .node.st-doing .bar {
    fill: var(--doing);
  }
  .node.st-done .bar {
    fill: var(--done);
  }
  .node.st-cancelled .bar {
    fill: var(--muted);
  }
  .node.st-done text,
  .node.st-cancelled text {
    opacity: 0.6;
  }
  .node:hover rect:first-of-type,
  .node:focus rect:first-of-type {
    stroke: var(--accent);
  }
  .code {
    font: 600 12px ui-monospace, SFMono-Regular, Menlo, monospace;
    fill: var(--text);
  }
  .sum {
    font-size: 12px;
    fill: var(--muted);
  }
</style>
