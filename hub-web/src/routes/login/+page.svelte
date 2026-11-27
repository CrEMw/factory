<script lang="ts">
  import { t } from '#lib/i18n/index';
  let { data } = $props();
</script>

<svelte:head><title>{t('auth.login.title')} · {data.hubName}</title></svelte:head>

<h1>{t('auth.login.title')}</h1>
<p class="error">{data.error}</p>
<p>{t('auth.login.how', { how: data.how })}</p>
