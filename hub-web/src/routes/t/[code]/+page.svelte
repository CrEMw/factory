<script lang="ts">
  import { enhance } from '$app/forms';
  import { documentHref } from '#lib/document-url';
  import { FIB } from '#lib/plan/forms';
  import { wsjfText } from '#lib/plan/flags';
  import { dateFormat, t } from '#lib/i18n/index';

  let { data, form } = $props();

  const when = dateFormat({ dateStyle: 'medium', timeStyle: 'short' });
  const ticket = $derived(data.ticket);

  // Graph links: "Waits for" are this ticket's blockers, "Unblocks" what it blocks,
  // "Part of" the parent and "Subtasks" by part_of, "Related" relates_to and duplicates.
  type Link = NonNullable<typeof data.links>['links'][number];
  const links = $derived(data.links?.links ?? []);
  const pick = (rel: string, dir: 'in' | 'out') => links.filter((l: Link) => l.rel === rel && l.dir === dir);
  const groups = $derived([
    { title: t('tickets.links.group.waits'), items: pick('blocks', 'in') },
    { title: t('tickets.links.group.unblocks'), items: pick('blocks', 'out') },
    { title: t('tickets.links.group.partOf'), items: pick('part_of', 'out') },
    { title: t('tickets.links.group.subtasks'), items: pick('part_of', 'in') },
    { title: t('tickets.links.group.related'), items: [...pick('relates_to', 'out'), ...pick('relates_to', 'in'), ...pick('duplicates', 'out'), ...pick('duplicates', 'in')] }
  ]);
  // Current estimate and WSJF prefill the forms.
  const est = $derived(data.planFields?.estimate ?? null);
  const wsjfCur = $derived(data.planFields?.wsjf ?? null);
  type WsjfKey = 'business_value' | 'time_criticality' | 'risk_opportunity';
  const keep = () => ({ update }: { update: (o?: { reset?: boolean }) => Promise<void> }) => update({ reset: false });
</script>

<svelte:head><title>{ticket.code}: {ticket.summary} · {data.hubName}</title></svelte:head>

<main>
  <p class="crumbs"><a href="/?project={encodeURIComponent(ticket.project)}">{ticket.project}</a> / <code>{ticket.code}</code></p>
  <h1>{ticket.summary}</h1>
  <p class="meta">
    <span class="pill {ticket.status}">{ticket.status}</span>
    {#if ticket.priority}<span class="prio {ticket.priority}">{ticket.priority}</span>{/if}
    <span class="muted">{t('tickets.ticket.meta', { seq: ticket.seq, created: when.format(new Date(ticket.created_at)), updated: when.format(new Date(ticket.updated_at)) })}</span>
  </p>
  {#if ticket.tags.length}<p class="tag">{ticket.tags.join(' · ')}</p>{/if}
  {#if ticket.assignee || ticket.components?.length}
    <p class="meta small">
      {#if ticket.assignee}<a href="/?assignee={encodeURIComponent(ticket.assignee)}&view=list">@{ticket.assignee}</a>{/if}
      {#each ticket.components ?? [] as c (c)}<a class="tag" href="/?component={encodeURIComponent(c)}&view=list">{c}</a>{/each}
    </p>
  {/if}

  <div class="layout">
    <article class="card prose">
      {#if ticket.body}{@html data.html}{:else}<p class="muted">{t('tickets.ticket.noBody')}</p>{/if}
    </article>

    <aside>
      <form class="card" method="POST" action="?/update" use:enhance={() => ({ update }) => update({ reset: false })}>
        <label
          >{t('tickets.field.status')} <select name="status" value={ticket.status}>
            <option>open</option><option>doing</option><option>done</option><option>cancelled</option>
          </select></label
        >
        <label
          >{t('tickets.field.priority')} <select name="priority" value={ticket.priority ?? ''}>
            <option value="">—</option><option>p0</option><option>p1</option><option>p2</option><option>p3</option>
          </select></label
        >
        <label
          >{t('tickets.field.project')} <input
            name="project"
            required
            list="projects"
            pattern="[a-z0-9][a-z0-9\-]{'{'}0,39{'}'}"
            value={ticket.project}
          /></label
        >
        <datalist id="projects">
          {#each data.projects as p (p.project)}<option value={p.project}></option>{/each}
        </datalist>
        <button>{t('tickets.common.save')}</button>
        {#if form?.message}<p role="alert">{form.message}</p>{:else if form?.saved}<p class="muted" role="status">{t('tickets.common.saved')}</p>{/if}
      </form>

      <section class="card fields">
        <h2>{t('tickets.fields.title')}</h2>
        {#if data.fieldsFailed}
          <p class="muted small" role="alert">{t('tickets.fields.failed')}</p>
        {:else if data.fields === null}
          <p class="muted small">{t('tickets.fields.missing')}</p>
        {:else}
          <form method="POST" action="?/fields" use:enhance={keep}>
            <label
              >{t('tickets.field.assignee')} <select name="assignee" value={ticket.assignee ?? ''}>
                <option value="">—</option>
                {#each data.fields.people as p (p.handle)}<option value={p.handle}>{p.name} ({p.handle})</option>{/each}
              </select></label
            >
            <fieldset>
              <legend>{t('tickets.fields.components')}</legend>
              <div class="checks">
                {#each data.fields.components as c (c.slug)}
                  <label class="check" title={c.description ?? c.title}
                    ><input type="checkbox" name="component" value={c.slug} checked={ticket.components?.includes(c.slug)} />
                    {c.title}{#if c.project}<span class="muted"> · {c.project}</span>{/if}</label
                  >
                {:else}
                  <p class="muted small">{t('tickets.components.none')}</p>
                {/each}
              </div>
            </fieldset>
            <button>{t('tickets.common.save')}</button>
            {#if form?.fieldsMessage}<p role="alert">{form.fieldsMessage}</p>{:else if form?.fieldsSaved}<p class="muted" role="status">{t('tickets.common.saved')}</p>{/if}
          </form>
        {/if}
      </section>

      <section class="card plan">
        <h2>{t('tickets.estimate.title')}</h2>
        <form method="POST" action="?/estimate" use:enhance={keep}>
          <label
            >{t('tickets.estimate.points')} <select name="effort_sp" required>
              <option value="">—</option>
              {#each FIB as f (f)}<option value={f} selected={est?.effort_sp === f}>{f}</option>{/each}
            </select></label
          >
          <div class="pair">
            <label title={t('tickets.estimate.aggressiveHint')}>{t('tickets.estimate.aggressive')} <input name="aggressive_hd" value={est?.aggressive_hd ?? ''} type="number" min="1" max="1000" step="1" inputmode="numeric" /></label>
            <label title={t('tickets.estimate.safeHint')}>{t('tickets.estimate.safe')} <input name="safe_hd" value={est?.safe_hd ?? ''} type="number" min="1" max="2000" step="1" inputmode="numeric" /></label>
          </div>
          <div class="pair">
            <label title={t('tickets.estimate.remainingHint')}>{t('tickets.estimate.remaining')} <input name="remaining_hd" value={est?.remaining_hd ?? ''} type="number" min="0" max="1000" step="1" inputmode="numeric" /></label>
            <label>{t('tickets.estimate.notBefore')} <input name="start_not_before" value={est?.start_not_before ?? ''} type="date" min="2000-01-01" max="2100-12-31" /></label>
          </div>
          <p class="muted small">{t('tickets.estimate.wholeVersion')}</p>
          <button>{t('tickets.estimate.submit')}</button>
          {#if form?.estimateMessage}<p role="alert">{form.estimateMessage}</p>{:else if form?.estimateSaved}<p class="muted" role="status">{t('tickets.estimate.saved')}</p>{/if}
        </form>
        <h2>WSJF</h2>
        <form method="POST" action="?/wsjf" use:enhance={keep}>
          {#each [['business_value', t('tickets.wsjf.businessValue')], ['time_criticality', t('tickets.wsjf.timeCriticality')], ['risk_opportunity', t('tickets.wsjf.riskOpportunity')]] as [name, title] (name)}
            <label
              >{title} <select {name} required>
                <option value="">—</option>
                {#each FIB as f (f)}<option value={f} selected={wsjfCur?.[name as WsjfKey] === f}>{f}</option>{/each}
              </select></label
            >
          {/each}
          <p class="muted small">{t('tickets.wsjf.hint')}</p>
          <button>{t('tickets.wsjf.save')}</button>
          {#if form?.wsjfMessage}<p role="alert">{form.wsjfMessage}</p>{:else if form?.wsjfSaved}<p class="muted" role="status">{t('tickets.wsjf.saved', { value: wsjfText(form.wsjfValue) })}</p>{/if}
        </form>
        {#if !data.signedIn}<p class="muted small">{t('tickets.wsjf.needSession')}</p>{/if}
      </section>

      <section class="card links">
        <h2>{t('tickets.links.title')}</h2>
        {#if data.linksFailed}
          <p class="muted small" role="alert">{t('tickets.links.failed')}</p>
        {:else if data.links === null}
          <p class="muted small">{t('tickets.links.missing')}</p>
        {:else}
          {#each groups as g (g.title)}
            {#if g.items.length}
              <h3>{g.title}</h3>
              <ul>
                {#each g.items as l (l.id)}
                  <li>
                    {#if l.kind === 'ticket'}
                      <a href="/t/{encodeURIComponent(l.key)}"><code>{l.key}</code> {l.summary}</a>
                      {#if l.status}<span class="pill {l.status}">{l.status}</span>{/if}
                    {:else}
                      <span><span class="tag">{l.kind}</span> {l.key}</span>
                    {/if}
                    {#if l.rel === 'duplicates'}<span class="tag">{l.dir === 'out' ? t('tickets.links.duplicate') : t('tickets.links.hasDuplicate')}</span>{/if}
                    {#if l.state === 'proposed'}<span class="tag">{t('tickets.links.proposed')}</span>{/if}
                    <span class="actions">
                      {#if l.state === 'proposed'}
                        <form method="POST" action="?/confirm" use:enhance={keep}>
                          <input type="hidden" name="id" value={l.id} /><button class="small">{t('tickets.links.confirm')}</button>
                        </form>
                      {/if}
                      <form method="POST" action="?/close" use:enhance={keep}>
                        <input type="hidden" name="id" value={l.id} />
                        <input type="hidden" name="reason" value={l.state === 'proposed' ? 'rejected' : 'removed'} />
                        <button class="small">{l.state === 'proposed' ? t('tickets.links.reject') : t('tickets.links.remove')}</button>
                      </form>
                    </span>
                  </li>
                {/each}
              </ul>
            {/if}
          {/each}
          {#if !links.length}<p class="muted small">{t('tickets.links.none')}</p>{/if}
          <form class="link" method="POST" action="?/link" use:enhance={keep}>
            <select name="kind" aria-label={t('tickets.links.kind')}>
              <option value="blocks:in">{t('tickets.links.kind.waits')}</option>
              <option value="blocks:out">{t('tickets.links.kind.blocks')}</option>
              <option value="part_of:out">{t('tickets.links.kind.partOf')}</option>
              <option value="part_of:in">{t('tickets.links.kind.subtask')}</option>
              <option value="relates_to:out">{t('tickets.links.kind.relates')}</option>
              <option value="duplicates:out">{t('tickets.links.duplicate')}</option>
            </select>
            <input name="other" required placeholder={t('tickets.links.codePlaceholder')} pattern="[0-9A-Za-z]{'{'}1,20{'}'}" aria-label={t('tickets.links.code')} />
            <button>{t('tickets.links.submit')}</button>
          </form>
          {#if !data.signedIn}<p class="muted small">{t('tickets.links.needSession')}</p>{/if}
          {#if form?.linkMessage}<p role="alert">{form.linkMessage}</p>{/if}
        {/if}
      </section>

      {#if ticket.documents.length}
        <section class="card docs">
          <h2>{t('tickets.ticket.documents')}</h2>
          <ul>
            {#each ticket.documents as d (d.repo + d.path + d.rel)}
              <li>
                <span class="tag">{d.role}</span>
                <a href={documentHref(d.repo, d.path)}>{d.title}</a>
                <!-- rel from git: a `Ticket:` line describes the ticket, a `Closes:` line closes it. -->
                <span class="tag">{d.rel === 'closes' ? t('tickets.ticket.docCloses') : t('tickets.ticket.docDescribes')}</span>
              </li>
            {/each}
          </ul>
        </section>
      {/if}
      {#if ticket.transcript_ref}<p class="muted small">{t('tickets.ticket.call')} <code>{ticket.transcript_ref}</code></p>{/if}
    </aside>
  </div>
</main>

<style>
  .crumbs {
    margin: 12px 0 4px;
    color: var(--muted);
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
    margin: 8px 0 4px;
  }
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 280px;
    gap: 16px;
    align-items: start;
    margin-top: 16px;
  }
  article {
    padding: 20px 24px;
  }
  aside {
    display: grid;
    gap: 16px;
  }
  form {
    display: grid;
    gap: 12px;
    padding: 16px;
  }
  .docs {
    padding: 12px 16px;
  }
  .docs h2 {
    margin: 0 0 8px;
    font-size: 15px;
  }
  .docs ul {
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 8px;
  }
  .docs li {
    display: grid;
  }
  .small {
    font-size: 13px;
    overflow-wrap: anywhere;
  }
  .links {
    padding: 12px 16px;
    display: grid;
    gap: 6px;
  }
  .plan {
    padding: 12px 16px;
    display: grid;
    gap: 6px;
  }
  .plan h2 {
    margin: 0;
    font-size: 15px;
  }
  .plan form {
    padding: 0 0 8px;
    gap: 8px;
  }
  .plan .pair {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }
  .plan p {
    margin: 0;
  }
  .fields {
    padding: 12px 16px;
    display: grid;
    gap: 6px;
  }
  .fields h2 {
    margin: 0;
    font-size: 15px;
  }
  .fields form {
    padding: 0;
  }
  fieldset {
    margin: 0;
    padding: 0;
    border: 0;
    min-width: 0;
  }
  legend {
    padding: 0;
    margin-bottom: 4px;
  }
  .checks {
    display: grid;
    gap: 2px;
    max-height: 240px;
    overflow-y: auto;
    font-size: 13px;
  }
  .check {
    display: flex;
    gap: 6px;
    align-items: baseline;
    overflow-wrap: anywhere;
  }
  .links h2 {
    margin: 0;
    font-size: 15px;
  }
  .links h3 {
    margin: 6px 0 0;
    font-size: 13px;
    color: var(--muted);
  }
  .links ul {
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 6px;
  }
  .links li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    overflow-wrap: anywhere;
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
  form.link {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 0;
  }
  form.link input {
    width: 7em;
  }
  @media (max-width: 800px) {
    .layout {
      grid-template-columns: 1fr;
    }
  }
</style>
