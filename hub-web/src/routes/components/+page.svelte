<script lang="ts">
  import { enhance } from '$app/forms';
  import { t } from '#lib/i18n/index';

  let { data, form } = $props();

  type C = NonNullable<typeof data.components>[number];
  // By project in hub's order (project, then slug); no project last.
  const groups = $derived(
    (data.components ?? []).reduce<{ project: string; items: C[] }[]>((out, c) => {
      const project = c.project ?? '';
      const last = out.at(-1);
      if (last && last.project === project) last.items.push(c);
      else out.push({ project, items: [c] });
      return out;
    }, [])
  );
</script>

<svelte:head><title>{t('tickets.components.title')} · {data.hubName}</title></svelte:head>

<main>
  <h1>{t('tickets.components.title')}</h1>
  <p class="muted">{t('tickets.components.intro')}</p>
  {#if data.failed}
    <p role="alert">{t('tickets.components.failed')}</p>
  {:else if data.components === null}
    <p class="muted">{t('tickets.components.missing')}</p>
  {:else}
    {#each groups as g (g.project)}
      <section>
        <h2>{#if g.project}<a class="plain" href="/?project={encodeURIComponent(g.project)}">{g.project}</a>{:else}{t('tickets.components.noProject')}{/if}</h2>
        <div class="card list">
          {#each g.items as c (c.slug)}
            <div class="row" class:retired={c.retired}>
              <a class="name" href="/?component={encodeURIComponent(c.slug)}&view=list">{c.title}</a>
              <code>{c.slug}</code>
              {#if c.retired}<span class="tag">{t('tickets.components.retired')}</span>{/if}
              <span class="muted count">{c.active} / {c.tickets}</span>
              {#if c.description}<p class="muted small desc">{c.description}</p>{/if}
            </div>
          {/each}
        </div>
      </section>
    {:else}
      <p class="muted">{t('tickets.components.none')}</p>
    {/each}

    <details class="card new" open={!!form}>
      <summary>{t('tickets.components.add')}</summary>
      <form method="POST" action="?/create" use:enhance>
        <label>slug <input name="slug" required pattern="[a-z0-9][a-z0-9\-]{'{'}0,39{'}'}" autocomplete="off" /></label>
        <label>{t('tickets.components.name')} <input name="title" required maxlength="200" autocomplete="off" /></label>
        <label>{t('tickets.field.project')} <input name="project" pattern="[a-z0-9][a-z0-9\-]{'{'}0,39{'}'}" autocomplete="off" /></label>
        <button>{t('tickets.common.add')}</button>
      </form>
      {#if !data.signedIn}<p class="muted small pad">{t('tickets.components.needSession')}</p>{/if}
      {#if form?.message}<p class="pad" role="alert">{form.message}</p>{:else if form?.created}<p class="muted pad" role="status">{t('tickets.components.added', { slug: form.created })}</p>{/if}
    </details>
  {/if}
</main>

<style>
  h1 {
    margin: 12px 0 4px;
  }
  h2 {
    margin: 20px 0 8px;
    font-size: 15px;
  }
  .plain {
    color: var(--text);
    text-decoration: none;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 10px;
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
    overflow-wrap: anywhere;
  }
  .name:hover,
  .plain:hover {
    color: var(--accent);
  }
  .count {
    margin-left: auto;
  }
  .desc {
    flex-basis: 100%;
    margin: 0;
  }
  .small {
    font-size: 13px;
  }
  .pad {
    padding: 0 16px 12px;
  }
  .new {
    margin-top: 24px;
  }
  .new summary {
    padding: 12px 16px;
    cursor: pointer;
    font-weight: 600;
  }
  .new form {
    display: grid;
    grid-template-columns: 180px 1fr 160px auto;
    gap: 12px 16px;
    align-items: end;
    padding: 0 16px 16px;
  }
  @media (max-width: 760px) {
    .new form {
      grid-template-columns: 1fr;
    }
  }
</style>
