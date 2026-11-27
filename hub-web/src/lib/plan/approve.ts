// Hub's answer to baseline approval (POST /api/plan/{root}/baseline, plan.rs approve) in human
// words. 409 defaults_over_threshold asks to retry with accept_defaults=true: the page shows a warning and a
// second button.
import { t } from '#lib/i18n/index';

export type ApproveOutcome =
  | { kind: 'approved' }
  | { kind: 'defaults'; defaulted_pct: number; max_defaulted_pct: number }
  | { kind: 'error'; message: string };

export function approveOutcome(status: number, text: string): ApproveOutcome {
  if (status === 201 || status === 200) return { kind: 'approved' };
  if (status === 404 && !text.trim()) return { kind: 'error', message: t('plan.approve.rollingOut') };
  let body: { error?: string; defaulted_pct?: number; max_defaulted_pct?: number } = {};
  try {
    body = JSON.parse(text);
  } catch {
    body = {};
  }
  switch (body.error) {
    case 'defaults_over_threshold':
      return { kind: 'defaults', defaulted_pct: body.defaulted_pct ?? 0, max_defaulted_pct: body.max_defaulted_pct ?? 0 };
    case 'approve_requires_human':
      return { kind: 'error', message: t('plan.approve.requiresHuman') };
    case 'nothing_to_approve':
      return { kind: 'error', message: t('plan.approve.nothing') };
    case 'no_scope':
      return { kind: 'error', message: t('plan.approve.noScope') };
    default:
      return { kind: 'error', message: `hub: ${status} ${body.error ?? text.slice(0, 200)}` };
  }
}
