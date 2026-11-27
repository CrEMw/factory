<script lang="ts">
  import { enhance } from '$app/forms';
  import { t } from '#lib/i18n/index';

  let { data, form } = $props();

  const bound = $derived(new Set((data.logins ?? []).map((l) => l.handle)));
  // "This is me": a human (not an agent) whose handle has no sign-in bound yet.
  const candidates = $derived((data.people ?? []).filter((p) => !p.is_agent && !p.retired && !bound.has(p.handle)));
</script>

<svelte:head><title>{t('agents.people.title')} · {data.hubName}</title></svelte:head>

<main>
  <h1>{t('agents.people.title')}</h1>
  <p class="muted">{t('agents.people.intro')}</p>
  {#if data.failed}
    <p role="alert">{t('agents.people.failed')}</p>
  {:else if data.people === null}
    <p class="muted">{t('agents.people.rollingOut')}</p>
  {:else}
    <div class="card list">
      {#each data.people as p (p.handle)}
        <div class="row" class:retired={p.retired}>
          <a class="name" href="/?assignee={encodeURIComponent(p.handle)}&view=list">{p.name}</a>
          <code>@{p.handle}</code>
          {#if p.is_agent}<span class="tag">{t('agents.people.tagAgent')}</span>{/if}
          {#if p.retired}<span class="tag">{t('agents.people.tagRetired')}</span>{/if}
          {#if data.mine === p.handle}<span class="tag">{t('agents.people.tagYou')}</span>{:else if bound.has(p.handle)}<span class="tag">{t('agents.people.tagBound')}</span>{/if}
          <span class="muted count">{p.active} / {p.tickets}</span>
          {#if bound.has(p.handle) && data.signedIn}
            <form method="POST" action="?/retire" use:enhance>
              <input type="hidden" name="handle" value={p.handle} />
              <button class="link danger" title={t('agents.people.retireTitle')}>{t('agents.people.retire')}</button>
            </form>
          {/if}
        </div>
      {:else}
        <p class="muted pad">{t('agents.people.empty')}</p>
      {/each}
      <div class="row">
        <a class="name" href="/?assignee=none&view=list">{t('agents.people.noAssignee')}</a>
      </div>
    </div>

    <section class="card me" aria-label={t('agents.people.me')}>
      <h2>{t('agents.people.me')}</h2>
      {#if data.loginsMissing}
        <p class="muted pad">{t('agents.people.loginsRollingOut')}</p>
      {:else if data.logins === null}
        <p class="pad" role="alert">{t('agents.people.loginsFailed')}</p>
      {:else if !data.signedIn}
        <p class="muted pad">{t('agents.people.signInToBind')} <a href={data.relogin}>{t('agents.people.signIn')}</a>.</p>
      {:else if data.mine}
        <p class="pad">{t('agents.people.mineBefore')} <code>@{data.mine}</code>. {t('agents.people.mineAfter')}</p>
      {:else}
        <p class="muted pad">
          {t('agents.people.choose')}
        </p>
        <form method="POST" action="?/bind" use:enhance>
          <label
            >{t('agents.people.person')}
            <select name="handle" required>
              {#each candidates as p (p.handle)}<option value={p.handle}>{p.name} (@{p.handle})</option>{/each}
            </select>
          </label>
          <button disabled={!candidates.length}>{t('agents.people.me')}</button>
        </form>
      {/if}
      {#if data.signedIn && !data.fresh && !data.loginsMissing}
        <p class="muted small pad">
          {t('agents.people.freshNeeded')} <a href={data.relogin}>{t('agents.signInAgainLower')}</a>.
        </p>
      {/if}
      {#if form?.bindMessage || form?.retireMessage}
        <p class="pad" role="alert">
          {form.bindMessage ?? form.retireMessage}
          {#if form.relogin}<a href={form.relogin}>{t('agents.signInAgain')}</a>{/if}
        </p>
      {:else if form?.bound}<p class="muted pad" role="status">{t('agents.people.bound', { handle: form.bound })}</p>
      {:else if form?.retired}<p class="muted pad" role="status">{t('agents.people.retired', { handle: form.retired })}</p>{/if}
    </section>

    <details class="card new" open={!!form?.message || !!form?.created}>
      <summary>{t('agents.people.add')}</summary>
      <form method="POST" action="?/create" use:enhance>
        <label>handle <input name="handle" required pattern="[a-z][a-z0-9\-]{'{'}0,39{'}'}" autocomplete="off" /></label>
        <label>{t('agents.people.name')} <input name="name" required maxlength="200" autocomplete="off" /></label>
        <button>{t('agents.people.addButton')}</button>
      </form>
      {#if !data.signedIn}<p class="muted small pad">{t('agents.people.addNeedsSession')}</p>{/if}
      {#if form?.message}<p class="pad" role="alert">{form.message}</p>{:else if form?.created}<p class="muted pad" role="status">{t('agents.people.added', { handle: form.created })}</p>{/if}
    </details>
  {/if}
</main>

<style>
  h1 {
    margin: 12px 0 4px;
  }
  .list {
    margin-top: 16px;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px 10px;
    padding: 10px 16px;
    border-bottom: 1px solid var(--line);
  }
  .row:last-child {
    border-bottom: 0;
  }
  .row.retired {
    opacity: 0.6;
  }
  .name {
    color: var(--text);
    font-weight: 600;
    text-decoration: none;
  }
  .name:hover {
    color: var(--accent);
  }
  .count {
    margin-left: auto;
  }
  .pad {
    padding: 0 16px 12px;
  }
  .small {
    font-size: 13px;
  }
  .new,
  .me {
    margin-top: 24px;
  }
  .me h2 {
    margin: 0;
    padding: 12px 16px;
    font-size: 15px;
  }
  .me form {
    display: flex;
    flex-wrap: wrap;
    gap: 12px 16px;
    align-items: end;
    padding: 0 16px 16px;
  }
  .row form {
    display: inline;
  }
  .link {
    background: none;
    border: 0;
    padding: 0;
    font-size: 13px;
    cursor: pointer;
  }
  .danger {
    color: var(--danger);
  }
  .new summary {
    padding: 12px 16px;
    cursor: pointer;
    font-weight: 600;
  }
  .new form {
    display: grid;
    grid-template-columns: 200px 1fr auto;
    gap: 12px 16px;
    align-items: end;
    padding: 0 16px 16px;
  }
  @media (max-width: 640px) {
    .new form {
      grid-template-columns: 1fr;
    }
  }
</style>
