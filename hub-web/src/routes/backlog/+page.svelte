<script lang="ts">
  // Backlog by WSJF: cost of delay (value + criticality + risk) per estimate point. A ticket without an
  // estimate or WSJF goes last; both are set on the ticket page.
  import { STATUS_NAMES, wsjfText } from '#lib/plan/flags';
  import { t } from '#lib/i18n/index';

  let { data } = $props();
</script>

<svelte:head><title>{t('tickets.backlog.title')} · {data.hubName}</title></svelte:head>

<main>
  <h1>{t('tickets.backlog.title')}</h1>
  <p class="muted">{t('tickets.backlog.intro')}</p>

  <form class="card filters" method="GET">
    <label>
      {t('tickets.field.project')}
      <select name="project">
        <option value="">{t('tickets.common.all')}</option>
        {#each data.projects as p (p.project)}<option value={p.project} selected={data.project === p.project}>{p.project}</option>{/each}
      </select>
    </label>
    <label>
      {t('tickets.backlog.limit')}
      <select name="limit">
        {#each data.limits as l (l)}<option value={l} selected={data.limit === l}>{l}</option>{/each}
      </select>
    </label>
    <button>{t('tickets.common.show')}</button>
  </form>

  {#if data.bad}
    <p role="alert">{t('tickets.backlog.bad')}</p>
  {:else if data.missing}
    <p class="muted">{t('tickets.backlog.missing')}</p>
  {:else if data.failed || !data.backlog}
    <p role="alert">{t('tickets.backlog.failed')}</p>
  {:else}
    {@const b = data.backlog}
    {#if b.scopes.length}
      <section class="card scopes">
        <h2>{t('tickets.backlog.scopes')}</h2>
        <ul>
          {#each b.scopes as s (s.root)}
            <li><a href="/plan/{encodeURIComponent(s.root)}"><code>{s.root}</code> {s.summary}</a> <span class="muted">WSJF {wsjfText(s.wsjf_root)}</span></li>
          {/each}
        </ul>
      </section>
    {/if}
    <p class="muted small">{t('tickets.backlog.shown', { shown: b.items.length, total: b.total })}</p>
    <div class="card scroll">
      <table>
        <thead>
          <tr><th>{t('tickets.field.ticket')}</th><th class="num">WSJF</th><th class="num" title={t('tickets.backlog.costOfDelay')}>CoD</th><th class="num" title={t('tickets.backlog.estimatePoints')}>{t('tickets.backlog.points')}</th></tr>
        </thead>
        <tbody>
          {#each b.items as item (item.code)}
            <tr>
              <td>
                <a href="/t/{encodeURIComponent(item.code)}"><code>{item.code}</code> {item.summary}</a>
                <span class="sub">
                  <span class="tag">{item.project}</span>
                  {#if item.status !== 'open'}<span class="pill {item.status}">{STATUS_NAMES[item.status] ?? item.status}</span>{/if}
                  {#if item.priority}<span class="prio {item.priority}">{item.priority}</span>{/if}
                </span>
              </td>
              <td class="num">{wsjfText(item.wsjf)}</td>
              <td class="num">{item.cost_of_delay ?? '—'}</td>
              <td class="num">{item.effort_sp ?? '—'}</td>
            </tr>
          {:else}
            <tr><td colspan="4" class="muted">{t('tickets.backlog.empty')}</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</main>

<style>
  h1 {
    margin: 12px 0 4px;
  }
  h2 {
    margin: 0 0 6px;
    font-size: 16px;
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
    flex: 1 1 120px;
    min-width: 0;
  }
  .scopes {
    margin-bottom: 12px;
    padding: 12px 16px;
  }
  .scopes ul {
    margin: 0;
    padding-left: 20px;
  }
  .small {
    font-size: 13px;
  }
  .scroll {
    overflow-x: auto;
  }
  td a {
    overflow-wrap: anywhere;
  }
  .sub {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 2px;
  }
  th.num,
  td.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  th,
  td {
    padding: 8px 10px;
  }
</style>
