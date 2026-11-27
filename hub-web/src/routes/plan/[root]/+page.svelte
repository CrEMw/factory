<script lang="ts">
  // Scope plan: tabs as links (?tab=), Gantt, fever chart and Gantt by person, buffers, baseline
  // approval. Without a baseline there are no zones or fever points, and the page says so in words.
  import { enhance } from '$app/forms';
  import { dateFormat, t } from '#lib/i18n/index';
  import Fever from '../../../lib/plan/Fever.svelte';
  import Gantt from '../../../lib/plan/Gantt.svelte';
  import { dueHalfDay } from '#lib/plan/calendar';
  import { bufferName, flagName, WARNING_NAMES, wsjfText, ZONE_NAMES } from '#lib/plan/flags';
  import { depsFromGraph } from '#lib/plan/gantt';

  let { data, form } = $props();

  const p = $derived(data.plan);
  const plan = $derived(p.plan);
  const codeOf = (seq: number) => plan.tasks.find((t) => t.seq === seq)?.code;
  const due = $derived(p.scope.due_date ? dueHalfDay(p.scope.due_date) : null);
  const deps = $derived(data.graph ? depsFromGraph(data.graph.edges) : []);
  const summaries = $derived(Object.fromEntries((data.graph?.nodes ?? []).map((n) => [n.code, n.summary])));
  const day = dateFormat({ dateStyle: 'medium', timeZone: 'UTC' });
  const when = dateFormat({ dateStyle: 'medium', timeStyle: 'short' });
  const fmt = (d: string) => day.format(new Date(`${d}T00:00:00Z`));
  const TABS = ['gantt', 'fever', 'people'] as const;
  const keep = () => ({ update }: { update: (o?: { reset?: boolean }) => Promise<void> }) => update({ reset: false });
</script>

<svelte:head><title>{t('plan.scope.title', { root: p.root })} · {data.hubName}</title></svelte:head>

<main>
  <p class="crumbs"><a href="/plan">{t('plan.list.heading')}</a> / <a href="/t/{encodeURIComponent(p.root)}"><code>{p.root}</code></a></p>
  <h1>{p.scope.summary}</h1>
  <p class="meta muted">
    {t('plan.scope.forecast')} <b>{p.end_date ? fmt(p.end_date) : '—'}</b>
    {#if p.scope.due_date}· {t('plan.dueBy', { date: fmt(p.scope.due_date) })}{/if}
    · {t('plan.scope.today', { date: fmt(p.today) })}
    {#if plan.late_by > 0}· <span class="warn">{t('plan.lateBy', { count: plan.late_by })}</span>{/if}
    · {t(p.scope.mode === 'backward' ? 'plan.scope.backward' : 'plan.scope.forward')}, {t('plan.scope.bufferMethod', { method: p.scope.buffer_method === 'ssq' ? 'SSQ' : 'C&P' })}
    {#if plan.wsjf_root != null}· WSJF {wsjfText(plan.wsjf_root)}{/if}
    · {p.source === 'live' ? t('plan.scope.live') : t('plan.scope.computed', { when: p.computed_at ? when.format(new Date(p.computed_at)) : '' })}
    {#if p.pending}· {t('plan.scope.pending')}{/if}
  </p>
  {#if plan.flags.length}
    <p class="flags-line">{#each plan.flags as f (f)}<span class="pill warnpill">{flagName(f)}</span>{/each}</p>
  {/if}

  <nav class="tabs" aria-label={t('plan.scope.tabs')}>
    {#each TABS as key (key)}
      <a href="?tab={key}" aria-current={data.tab === key ? 'page' : undefined}>{t(`plan.tab.${key}`)}</a>
    {/each}
  </nav>

  {#if data.tab === 'fever'}
    {#if data.feverFailed}
      <p role="alert">{t('plan.fever.failed')}</p>
    {:else if !p.baseline}
      <p class="muted">{t('plan.fever.noBaseline')}</p>
    {:else if !data.fever || data.fever.points.length === 0}
      <p class="muted">{t('plan.fever.noPoints')}</p>
    {:else}
      <Fever points={data.fever.points} cfg={p.scope} {codeOf} />
    {/if}
    <p class="muted small">
      {t('plan.fever.zones', { green: p.scope.green_a_pct, red: p.scope.red_a_pct, slope: p.scope.zone_slope_pct })}
    </p>
  {:else}
    {#if data.graphFailed}<p class="muted small" role="alert">{t('plan.scope.graphFailed')}</p>{/if}
    {#if !plan.tasks.some((t) => t.start != null)}
      <p class="muted">{t('plan.scope.noTasks')}</p>
    {:else}
      <p class="legend muted small">
        <span class="key chain">{t('plan.chain')}</span>
        <span class="key hatch">{t('plan.legend.defaulted')}</span>
        <span class="key buf">{t('plan.legend.buffer')}</span>
        <span class="key res">{t('plan.legend.resource')}</span>
        <span class="key ms">{t('plan.legend.milestone')}</span>
        <span class="key today">{t('plan.gantt.today')}</span>
        {#if p.scope.due_date}<span class="key due">{t('plan.legend.due')}</span>{/if}
      </p>
      <Gantt
        {plan}
        {due}
        {deps}
        {summaries}
        group={data.tab === 'people' ? 'person' : 'task'}
        label={t(data.tab === 'people' ? 'plan.scope.ganttPeople' : 'plan.scope.gantt', { root: p.root })}
      />
    {/if}
  {/if}

  <section class="card block">
    <h2>{t('plan.buffers.title')}</h2>
    {#if plan.buffers.length}
      <div class="scroll">
        <table>
          <thead><tr><th>{t('plan.buffers.buffer')}</th><th>{t('plan.buffers.size')}</th><th>{t('plan.buffers.zone')}</th><th>{t('plan.buffers.consumed')}</th><th>{t('plan.buffers.chain')}</th></tr></thead>
          <tbody>
            {#each plan.buffers as b (b.key)}
              <tr>
                <td>{bufferName(b.key, codeOf)}{#if b.compressed}{' '}<span class="tag">{t('plan.buffers.compressed')}</span>{/if}</td>
                <td class="num">{b.size}</td>
                <td><span class="zone {b.zone ?? 'none'}">{b.zone ? ZONE_NAMES[b.zone] : '—'}</span></td>
                <td class="num">{b.consumed_pct == null ? '—' : b.consumed_pct + '%'}</td>
                <td class="num">{b.chain_complete_pct == null ? '—' : b.chain_complete_pct + '%'}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {:else}
      <p class="muted">{t('plan.buffers.none')}</p>
    {/if}
    {#if plan.resource_warnings.length}
      <h3>{t('plan.buffers.resource')}</h3>
      <ul>
        {#each plan.resource_warnings as w, i (i)}
          <li>@{w.assignee}: {t('plan.buffers.prepareFor')} <code>{codeOf(w.seq) ?? w.seq}</code> {t('plan.buffers.after')} <code>{codeOf(w.after) ?? w.after}</code> ({WARNING_NAMES[w.reason] ?? w.reason})</li>
        {/each}
      </ul>
    {/if}
  </section>

  <section class="card block">
    <h2>{t('plan.baseline.title')}</h2>
    {#if p.baseline}
      <p>
        {t('plan.baseline.approved', { when: when.format(new Date(p.baseline.approved_at)), by: p.baseline.approved_by })}{#if p.baseline.accepted_defaults}{t('plan.baseline.withDefaults', { pct: p.baseline.defaulted_pct, max: p.baseline.max_defaulted_pct })}{/if}.
      </p>
    {:else}
      <p class="muted">{t('plan.baseline.none')}</p>
    {/if}
    <p class="muted small">
      {t('plan.baseline.defaults', { pct: plan.defaulted_pct, max: p.scope.max_defaulted_pct })}
      {t('plan.baseline.fixes')}
    </p>
    <form method="POST" action="?/approve&tab={data.tab}" use:enhance={keep}>
      <input type="hidden" name="accept_defaults" value="false" />
      <button>{t(p.baseline ? 'plan.baseline.reapprove' : 'plan.baseline.approve')}</button>
    </form>
    {#if form?.defaults}
      <div class="warnbox" role="alert">
        <p>
          {t('plan.baseline.defaultsWarning', { pct: form.defaults.defaulted_pct, max: form.defaults.max_defaulted_pct })}
        </p>
        <form method="POST" action="?/approve&tab={data.tab}" use:enhance={keep}>
          <input type="hidden" name="accept_defaults" value="true" />
          <button class="danger">{t('plan.baseline.approveDefaults')}</button>
        </form>
      </div>
    {:else if form?.approveMessage}
      <p role="alert">{form.approveMessage}</p>
    {:else if form?.approved}
      <p class="muted" role="status">{t('plan.baseline.done')}</p>
    {/if}
    {#if !data.signedIn}<p class="muted small">{t('plan.baseline.humanOnly')}</p>{/if}
  </section>

  {#if plan.tasks.some((t) => t.start == null && t.status !== 'done' && t.status !== 'cancelled')}
    <section class="card block">
      <h2>{t('plan.scope.unscheduled')}</h2>
      <ul>
        {#each plan.tasks.filter((t) => t.start == null && t.status !== 'done' && t.status !== 'cancelled') as t (t.seq)}
          <li><a href="/t/{encodeURIComponent(t.code)}"><code>{t.code}</code></a>{#if t.flags.length}: {t.flags.map(flagName).join(', ')}{/if}{#if t.cycle?.length}{' '}({t.cycle.join(' → ')}){/if}</li>
        {/each}
      </ul>
    </section>
  {/if}
</main>

<style>
  .crumbs {
    margin: 12px 0 4px;
    color: var(--muted);
  }
  h2 {
    margin: 0 0 8px;
    font-size: 17px;
  }
  h3 {
    margin: 12px 0 4px;
    font-size: 14px;
  }
  .meta {
    margin: 6px 0;
    font-size: 14px;
  }
  .warn {
    color: var(--danger);
  }
  .flags-line {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin: 6px 0;
  }
  .warnpill {
    border-color: var(--danger);
    color: var(--danger);
  }
  .tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 4px;
    margin: 16px 0 12px;
    border-bottom: 1px solid var(--line);
  }
  .tabs a {
    padding: 6px 12px;
    color: var(--muted);
    text-decoration: none;
    border-bottom: 2px solid transparent;
    margin-bottom: -1px;
  }
  .tabs a[aria-current='page'] {
    color: var(--text);
    font-weight: 600;
    border-bottom-color: var(--accent);
  }
  .small {
    font-size: 13px;
  }
  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    margin: 0 0 8px;
  }
  .key::before {
    content: '';
    display: inline-block;
    width: 14px;
    height: 8px;
    margin-right: 4px;
    vertical-align: middle;
    background: var(--accent);
  }
  .key.chain::before {
    outline: 2px solid var(--text);
  }
  .key.hatch::before {
    background: repeating-linear-gradient(45deg, var(--accent) 0 3px, var(--card) 3px 5px);
  }
  .key.buf::before {
    background: var(--zone-green);
  }
  .key.res::before {
    height: 0;
    border-top: 2px dashed var(--text);
    background: none;
  }
  .key.ms::before {
    width: 8px;
    background: none;
    border: 2px solid var(--danger);
    transform: rotate(45deg);
  }
  .key.today::before {
    width: 2px;
    height: 12px;
    background: var(--danger);
  }
  .key.due::before {
    width: 0;
    height: 12px;
    border-left: 2px dashed var(--text);
    background: none;
  }
  .block {
    margin-top: 16px;
    padding: 14px 16px;
  }
  .block ul {
    margin: 4px 0;
    padding-left: 20px;
  }
  .scroll {
    overflow-x: auto;
  }
  td.num {
    font-variant-numeric: tabular-nums;
  }
  .zone.green {
    color: var(--zone-green);
  }
  .zone.yellow {
    color: var(--zone-yellow);
  }
  .zone.red {
    color: var(--zone-red);
  }
  form {
    margin-top: 8px;
  }
  .warnbox {
    margin-top: 12px;
    padding: 10px 12px;
    border: 1px solid var(--danger);
    border-radius: 8px;
  }
  .warnbox p {
    margin: 0;
  }
  button.danger {
    background: var(--danger);
  }
</style>
