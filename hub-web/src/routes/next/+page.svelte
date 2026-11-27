<script lang="ts">
  // "Next": the current doing task or the next ready one across all scopes, flags and resource buffer
  // warnings. hub computes the order (ccpm::next), the page only shows it.
  import { flagName, WARNING_NAMES } from '#lib/plan/flags';
  import { t } from '#lib/i18n/index';

  let { data } = $props();
</script>

<svelte:head><title>{t('tickets.next.title')} · {data.hubName}</title></svelte:head>

<main>
  <h1>{t('tickets.next.title')}</h1>
  <p class="muted">{t('tickets.next.intro')}</p>

  <form class="card pick" method="GET">
    <label>
      {t('tickets.next.person')}
      <select name="person">
        <option value="" selected={!data.person}>—</option>
        {#each data.people as p (p.handle)}
          <option value={p.handle} selected={data.person === p.handle}>{p.name} (@{p.handle}){p.handle === data.mine ? ` · ${t('tickets.next.you')}` : ''}</option>
        {/each}
      </select>
    </label>
    <button>{t('tickets.common.show')}</button>
  </form>

  {#if data.peopleFailed}<p class="muted small" role="alert">{t('tickets.next.peopleFailed')}</p>{/if}
  {#if !data.person}
    <p class="muted">{data.mine ? t('tickets.next.pick') : t('tickets.next.pickUnknown')}</p>
  {:else if data.bad}
    <p role="alert">{t('tickets.next.bad')}</p>
  {:else if data.missing}
    <p class="muted">{t('tickets.next.missing')}</p>
  {:else if data.nextFailed}
    <p role="alert">{t('tickets.next.failed')}</p>
  {:else if data.next}
    {@const n = data.next}
    <section class="card task">
      {#if n.code}
        <p class="muted small">@{n.person}{#if n.flags.includes('multitasking')} · {t('tickets.next.multitasking')}{/if}</p>
        <p class="code">
          <a href="/t/{encodeURIComponent(n.code)}"><code>{n.code}</code>{#if data.summary} {data.summary}{/if}</a>
        </p>
        <p class="muted small">
          {#if n.scope}{t('tickets.next.scope')} <a href="/plan/{encodeURIComponent(n.scope)}"><code>{n.scope}</code></a>{:else}{t('tickets.next.noScope')}{/if}
        </p>
        {#if n.flags.length}
          <p class="flags">{#each n.flags as f (f)}<span class="pill">{flagName(f)}</span>{/each}</p>
        {/if}
      {:else}
        <p>{t('tickets.next.nothing', { person: n.person })}</p>
      {/if}
    </section>
    {#if n.warnings.length}
      <section class="card task">
        <h2>{t('tickets.next.buffer')}</h2>
        <ul>
          {#each n.warnings as w, i (i)}
            <li>
              {t('tickets.next.prepare')} <a href="/t/{encodeURIComponent(w.code)}"><code>{w.code}</code></a> {t('tickets.next.after')}
              <a href="/t/{encodeURIComponent(w.after)}"><code>{w.after}</code></a>
              ({WARNING_NAMES[w.reason] ?? w.reason}), {t('tickets.next.scope')} <a href="/plan/{encodeURIComponent(w.scope)}"><code>{w.scope}</code></a>
            </li>
          {/each}
        </ul>
      </section>
    {/if}
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
  .pick {
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    gap: 8px 16px;
    margin: 16px 0;
    padding: 12px 16px;
  }
  .pick label {
    min-width: 0;
    flex: 1 1 200px;
  }
  .task {
    margin-top: 12px;
    padding: 14px 16px;
  }
  .task p {
    margin: 4px 0;
  }
  .code {
    font-size: 18px;
    overflow-wrap: anywhere;
  }
  .small {
    font-size: 13px;
  }
  .flags {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  ul {
    margin: 0;
    padding-left: 20px;
  }
</style>
