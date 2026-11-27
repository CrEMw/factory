<script lang="ts">
  import { documentHref } from '#lib/document-url';
  import { dateFormat, t } from '#lib/i18n/index';
  let { data } = $props();

  const when = dateFormat({ dateStyle: 'medium' });
  const all = $derived(data.docs.roles.reduce((n, r) => n + r.documents, 0));
  function href(change: Record<string, string | number | null>): string {
    const q = new URLSearchParams();
    const cur: Record<string, string | number | null> = {
      role: data.role,
      repo: data.repo,
      q: data.q,
      retired: data.retired ? 1 : null,
      offset: null,
      ...change
    };
    for (const [k, v] of Object.entries(cur)) if (v) q.set(k, String(v));
    return q.size ? `?${q}` : '?';
  }
</script>

<svelte:head><title>{t('docs.list.title')} · {data.hubName}</title></svelte:head>

<main>
  <div class="head">
    <h1>{t('docs.list.title')}</h1>
    <p class="muted">{data.docs.total}</p>
  </div>
  <nav class="chips" aria-label={t('docs.list.repoNav')}>
    <a href={href({ repo: null })} class:active={!data.repo} aria-current={data.repo ? undefined : 'page'}>{t('docs.list.allRepos')}</a>
    {#each data.docs.repos ?? [] as r (r.repo)}
      <a href={href({ repo: r.repo })} class:active={data.repo === r.repo} aria-current={data.repo === r.repo ? 'page' : undefined}
        >{r.repo.split('/')[1]} <span>{r.documents}</span></a
      >
    {/each}
    <a href={href({ retired: data.retired ? null : 1 })} class:active={data.retired}>{data.retired ? t('docs.list.withRetired') : t('docs.list.liveOnly')}</a>
  </nav>
  <nav class="chips" aria-label={t('docs.list.roleNav')}>
    <a href={href({ role: null })} class:active={!data.role} aria-current={data.role ? undefined : 'page'}>{t('docs.list.allRoles')} <span>{all}</span></a>
    {#each data.docs.roles as r (r.role)}
      <a href={href({ role: r.role })} class:active={data.role === r.role} aria-current={data.role === r.role ? 'page' : undefined}
        >{r.role} <span>{r.documents}</span></a
      >
    {/each}
  </nav>
  <form method="GET" class="search" role="search">
    {#if data.role}<input type="hidden" name="role" value={data.role} />{/if}
    {#if data.repo}<input type="hidden" name="repo" value={data.repo} />{/if}
    {#if data.retired}<input type="hidden" name="retired" value="1" />{/if}
    <input type="search" name="q" value={data.q} placeholder={t('docs.list.searchPlaceholder')} aria-label={t('docs.list.searchLabel')} />
  </form>
  <div class="card table">
    <table>
      <thead><tr><th>{t('docs.list.colDocument')}</th><th>{t('docs.list.colRole')}</th><th>{t('docs.list.colTickets')}</th><th>{t('docs.list.colCreated')}</th></tr></thead>
      <tbody>
        {#each data.docs.items as d (d.repo + d.path)}
          <tr class:retired={d.retired_at}>
            <td><a href={documentHref(d.repo, d.path)}>{d.title}</a><div class="tag">{d.repo} · {d.path}</div></td>
            <td><span class="pill">{d.role}</span></td>
            <td class="muted">{d.tickets || ''}</td>
            <td class="muted nowrap">{when.format(new Date(d.created_at))}</td>
          </tr>
        {:else}
          <tr><td colspan="4" class="muted">{t('docs.list.empty')}</td></tr>
        {/each}
      </tbody>
    </table>
  </div>
  <details class="sync">
    <summary class="muted">{t('docs.list.sync')}</summary>
    <ul>
      {#each data.docs.repos ?? [] as r (r.repo)}
        <li>
          <b>{r.repo}</b>
          {#if r.revision && r.synced_at}
            <span class="muted">{t('docs.list.revision')} <code>{r.revision.slice(0, 7)}</code>, {when.format(new Date(r.synced_at))}</span>
          {:else}
            <span class="muted">{t('docs.list.notSynced')}</span>
          {/if}
          {#if r.problems.length}
            <span class="problems">{t('docs.list.problems', { count: r.problems.length })}</span>
            <ul>
              {#each r.problems as p, i (i)}
                <li class="tag">{p.kind} · {p.path}{p.detail ? ` · ${p.detail}` : ''}</li>
              {/each}
            </ul>
          {/if}
        </li>
      {/each}
    </ul>
  </details>
  {#if data.docs.total > data.pageSize}
    <nav class="pager" aria-label={t('docs.list.pagesNav')}>
      {#if data.offset > 0}<a href={href({ offset: Math.max(0, data.offset - data.pageSize) || null })}>{t('docs.list.prev')}</a>{/if}
      <span class="muted">{t('docs.list.range', { from: data.offset + 1, to: data.offset + data.docs.items.length, total: data.docs.total })}</span>
      {#if data.offset + data.pageSize < data.docs.total}<a href={href({ offset: data.offset + data.pageSize })}>{t('docs.list.next')}</a>{/if}
    </nav>
  {/if}
</main>

<style>
  .head {
    display: flex;
    align-items: baseline;
    gap: 12px;
    margin: 12px 0 16px;
  }
  .head p {
    margin: 0;
  }
  .search {
    margin-bottom: 12px;
  }
  .pager {
    display: flex;
    gap: 16px;
    justify-content: center;
    margin: 12px 0;
  }
  .table {
    overflow-x: auto;
  }
  td a {
    color: var(--text);
    font-weight: 550;
    text-decoration: none;
  }
  td a:hover {
    color: var(--accent);
  }
  .tag {
    overflow-wrap: anywhere;
  }
  .retired {
    opacity: 0.6;
  }
  .sync {
    margin: 12px 0;
  }
  .sync ul {
    margin: 4px 0;
    padding-left: 20px;
  }
  .problems {
    color: var(--danger);
    margin-left: 8px;
  }
</style>
