<script lang="ts">
  import { t } from '#lib/i18n/index';
  let { data } = $props();
  const SCOPE: Record<string, string> = {
    openid: t('auth.consent.scope.openid'),
    profile: t('auth.consent.scope.profile'),
    email: t('auth.consent.scope.email'),
    offline_access: t('auth.consent.scope.offline_access')
  };
</script>

<svelte:head><title>{t('auth.consent.title')} · {data.hubName}</title></svelte:head>

<h1>{t('auth.consent.title')}</h1>
<p><strong>{data.client}</strong> {t('auth.consent.asks', { name: data.user.name, email: data.user.email })}</p>
<p>{t('auth.consent.replyTo')} <code>{data.redirectUri}</code>. {t('auth.consent.localClients')}
  <code>http://localhost:…</code> {t('auth.consent.or')} <code>http://127.0.0.1:…</code>{t('auth.consent.otherAddress')}</p>
{#if data.scopes.length}
  <ul>
    {#each data.scopes as s (s)}<li>{SCOPE[s] ?? s}</li>{/each}
  </ul>
{/if}
<form method="POST" class="row">
  <button name="accept" value="yes">{t('auth.consent.allow')}</button>
  <button name="accept" value="no" class="secondary">{t('auth.consent.deny')}</button>
</form>
