// Fever zones as in services/hub/src/ccpm/fever.rs: the same table of boundary points
// (services/hub/src/ccpm/fixtures/zones.table.json), chart layout on a fixture.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { setLocale } from '#lib/i18n/index';
import { feverLayout, greenLine, M, redLine, zoneOf } from '#lib/plan/fever';
import type { FeverResponse } from '#lib/plan/types';

const DEFAULTS = { green_a_pct: 10, red_a_pct: 30, zone_slope_pct: 60 };

setLocale('en');

test('zones: boundary points from the hub golden table', () => {
  const table = JSON.parse(readFileSync(new URL('../../../../services/hub/src/ccpm/fixtures/zones.table.json', import.meta.url), 'utf8')) as {
    rows: [number, number, string][];
  };
  assert.ok(table.rows.length >= 10);
  for (const [x, c, z] of table.rows) assert.equal(zoneOf(DEFAULTS, x, c), z, `x=${x} consumed=${c}`);
});

test('zones: negative consumption is green, zone lines with slope', () => {
  assert.equal(zoneOf(DEFAULTS, 0, -500), 'green');
  assert.equal(greenLine(DEFAULTS, 50), 40);
  assert.equal(redLine(DEFAULTS, 100), 90);
  assert.equal(zoneOf({ green_a_pct: 0, red_a_pct: 0, zone_slope_pct: 0 }, 100, 1), 'red');
});

test('fever layout: axes, zone bands, series by baseline buffer', () => {
  const f: FeverResponse = JSON.parse(readFileSync(new URL('./fixtures/fever.json', import.meta.url), 'utf8'));
  const l = feverLayout(f.points, DEFAULTS, (seq) => (seq === 105 ? 'ta05' : undefined));
  assert.equal(l.yMin, -20);
  assert.equal(l.yMax, 100);
  assert.deepEqual(l.plot, { x0: M.l, x1: l.width - M.r, y0: l.height - M.b, y1: M.t });
  assert.deepEqual(
    l.bands.map((b) => b.zone),
    ['green', 'yellow', 'red']
  );
  assert.deepEqual(
    l.xTicks.map((t) => t.label),
    ['0%', '25%', '50%', '75%', '100%']
  );
  assert.equal(l.yTicks[0].label, '-20%');
  assert.equal(l.yTicks.at(-1)!.label, '100%');
  assert.equal(l.yTicks[0].y, l.plot.y0);
  assert.equal(l.yTicks.at(-1)!.y, l.plot.y1);
  assert.deepEqual(
    l.series.map((s) => [s.label, s.project, s.points.length]),
    [
      ['FB before ta05, baseline 3', false, 2],
      ['PB, project buffer, baseline 3', true, 3]
    ]
  );
  const pb = l.series[1];
  assert.deepEqual(
    pb.points.map((p) => p.zone),
    ['green', 'green', 'yellow']
  );
  assert.match(pb.points[2].title, /2030-01-14: chain 20%, buffer 33%, yellow zone/);
  assert.ok(pb.d.startsWith(`M${l.plot.x0},`));
  // No points: empty series, y axis from 0 to 100 (or to the red line at 100%).
  const empty = feverLayout([], DEFAULTS);
  assert.deepEqual([empty.yMin, empty.yMax, empty.series.length], [0, 100, 0]);
});
