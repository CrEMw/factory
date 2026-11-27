// SVG snapshots of the Gantt (by task and by person) and the fever chart on a fixed plan: the component
// renders on the server (svelte/server) and is compared with a file in fixtures/. The .svelte file is compiled on the
// fly by a Node loader hook (svelte/compiler, generate: 'server'), without a Vite build.
// Each snapshot is taken in both locales: en as the readable fixtures/<name>.snap.html, ru as the sha256 of the render
// in fixtures/<name>.ru.sha256 (Russian text lives only in lib/i18n/ru.ts).
// Update the snapshots after an intended change: UPDATE_SNAPSHOTS=1 pnpm run test.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { registerHooks } from 'node:module';
import { fileURLToPath } from 'node:url';
import { compile } from 'svelte/compiler';
import { render } from 'svelte/server';
import { getLocale, setLocale, t, type Locale } from '#lib/i18n/index';
import { dueHalfDay } from '#lib/plan/calendar';
import { depsFromGraph } from '#lib/plan/gantt';
import type { FeverResponse, PlanResponse } from '#lib/plan/types';

registerHooks({
  load(url, context, next) {
    if (!url.endsWith('.svelte')) return next(url, context);
    const filename = fileURLToPath(url);
    const { js } = compile(readFileSync(filename, 'utf8'), { generate: 'server', filename });
    return { format: 'module', source: js.code, shortCircuit: true };
  }
});

const { default: Gantt } = await import('./Gantt.svelte');
const { default: Fever } = await import('./Fever.svelte');

const fixture = (name: string) => JSON.parse(readFileSync(new URL(`./fixtures/${name}`, import.meta.url), 'utf8'));
const p: PlanResponse = fixture('plan.json');
const graph = fixture('graph.json');
const fever: FeverResponse = fixture('fever.json');

function snapshot(name: string, locale: Locale, body: string) {
  // Svelte hydration comments are not part of the snapshot: they change with the compiler version, not the drawing.
  const svg = body.replace(/<!--[^>]*-->/g, '').trim() + '\n';
  const en = locale === 'en';
  const file = new URL(`./fixtures/${name}.${en ? 'snap.html' : `${locale}.sha256`}`, import.meta.url);
  const content = en ? svg : createHash('sha256').update(svg).digest('hex') + '\n';
  if (process.env.UPDATE_SNAPSHOTS === '1') writeFileSync(file, content);
  assert.ok(existsSync(file), `no snapshot ${name} (${locale}): UPDATE_SNAPSHOTS=1 pnpm run test`);
  assert.equal(content, readFileSync(file, 'utf8'), `snapshot ${name} (${locale}) diverged: if intended, UPDATE_SNAPSHOTS=1 pnpm run test`);
  return svg;
}

/** Snapshots the render in every locale and returns the en one for the assertions below. */
function snapshots(name: string, body: () => string): string {
  const saved = getLocale();
  try {
    setLocale('ru');
    snapshot(name, 'ru', body());
    setLocale('en');
    return snapshot(name, 'en', body());
  } finally {
    setLocale(saved);
  }
}

const props = {
  plan: p.plan,
  due: dueHalfDay(p.scope.due_date!),
  deps: depsFromGraph(graph.edges),
  summaries: Object.fromEntries(graph.nodes.map((n: { code: string; summary: string }) => [n.code, n.summary]))
};

test('Gantt snapshot by task', () => {
  const svg = snapshots('gantt', () => render(Gantt, { props: { ...props, label: t('plan.scope.gantt', { root: 'tr00' }) } }).body);
  // The block scrolls itself: at 360px the page is no wider than the screen.
  assert.match(svg, /<div class="frame card[^"]*" role="region" aria-label="Scope Gantt tr00"/);
  assert.match(svg, /<title>Today<\/title>/);
  assert.match(svg, /<title>Scope due date<\/title>/);
  assert.match(svg, /fill="url\(#gantt-hatch\)"/);
  assert.equal((svg.match(/class="arrow[ "]/g) ?? []).length, 3);
  assert.match(svg, /class="arrow svelte-\w+ resource"/);
  assert.match(svg, /<a href="\/t\/ta02" class="task st-doing svelte-\w+ chain">/);
  assert.match(svg, /<g class="buffer zone-yellow svelte-\w+ project">/);
  assert.match(svg, /ta06 waits for tx09: no date/);
});

test('Gantt snapshot by person', () => {
  const svg = snapshots('gantt-people', () =>
    render(Gantt, { props: { ...props, group: 'person', label: t('plan.scope.ganttPeople', { root: 'tr00' }) } }).body
  );
  assert.match(svg, />@alice<\/text>/);
  assert.match(svg, />unassigned<\/text>/);
});

test('fever snapshot', () => {
  const svg = snapshots('fever', () =>
    render(Fever, { props: { points: fever.points, cfg: p.scope, codeOf: (s: number) => (s === 105 ? 'ta05' : undefined) } }).body
  );
  // Scales to the block width: the svg has no width in pixels.
  assert.match(svg, /<svg viewBox="0 0 420 320" role="img"/);
  assert.doesNotMatch(svg, /<svg[^>]* width=/);
  assert.match(svg, />% of chain complete</);
  assert.match(svg, />% of buffer consumed</);
  assert.equal((svg.match(/<circle /g) ?? []).length, 5);
});
