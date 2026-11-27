<script lang="ts">
  import { t } from '#lib/i18n/index';
  let { data, form } = $props();
</script>

<svelte:head><title>{t('auth.fresh.title')} · {data.hubName}</title></svelte:head>

<main>
  <h1>{t('auth.fresh.title')}</h1>
  <p class="muted">
    {t('auth.fresh.intro', { how: data.how })} <code>{data.next}</code>.
  </p>
  {#if data.signedIn && data.fresh}<p class="muted">{t('auth.fresh.alreadyFresh')} <a href={data.next}>{t('auth.fresh.goBack')}</a>.</p>{/if}
  <form method="POST">
    <input type="hidden" name="next" value={data.next} />
    <button>{t('auth.fresh.title')}</button>
  </form>
  {#if form?.message}<p role="alert">{t('auth.fresh.failed', { message: form.message })}</p>{/if}
</main>

<style>
  form {
    margin-top: 16px;
  }
</style>
