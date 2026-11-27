<script lang="ts">
  import '../app.css';
  import { untrack } from 'svelte';
  import { page } from '$app/state';
  import { setLocale, t } from '#lib/i18n/index';

  let { children, data } = $props();
  // Before children render: the client gets the server locale (HUB_LOCALE) from layout data; it never changes.
  setLocale(untrack(() => data.locale));

  const docs = $derived(page.url.pathname.startsWith('/d'));
  const agents = $derived(page.url.pathname.startsWith('/agents'));
  const people = $derived(page.url.pathname.startsWith('/people'));
  const components = $derived(page.url.pathname.startsWith('/components'));
  const graph = $derived(page.url.pathname.startsWith('/graph'));
  const plan = $derived(page.url.pathname.startsWith('/plan'));
  const next = $derived(page.url.pathname.startsWith('/next'));
  const backlog = $derived(page.url.pathname.startsWith('/backlog'));
  const tickets = $derived(!docs && !agents && !people && !components && !graph && !plan && !next && !backlog);
</script>

<header class="top">
  <a class="brand" href="/">{data.hubName}</a>
  <nav aria-label={t('core.nav.label')}>
    <a href="/" aria-current={tickets ? 'page' : undefined}>{t('core.nav.tickets')}</a>
    <a href="/graph" aria-current={graph ? 'page' : undefined}>{t('core.nav.graph')}</a>
    <a href="/plan" aria-current={plan ? 'page' : undefined}>{t('core.nav.plan')}</a>
    <a href="/next" aria-current={next ? 'page' : undefined}>{t('core.nav.next')}</a>
    <a href="/backlog" aria-current={backlog ? 'page' : undefined}>{t('core.nav.backlog')}</a>
    <a href="/d" aria-current={docs ? 'page' : undefined}>{t('core.nav.docs')}</a>
    <a href="/components" aria-current={components ? 'page' : undefined}>{t('core.nav.components')}</a>
    <a href="/people" aria-current={people ? 'page' : undefined}>{t('core.nav.people')}</a>
    <a href="/agents" aria-current={agents ? 'page' : undefined}>{t('core.nav.agents')}</a>
  </nav>
</header>

{@render children()}

<style>
  .top {
    display: flex;
    align-items: center;
    gap: 24px;
    max-width: 1100px;
    margin: 0 auto;
    padding: 14px 16px 0;
  }
  .brand {
    color: var(--text);
    font-weight: 700;
    text-decoration: none;
  }
  nav {
    display: flex;
    flex-wrap: wrap;
    min-width: 0;
    gap: 4px 16px;
  }
  nav a {
    color: var(--muted);
    text-decoration: none;
  }
  nav a[aria-current='page'] {
    color: var(--text);
    font-weight: 600;
  }
</style>
