<script lang="ts">
  import { enhance } from '$app/forms';
  import { page } from '$app/state';
  import { dateFormat, t } from '#lib/i18n/index';

  let { data, form } = $props();

  type Item = {
    code: string;
    project: string;
    summary: string;
    status: string;
    priority: string | null;
    tags: string[];
    updated_at: string;
    assignee?: string | null;
    components?: string[];
  };

  const when = dateFormat({ dateStyle: 'medium' });
  // Board columns; cancelled is not on the board, it shows in the list (the "cancelled" filter).
  const BOARD = ['open', 'doing', 'done'] as const;
  const STATUSES = [...BOARD, 'cancelled'] as const;
  const PRIOS = ['p0', 'p1', 'p2', 'p3'] as const;
  const VIEW_NAMES = { board: t('tickets.index.view.board'), prio: t('tickets.index.view.prio'), list: t('tickets.index.view.list') };
  const STATUS_NAMES: Record<string, string> = {
    open: t('tickets.index.status.open'),
    doing: t('tickets.index.status.doing'),
    done: t('tickets.index.status.done'),
    cancelled: t('tickets.index.status.cancelled')
  };

  /** The same URL with params replaced; null removes a param. Changing a filter resets offset. */
  function href(change: Record<string, string | number | null>): string {
    const q = new URLSearchParams(page.url.search);
    if (!('offset' in change)) q.delete('offset');
    for (const [k, v] of Object.entries(change)) {
      if (v === null || v === '') q.delete(k);
      else q.set(k, String(v));
    }
    return q.size ? `?${q}` : '?';
  }

  const total = $derived(data.projects.reduce((n, p) => n + p.tickets, 0));
  // Counters from hub stats: by status, and by priority among active tickets (open, doing).
  const byStatus = $derived(
    Object.fromEntries(STATUSES.map((s) => [s, data.stats.cells.filter((c) => c.status === s).reduce((n, c) => n + c.tickets, 0)]))
  );
  const byPrio = $derived(
    Object.fromEntries(
      [...PRIOS, null].map((p) => [
        p ?? 'none',
        data.stats.cells.filter((c) => (c.status === 'open' || c.status === 'doing') && c.priority === p).reduce((n, c) => n + c.tickets, 0)
      ])
    )
  );
  const prioGroups = $derived(
    data.list && data.view === 'prio'
      ? [...PRIOS, null].map((p) => ({ prio: p, items: data.list!.items.filter((item) => item.priority === p) }))
      : []
  );

  // Board drag and drop: a card is dropped on a column, a hidden form submits ?/status.
  let moveForm: HTMLFormElement | undefined = $state();
  let moveCode = $state('');
  let moveStatus = $state('');
  let dragging = $state<string | null>(null);
  let over = $state<string | null>(null);
  function drop(status: string) {
    over = null;
    if (!dragging || !moveForm) return;
    moveCode = dragging;
    moveStatus = status;
    dragging = null;
    queueMicrotask(() => moveForm?.requestSubmit());
  }
</script>

<svelte:head><title>{t('tickets.index.title')} · {data.hubName}</title></svelte:head>

{#snippet card(item: Item)}
  <a class="code" href="/t/{encodeURIComponent(item.code)}"><code>{item.code}</code></a>
  {#if item.priority}<span class="prio {item.priority}">{item.priority}</span>{/if}
  <a class="summary" href="/t/{encodeURIComponent(item.code)}">{item.summary}</a>
  <div class="meta">
    {#if !data.filters.project}<a class="plain" href={href({ project: item.project })}>{item.project}</a>{/if}
    {#if item.assignee}<a class="plain" href={href({ assignee: item.assignee })}>@{item.assignee}</a>{/if}
    {#each item.components ?? [] as c (c)}<a class="plain comp" href={href({ component: c })}>{c}</a>{/each}
    {#each item.tags as tag (tag)}<a class="plain tag" href={href({ tag })}>#{tag}</a>{/each}
  </div>
{/snippet}

{#snippet statusSelect(item: Item)}
  <form method="POST" action="?/status" use:enhance class="inline">
    <input type="hidden" name="code" value={item.code} />
    <select name="status" aria-label={t('tickets.index.statusOf', { code: item.code })} onchange={(e) => e.currentTarget.form?.requestSubmit()}>
      {#each STATUSES as s (s)}<option value={s} selected={item.status === s}>{s}</option>{/each}
    </select>
    <noscript><button>ok</button></noscript>
  </form>
{/snippet}

<main>
  <div class="head">
    <h1>{t('tickets.index.title')}</h1>
    <nav class="views" aria-label={t('tickets.index.views')}>
      {#each Object.entries(VIEW_NAMES) as [v, name] (v)}
        <a href={href({ view: v, sort: null })} class:active={data.view === v} aria-current={data.view === v ? 'page' : undefined}>{name}</a>
      {/each}
    </nav>
  </div>

  <nav class="chips" aria-label={t('tickets.index.projects')}>
    <a href={href({ project: null })} class:active={!data.filters.project}>{t('tickets.index.allProjects')} <span>{total}</span></a>
    {#each data.projects as p (p.project)}
      <a href={href({ project: p.project })} class:active={data.filters.project === p.project}>{p.project} <span>{p.tickets}</span></a>
    {/each}
  </nav>

  <div class="bar">
    <form method="GET" class="search" role="search">
      {#each ['view', 'project', 'status', 'priority', 'tag', 'assignee', 'component', 'sort'] as k (k)}
        {#if page.url.searchParams.get(k)}<input type="hidden" name={k} value={page.url.searchParams.get(k)} />{/if}
      {/each}
      <input type="search" name="q" value={data.filters.q} placeholder={t('tickets.index.searchPlaceholder')} aria-label={t('tickets.index.search')} />
    </form>
    {#if data.view === 'list'}
      <nav class="chips" aria-label={t('tickets.field.status')}>
        {#each [['open,doing', t('tickets.index.filter.active')], ['open', 'open'], ['doing', 'doing'], ['done', 'done'], ['cancelled', t('tickets.index.filter.cancelled')], ['all', t('tickets.common.all')]] as [s, name] (s)}
          <a href={href({ status: s === 'open,doing' ? null : s })} class:active={data.filters.status === s}
            >{name}{#if s in byStatus}&nbsp;<span>{byStatus[s]}</span>{/if}</a
          >
        {/each}
      </nav>
    {/if}
    <nav class="chips" aria-label={t('tickets.field.priority')}>
      <a href={href({ priority: null })} class:active={!data.filters.priority}>{t('tickets.index.any')}</a>
      {#each [...PRIOS, 'none'] as p (p)}
        <a href={href({ priority: p })} class:active={data.filters.priority === p}
          >{p === 'none' ? t('tickets.index.noPrio') : p}&nbsp;<span>{byPrio[p]}</span></a
        >
      {/each}
    </nav>
  </div>

  {#if data.people.length || data.components.length || data.filters.assignee || data.filters.component}
    <div class="bar">
      <nav class="chips" aria-label={t('tickets.field.assignee')}>
        <a href={href({ assignee: null })} class:active={!data.filters.assignee}>{t('tickets.index.any')}</a>
        {#each data.people as p (p.handle)}
          <a href={href({ assignee: p.handle })} class:active={data.filters.assignee === p.handle} title={p.name}
            >@{p.handle}&nbsp;<span>{p.active}</span></a
          >
        {/each}
        <a href={href({ assignee: 'none' })} class:active={data.filters.assignee === 'none'}>{t('tickets.index.noAssignee')}</a>
      </nav>
      <form method="GET" class="component">
        {#each ['view', 'project', 'status', 'priority', 'tag', 'assignee', 'q', 'sort'] as k (k)}
          {#if page.url.searchParams.get(k)}<input type="hidden" name={k} value={page.url.searchParams.get(k)} />{/if}
        {/each}
        <select name="component" aria-label={t('tickets.field.component')} value={data.filters.component} onchange={(e) => e.currentTarget.form?.requestSubmit()}>
          <option value="">{t('tickets.index.anyComponent')}</option>
          {#each data.components as c (c.slug)}<option value={c.slug}>{c.title} ({c.active})</option>{/each}
          {#if data.filters.component && !data.components.some((c) => c.slug === data.filters.component)}
            <option value={data.filters.component}>{data.filters.component}</option>
          {/if}
        </select>
        <noscript><button>ok</button></noscript>
      </form>
    </div>
  {/if}

  {#if data.filters.tag || data.stats.active_tags.length}
    <nav class="chips tags" aria-label={t('tickets.index.tags')}>
      {#if data.filters.tag}<a class="active" href={href({ tag: null })}>#{data.filters.tag} ×</a>{/if}
      {#each data.stats.active_tags.filter((item) => item.tag !== data.filters.tag) as item (item.tag)}
        <a href={href({ tag: item.tag })}>#{item.tag} <span>{item.tickets}</span></a>
      {/each}
    </nav>
  {/if}

  {#if form?.message}<p role="alert">{form.message}</p>{/if}

  {#if data.board}
    <form method="POST" action="?/status" use:enhance bind:this={moveForm} hidden>
      <input name="code" value={moveCode} /><input name="status" value={moveStatus} />
    </form>
    <div class="board">
      {#each BOARD as s (s)}
        {@const col = data.board[s]}
        <section
          class="column card"
          class:over={over === s}
          aria-label={STATUS_NAMES[s]}
          ondragover={(e) => {
            if (dragging) {
              e.preventDefault();
              over = s;
            }
          }}
          ondragleave={() => (over = over === s ? null : over)}
          ondrop={(e) => {
            e.preventDefault();
            drop(s);
          }}
        >
          <h2><span class="pill {s}">{STATUS_NAMES[s]}</span> <span class="muted">{col.total}</span></h2>
          {#each col.items as item (item.code)}
            <article
              class="ticket"
              class:dragging={dragging === item.code}
              draggable="true"
              ondragstart={(e) => {
                dragging = item.code;
                e.dataTransfer?.setData('text/plain', item.code);
              }}
              ondragend={() => (dragging = null)}
            >
              {@render card(item)}
            </article>
          {:else}
            <p class="empty">{t('tickets.index.emptyColumn')}</p>
          {/each}
          {#if col.total > col.items.length}
            <a class="more" href={href({ view: 'list', status: s, sort: s === 'done' ? 'updated' : 'prio' })}
              >{t('tickets.index.more', { n: col.total - col.items.length })}</a
            >
          {/if}
        </section>
      {/each}
    </div>
  {:else if data.view === 'prio' && data.list}
    {#each prioGroups as g (g.prio ?? 'none')}
      {#if g.items.length}
        <section class="group">
          <h2>
            {#if g.prio}<span class="prio {g.prio}">{g.prio}</span>{:else}<span class="muted">{t('tickets.index.noPriority')}</span>{/if}
            <span class="muted">{g.items.length}</span>
          </h2>
          <div class="card rows">
            {#each g.items as item (item.code)}
              <div class="row">
                <div class="grow">{@render card(item)}</div>
                {@render statusSelect(item)}
              </div>
            {/each}
          </div>
        </section>
      {/if}
    {:else}
      <p class="empty">{t('tickets.index.nothing')}</p>
    {/each}
    {#if data.list.total > data.list.items.length}
      <p class="muted">{t('tickets.index.capped', { shown: data.list.items.length, total: data.list.total })}</p>
    {/if}
  {:else if data.list}
    <div class="card table">
      <table>
        <thead>
          <tr>
            <th>{t('tickets.field.code')}</th><th>{t('tickets.field.summary')}</th><th>{t('tickets.field.status')}</th>
            <th><a class="plain" href={href({ sort: data.filters.sort === 'updated' ? null : 'updated' })}>{t('tickets.index.updated')}{data.filters.sort === 'updated' ? ' ↓' : ''}</a></th>
          </tr>
        </thead>
        <tbody>
          {#each data.list.items as item (item.code)}
            <tr>
              <td class="nowrap">
                <a class="code" href="/t/{encodeURIComponent(item.code)}"><code>{item.code}</code></a>
                {#if item.priority}<span class="prio {item.priority}">{item.priority}</span>{/if}
              </td>
              <td>
                <a class="summary" href="/t/{encodeURIComponent(item.code)}">{item.summary}</a>
                <div class="meta">
                  {#if !data.filters.project}<a class="plain" href={href({ project: item.project })}>{item.project}</a>{/if}
                  {#if item.assignee}<a class="plain" href={href({ assignee: item.assignee })}>@{item.assignee}</a>{/if}
                  {#each item.components ?? [] as c (c)}<a class="plain comp" href={href({ component: c })}>{c}</a>{/each}
                  {#each item.tags as tag (tag)}<a class="plain tag" href={href({ tag })}>#{tag}</a>{/each}
                </div>
              </td>
              <td>{@render statusSelect(item)}</td>
              <td class="muted nowrap">{when.format(new Date(item.updated_at))}</td>
            </tr>
          {:else}
            <tr><td colspan="4" class="empty">{t('tickets.index.nothing')}</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
    {#if data.list.total > data.pageSize}
      <nav class="pager" aria-label={t('tickets.index.pages')}>
        {#if data.filters.offset > 0}<a href={href({ offset: Math.max(0, data.filters.offset - data.pageSize) || null })}>{t('tickets.index.prev')}</a>{/if}
        <span class="muted"
          >{t('tickets.index.range', { from: data.filters.offset + 1, to: data.filters.offset + data.list.items.length, total: data.list.total })}</span
        >
        {#if data.filters.offset + data.pageSize < data.list.total}<a href={href({ offset: data.filters.offset + data.pageSize })}>{t('tickets.index.next')}</a>{/if}
      </nav>
    {/if}
  {/if}

  <details class="card new" open={!!form && 'create' in form}>
    <summary>{t('tickets.index.new')}</summary>
    <form method="POST" action="?/create" use:enhance class="create">
      <label
        >{t('tickets.field.project')} <input
          name="project"
          required
          autocomplete="off"
          list="projects"
          pattern="[a-z0-9][a-z0-9\-]{'{'}0,39{'}'}"
          title={t('tickets.index.projectPattern')}
          value={data.filters.project}
        /></label
      >
      <datalist id="projects">
        {#each data.projects as p (p.project)}<option value={p.project}></option>{/each}
      </datalist>
      <label
        >{t('tickets.field.priority')} <select name="priority">
          <option value="">—</option>
          <option>p0</option><option>p1</option><option selected>p2</option><option>p3</option>
        </select></label
      >
      <label class="wide">{t('tickets.field.summary')} <input name="summary" required autocomplete="off" /></label>
      <label class="wide">{t('tickets.index.body')} <textarea name="body" rows="4"></textarea></label>
      <button>{t('tickets.common.add')}</button>
    </form>
  </details>
</main>

<style>
  main {
    max-width: 1400px;
  }
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 12px 24px;
    margin: 12px 0 16px;
  }
  .views {
    display: flex;
    padding: 3px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--card);
  }
  .views a {
    padding: 4px 14px;
    border-radius: 7px;
    color: var(--text);
    text-decoration: none;
    font-size: 14px;
  }
  .views a.active {
    background: var(--accent);
    color: var(--accent-text);
  }
  .bar {
    display: flex;
    flex-wrap: wrap;
    align-items: start;
    gap: 0 16px;
  }
  .search {
    flex: 1 1 280px;
    margin-bottom: 12px;
  }
  .tags a {
    font-size: 12px;
  }
  .component {
    margin-bottom: 12px;
  }
  .component select {
    width: auto;
    max-width: 100%;
  }
  .meta .comp {
    border: 1px solid var(--line);
    border-radius: 6px;
    padding: 0 4px;
  }
  h2 {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 0 8px;
    font-size: 14px;
  }
  .board {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 12px;
    align-items: start;
  }
  .column {
    padding: 12px;
    min-height: 120px;
    transition: border-color 0.1s;
  }
  .column.over {
    border-color: var(--accent);
    outline: 2px dashed var(--accent);
  }
  .ticket {
    padding: 8px 10px;
    margin-bottom: 8px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--bg);
    cursor: grab;
  }
  .ticket.dragging {
    opacity: 0.4;
  }
  .ticket .summary {
    display: block;
    margin-top: 2px;
  }
  .more {
    display: block;
    font-size: 13px;
    text-decoration: none;
  }
  .group {
    margin-bottom: 20px;
  }
  .rows .row {
    display: flex;
    align-items: start;
    gap: 12px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--line);
  }
  .rows .row:last-child {
    border-bottom: 0;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 8px;
    color: var(--muted);
    font-size: 12px;
  }
  .inline select {
    width: auto;
    padding: 2px 6px;
    font-size: 13px;
  }
  .table {
    overflow-x: auto;
  }
  .code,
  .summary,
  .plain {
    color: var(--text);
    text-decoration: none;
  }
  .meta .plain {
    color: var(--muted);
  }
  .summary {
    font-weight: 550;
  }
  .code:hover,
  .summary:hover,
  .plain:hover {
    color: var(--accent);
  }
  .empty {
    padding: 24px 8px;
    text-align: center;
    color: var(--muted);
  }
  .pager {
    display: flex;
    gap: 16px;
    justify-content: center;
    margin: 12px 0;
  }
  .new {
    margin: 24px 0 0;
  }
  .new summary {
    padding: 12px 16px;
    cursor: pointer;
    font-weight: 600;
  }
  .create {
    display: grid;
    grid-template-columns: 200px 140px 1fr;
    gap: 12px 16px;
    align-items: end;
    padding: 0 16px 16px;
  }
  .wide {
    grid-column: 1 / -1;
  }
  .create button {
    justify-self: start;
  }
  @media (max-width: 900px) {
    .board {
      grid-template-columns: 1fr;
    }
  }
  @media (max-width: 640px) {
    .create {
      grid-template-columns: 1fr;
    }
  }
</style>
