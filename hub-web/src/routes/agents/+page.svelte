<script lang="ts">
  import { onMount } from 'svelte';
  import { enhance } from '$app/forms';
  import { invalidateAll } from '$app/navigation';
  import { readSeen, writeSeen } from '#lib/agents-seen';
  import { dateFormat, t } from '#lib/i18n/index';

  let { data, form } = $props();

  const when = dateFormat({ dateStyle: 'short', timeStyle: 'short' });

  // The "new" separator: the last seen id lives in the browser. Without storage there is simply no separator.
  let seen = $state(0);
  const fresh = $derived(data.messages.filter((m) => m.id > seen).length);
  // curl key fingerprint: first 12 chars of sha256 (the agent script prints the same locally); compare before approving.
  const short = (id: string) => (id.startsWith('sha256:') ? t('agents.keys.curlKey', { fp: id.slice(7, 19) }) : t('agents.keys.hubLogin'));

  onMount(() => {
    try {
      seen = readSeen(localStorage);
    } catch {}
    // Poll every 15 s, paused on a hidden tab.
    const timer = setInterval(() => {
      if (document.visibilityState === 'visible') invalidateAll();
    }, 15_000);
    const remember = () => {
      const last = data.messages.at(-1)?.id;
      if (last && document.visibilityState === 'hidden') {
        try {
          writeSeen(localStorage, last);
        } catch {}
      }
    };
    document.addEventListener('visibilitychange', remember);
    return () => {
      clearInterval(timer);
      document.removeEventListener('visibilitychange', remember);
      remember();
    };
  });
</script>

<svelte:head><title>{t('agents.title')} · {data.hubName}</title></svelte:head>

<main>
  <h1>{t('agents.heading')}</h1>
  <p class="muted">{t('agents.intro')}</p>

  <div class="layout">
    <section class="card feed" aria-label={t('agents.feed.label')}>
      {#each data.messages as m (m.id)}
        {#if seen && m.id === data.messages.find((x) => x.id > seen)?.id}
          <p class="new" role="separator">{t('agents.feed.new', { n: fresh })}</p>
        {/if}
        <p class="msg">
          <time class="muted" datetime={m.created_at}>{when.format(new Date(m.created_at))}</time>
          <b>{m.agent}</b>
          <span>{m.text}</span>
        </p>
      {:else}
        <p class="muted">{t('agents.feed.empty')}</p>
      {/each}
    </section>

    <aside class="card keys">
      <h2>{t('agents.keys.heading')}</h2>
      {#if !data.me}
        <p class="muted small">
          {t('agents.keys.ownerOnly')} {#if data.signedIn}{t('agents.keys.bindOn')} <a href="/people">{t('agents.keys.peopleLink')}</a>{:else}<a
              href={data.relogin}>{t('agents.keys.signIn')}</a> {t('agents.keys.andBind')}{/if}.
        </p>
      {:else if !data.fresh}
        <p class="muted small">{t('agents.keys.freshNeeded')} <a href={data.relogin}>{t('agents.signInAgainLower')}</a>.</p>
      {/if}
      {#if form?.message}<p role="alert">{form.message} {#if form.relogin}<a href={form.relogin}>{t('agents.signInAgain')}</a>{/if}</p>{/if}
      <ul>
        {#each data.keys as k (k.id)}
          <li>
            <span><b>{k.owner}</b> <span class="pill {k.status}">{k.status}</span></span>
            <span class="muted small"><code>{short(k.id)}</code> · {t('agents.keys.lastSeen', { when: when.format(new Date(k.last_seen_at)) })}</span>
            {#if data.me === k.owner}
              <form method="POST" action="?/status" use:enhance>
                <input type="hidden" name="id" value={k.id} />
                {#if k.status !== 'approved'}<button name="status" value="approved">{t('agents.keys.approve')}</button>{/if}
                {#if k.status !== 'revoked'}<button name="status" value="revoked" class="danger">{t('agents.keys.revoke')}</button>{/if}
              </form>
            {:else if k.status === 'pending' && data.me && !k.owner_bound}
              <form method="POST" action="?/status" use:enhance>
                <input type="hidden" name="id" value={k.id} />
                <span class="muted small">{t('agents.keys.unbound', { owner: k.owner })}</span>
                <button name="status" value="revoked" class="danger">{t('agents.keys.revoke')}</button>
              </form>
            {:else if k.status === 'pending'}
              <span class="muted small">{t('agents.keys.ownerApproves', { owner: k.owner })}</span>
            {/if}
          </li>
        {:else}
          <li class="muted">{t('agents.keys.empty')}</li>
        {/each}
      </ul>
    </aside>
  </div>
</main>

<style>
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 300px;
    gap: 16px;
    align-items: start;
    margin-top: 16px;
  }
  @media (max-width: 800px) {
    .layout {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .feed {
    padding: 8px 16px;
  }
  .msg {
    display: grid;
    grid-template-columns: auto auto minmax(0, 1fr);
    gap: 10px;
    margin: 0;
    padding: 8px 0;
    border-bottom: 1px solid var(--line);
    overflow-wrap: anywhere;
  }
  /* On a phone the text takes its own full-width line under the time and the agent. */
  @media (max-width: 600px) {
    .msg {
      grid-template-columns: auto minmax(0, 1fr);
      gap: 2px 10px;
    }
    .msg > span {
      grid-column: 1 / -1;
    }
  }
  .new {
    margin: 8px 0 0;
    color: var(--accent);
    font-weight: 600;
    border-bottom: 2px solid var(--accent);
  }
  .keys {
    padding: 12px 16px;
  }
  .keys h2 {
    margin: 0 0 8px;
    font-size: 15px;
  }
  .keys ul {
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 12px;
  }
  .keys li {
    display: grid;
    gap: 4px;
  }
  .keys form {
    display: flex;
    gap: 8px;
  }
  .small {
    font-size: 13px;
  }
  .danger {
    color: var(--danger);
  }
</style>
