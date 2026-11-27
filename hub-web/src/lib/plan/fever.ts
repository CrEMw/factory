// Fever chart without DOM: axes "% of chain complete" (x, 0..100) and "% of buffer consumed" (y), three
// zones by the rule of services/hub/src/ccpm/fever.rs (zone): max(consumed, 0) <= green_a + slope·x/100 is green,
// <= red_a + slope·x/100 yellow, otherwise red. Compared in hundredths of a percent, as in hub.
import { t } from '#lib/i18n/index';
import { bufferName, ZONE_NAMES } from '#lib/plan/flags';
import type { FeverPoint, ScopeRow, Zone } from '#lib/plan/types';

export type ZoneCfg = Pick<ScopeRow, 'green_a_pct' | 'red_a_pct' | 'zone_slope_pct'>;

/** Zone of a point: the same integer comparison as fever.rs zone, byte for byte. */
export function zoneOf(cfg: ZoneCfg, chainComplete: number, consumed: number): Zone {
  const c = Math.max(consumed, 0) * 100;
  const slope = cfg.zone_slope_pct * chainComplete;
  if (c <= cfg.green_a_pct * 100 + slope) return 'green';
  if (c <= cfg.red_a_pct * 100 + slope) return 'yellow';
  return 'red';
}

/** Zone bounds in percent at x% of the chain complete. */
export const greenLine = (cfg: ZoneCfg, x: number) => cfg.green_a_pct + (cfg.zone_slope_pct * x) / 100;
export const redLine = (cfg: ZoneCfg, x: number) => cfg.red_a_pct + (cfg.zone_slope_pct * x) / 100;

export const FEVER_W = 420;
export const FEVER_H = 320;
export const M = { l: 52, r: 22, t: 14, b: 46 };

export type FeverSeries = {
  key: string;
  label: string;
  project: boolean;
  d: string;
  points: { cx: number; cy: number; zone: Zone; title: string }[];
};

export type FeverLayout = {
  width: number;
  height: number;
  plot: { x0: number; x1: number; y0: number; y1: number };
  yMin: number;
  yMax: number;
  bands: { zone: Zone; d: string; title: string }[];
  xTicks: { x: number; label: string }[];
  yTicks: { y: number; label: string }[];
  series: FeverSeries[];
};

const ceil10 = (v: number) => Math.ceil(v / 10) * 10;
const floor10 = (v: number) => Math.floor(v / 10) * 10;
const r = (v: number) => Math.round(v * 10) / 10;

export function feverLayout(points: FeverPoint[], cfg: ZoneCfg, codeOf: (seq: number) => string | undefined = () => undefined): FeverLayout {
  const consumed = points.map((p) => p.consumed_pct);
  const yMax = ceil10(Math.max(100, redLine(cfg, 100), ...consumed));
  const yMin = Math.min(0, floor10(Math.min(0, ...consumed)));
  const x0 = M.l;
  const x1 = FEVER_W - M.r;
  const y0 = FEVER_H - M.b;
  const y1 = M.t;
  const sx = (v: number) => r(x0 + ((x1 - x0) * Math.min(Math.max(v, 0), 100)) / 100);
  const sy = (v: number) => r(y0 - ((y0 - y1) * (Math.min(Math.max(v, yMin), yMax) - yMin)) / (yMax - yMin));
  const poly = (pts: [number, number][]) => 'M' + pts.map(([x, y]) => `${sx(x)},${sy(y)}`).join(' L') + ' Z';
  const g0 = greenLine(cfg, 0);
  const g1 = greenLine(cfg, 100);
  const r0 = redLine(cfg, 0);
  const r1 = redLine(cfg, 100);
  const bands = [
    { zone: 'green' as const, d: poly([[0, yMin], [100, yMin], [100, g1], [0, g0]]), title: t('plan.fever.bandGreen', { green: cfg.green_a_pct, slope: cfg.zone_slope_pct }) },
    { zone: 'yellow' as const, d: poly([[0, g0], [100, g1], [100, r1], [0, r0]]), title: t('plan.fever.bandYellow', { red: cfg.red_a_pct, slope: cfg.zone_slope_pct }) },
    { zone: 'red' as const, d: poly([[0, r0], [100, r1], [100, yMax], [0, yMax]]), title: t('plan.fever.bandRed') }
  ];
  const xTicks = [0, 25, 50, 75, 100].map((v) => ({ x: sx(v), label: `${v}%` }));
  const step = (yMax - yMin) / 10 > 20 ? 50 : (yMax - yMin) / 10 > 10 ? 20 : 10;
  const yTicks: { y: number; label: string }[] = [];
  for (let v = Math.ceil(yMin / step) * step; v <= yMax; v += step) yTicks.push({ y: sy(v), label: `${v}%` });

  const groups = new Map<string, FeverPoint[]>();
  for (const p of [...points].sort((a, b) => a.day.localeCompare(b.day))) {
    const key = `${p.baseline_id}:${p.buffer_key}`;
    groups.set(key, [...(groups.get(key) ?? []), p]);
  }
  const series: FeverSeries[] = [...groups.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([key, ps]) => {
      const label = t('plan.fever.series', { buffer: bufferName(ps[0].buffer_key, codeOf), id: ps[0].baseline_id });
      const pts = ps.map((p) => {
        const zone = p.zone ?? zoneOf(cfg, p.chain_complete_pct, p.consumed_pct);
        return {
          cx: sx(p.chain_complete_pct),
          cy: sy(p.consumed_pct),
          zone,
          title: t('plan.fever.point', { label, day: p.day, chain: p.chain_complete_pct, buffer: p.consumed_pct, zone: ZONE_NAMES[zone] })
        };
      });
      return {
        key,
        label,
        project: ps[0].kind === 'project',
        d: pts.length > 1 ? 'M' + pts.map((p) => `${p.cx},${p.cy}`).join(' L') : '',
        points: pts
      };
    });
  return { width: FEVER_W, height: FEVER_H, plot: { x0, x1, y0, y1 }, yMin, yMax, bands, xTicks, yTicks, series };
}
