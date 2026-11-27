<script lang="ts">
  import { enhance } from '$app/forms';
  import { documentHref } from '#lib/document-url';
  import { dateFormat, t, type Key } from '#lib/i18n/index';

  let { data, form } = $props();

  const when = dateFormat({ dateStyle: 'medium', timeStyle: 'short' });
  const d = $derived(data.document);
  // Related documents by rel and direction: out means this document is the edge source.
  const REL: Record<string, [Key, Key]> = {
    implements: ['docs.doc.implements', 'docs.doc.implementedIn'],
    specifies: ['docs.doc.specifies', 'docs.doc.specifiedIn'],
    references: ['docs.doc.references', 'docs.doc.referencedIn']
  };
  const groups = $derived(
    Object.entries(REL)
      .flatMap(([rel, [out, inn]]) => [
        { title: t(out), items: (d.documents ?? []).filter((l) => l.rel === rel && l.dir === 'out') },
        { title: t(inn), items: (d.documents ?? []).filter((l) => l.rel === rel && l.dir === 'in') }
      ])
      .filter((g) => g.items.length)
  );
  // Form action keeps repo in the URL: otherwise the page would reload without repo (a document of the default repository).
  const action = (name: string) => `?repo=${encodeURIComponent(d.repo)}&/${name}`;
  const keep = () => ({ update }: { update: (o?: { reset?: boolean }) => Promise<void> }) => update({ reset: false });
</script>

<svelte:head><title>{d.title} · {data.hubName}</title></svelte:head>

<main>
  <p class="crumbs"><a href="/d?repo={encodeURIComponent(d.repo)}&role={encodeURIComponent(d.role)}">{d.role}</a> / <span class="tag">{d.repo} · {d.path}</span></p>
  <h1>{d.title}</h1>
  <p class="muted meta">
    {t('docs.doc.created', { date: when.format(new Date(d.created_at)) })}
    {#if d.source_revision}· {t('docs.doc.revision')} <code>{d.source_revision.slice(0, 10)}</code>{/if}
    {#if d.retired_at}· {t('docs.doc.retired', { date: when.format(new Date(d.retired_at)) })}{/if}
  </p>

  {#if d.project}
    <section class="card tickets">
      <h2>{t('docs.doc.project')}</h2>
      <a href="/?project={encodeURIComponent(d.project)}">{d.project}</a>
    </section>
  {/if}

  {#if groups.length}
    <section class="card tickets">
      <h2>{t('docs.doc.related')}</h2>
      {#each groups as g (g.title)}
        <h3>{g.title}</h3>
        <ul>
          {#each g.items as l (l.edge_id)}
            <li>
              <span>
                <span class="tag">{l.role}</span>
                <a href={documentHref(l.repo, l.path)}>{l.title}</a>
                {#if l.repo !== d.repo}<span class="tag">{l.repo}</span>{/if}
                {#if l.retired}<span class="tag">{t('docs.doc.linkRetired')}</span>{/if}
                {#if l.state === 'proposed'}<span class="tag">{t('docs.doc.proposed')}</span>{/if}
              </span>
              {#if l.state === 'proposed'}
                <span class="actions">
                  <form method="POST" action={action('confirm')} use:enhance={keep}>
                    <input type="hidden" name="id" value={l.edge_id} /><button class="small">{t('docs.doc.confirm')}</button>
                  </form>
                  <form method="POST" action={action('reject')} use:enhance={keep}>
                    <input type="hidden" name="id" value={l.edge_id} /><button class="small">{t('docs.doc.reject')}</button>
                  </form>
                </span>
              {/if}
            </li>
          {/each}
        </ul>
      {/each}
      {#if form?.linkMessage}<p role="alert">{form.linkMessage}</p>{/if}
    </section>
  {/if}

  {#if d.tickets.length}
    <section class="card tickets">
      <h2>{t('docs.doc.tickets')}</h2>
      <ul>
        {#each d.tickets as tk (tk.code + tk.rel)}
          <li>
            <a href="/t/{encodeURIComponent(tk.code)}"><code>{tk.code}</code> {tk.summary}</a>
            <span>
              <span class="tag">{tk.rel === 'closes' ? t('docs.doc.closes') : t('docs.doc.describes')}</span>
              <span class="pill {tk.status}">{tk.status}</span>
            </span>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  <article class="card prose">{@html data.html}</article>
</main>

<style>
  .crumbs {
    margin: 12px 0 4px;
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .meta {
    margin: 8px 0 16px;
  }
  .tickets {
    padding: 12px 16px;
    margin-bottom: 16px;
  }
  .tickets h2 {
    margin: 0 0 8px;
    font-size: 15px;
  }
  .tickets h3 {
    margin: 8px 0 4px;
    font-size: 13px;
    color: var(--muted);
  }
  .actions {
    display: inline-flex;
    gap: 4px;
    margin-left: auto;
  }
  .actions form {
    display: inline;
    padding: 0;
  }
  .small {
    font-size: 13px;
  }
  .tickets ul {
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 6px;
  }
  .tickets li {
    display: flex;
    gap: 8px;
    align-items: baseline;
    justify-content: space-between;
  }
  .tickets a {
    color: var(--text);
    text-decoration: none;
  }
  .tickets a:hover {
    color: var(--accent);
  }
  article {
    padding: 20px 24px;
  }
</style>
