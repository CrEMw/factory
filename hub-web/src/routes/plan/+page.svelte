<script lang="ts">
  // Plan scopes: root, project buffer zone, forecast finish (end of PB), whether the baseline is approved.
  import { dateFormat, t } from '#lib/i18n/index';
  import { flagName, wsjfText, ZONE_NAMES } from '#lib/plan/flags';

  let { data } = $props();
  const day = dateFormat({ dateStyle: 'medium', timeZone: 'UTC' });
  const fmt = (d: string) => day.format(new Date(`${d}T00:00:00Z`));
</script>

<svelte:head><title>{t('plan.list.heading')} · {data.hubName}</title></svelte:head>

<main>
  <h1>{t('plan.list.heading')}</h1>
  <p class="muted">{t('plan.list.intro')}</p>
  {#if data.failed}
    <p role="alert">{t('plan.list.failed')}</p>
  {:else if data.scopes === null}
    <p class="muted">{t('plan.list.notYet')}</p>
  {:else}
    <ul class="card list">
      {#each data.rows as r (r.scope.root)}
        <li class="row">
          <span class="zone {r.zone ?? 'none'}" title={r.zone ? t('plan.list.zoneTitle', { zone: ZONE_NAMES[r.zone] }) : t('plan.list.noZoneTitle')} aria-hidden="true"></span>
          <a class="name" href="/plan/{encodeURIComponent(r.scope.root)}"><code>{r.scope.root}</code> {r.scope.summary}</a>
          <span class="meta muted">
            {#if r.zone}
              {t('plan.zoneOf', { zone: ZONE_NAMES[r.zone] })}{#if r.consumed != null}{t('plan.list.consumed', { pct: r.consumed })}{/if}
            {:else if r.scope.baseline_id === null}
              {t('plan.list.noBaseline')}
            {:else}
              {t('plan.list.noZone')}
            {/if}
            · {t('plan.list.forecast', { date: r.end_date ? fmt(r.end_date) : '—' })}
            {#if r.scope.due_date}· {t('plan.dueBy', { date: fmt(r.scope.due_date) })}{/if}
            {#if r.late_by > 0}· {t('plan.lateBy', { count: r.late_by })}{/if}
            {#if r.scope.wsjf_root != null}· WSJF {wsjfText(r.scope.wsjf_root)}{/if}
            {#if r.scope.pending}· {t('plan.list.pending')}{/if}
            {#if r.flags.length}· {r.flags.map(flagName).join(', ')}{/if}
            {#if r.planFailed}· {t('plan.list.planFailed')}{/if}
          </span>
        </li>
      {:else}
        <li class="row muted">{t('plan.list.empty')}</li>
      {/each}
    </ul>
  {/if}
</main>

<style>
  h1 {
    margin: 12px 0 4px;
  }
  .list {
    margin: 16px 0 0;
    padding: 0;
    list-style: none;
  }
  .row {
    display: grid;
    grid-template-columns: 12px minmax(0, 1fr);
    gap: 2px 10px;
    align-items: baseline;
    padding: 10px 16px;
    border-bottom: 1px solid var(--line);
  }
  .row:last-child {
    border-bottom: 0;
  }
  .name {
    overflow-wrap: anywhere;
  }
  .meta {
    grid-column: 2;
    font-size: 13px;
  }
  .zone {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    border: 2px solid var(--muted);
    box-sizing: border-box;
  }
  .zone.green {
    background: var(--zone-green);
    border-color: var(--zone-green);
  }
  .zone.yellow {
    background: var(--zone-yellow);
    border-color: var(--zone-yellow);
  }
  .zone.red {
    background: var(--zone-red);
    border-color: var(--zone-red);
  }
</style>
