// JSON contracts of the scope plan, fever chart, next task and backlog. Source of truth:
// services/hub/src/plan.rs (routes) and services/hub/src/ccpm/model.rs (Plan and its parts, serde). Plan time:
// working half-day (HalfDay) from Monday 2024-01-01, Mon-Fri calendar (calendar.ts).

export type HalfDay = number;
export type Status = 'open' | 'doing' | 'done' | 'cancelled';
export type Zone = 'red' | 'yellow' | 'green';

/** TaskPlan: interval [start, end) in half-days; an umbrella (flag umbrella) has the span of its children, a and s empty. */
export type TaskPlan = {
  seq: number;
  code: string;
  status: Status;
  assignee: string | null;
  a: number | null;
  a_full: number | null;
  s: number | null;
  start: HalfDay | null;
  end: HalfDay | null;
  on_chain: boolean;
  /** Key of the feeding buffer of a task off the critical chain. */
  buffer: string | null;
  ready: boolean;
  /** WSJF in hundredths. */
  wsjf: number | null;
  flags: string[];
  /** Cycle path of codes (only with in_cycle; omitted from JSON when empty). */
  cycle?: string[];
};

/** Critical chain link in order from the start; resource_link: tied to the previous link by a resource, not a dependency. */
export type ChainLink = { seq: number; resource_link: boolean };

export type BufferPlan = {
  /** pb, fb:<seq of the entry point> or fb:finish. */
  key: string;
  kind: 'project' | 'feeding';
  entry: number | null;
  tasks: number[];
  size: number;
  start: HalfDay;
  end: HalfDay;
  compressed: boolean;
  chain_complete_pct: number | null;
  consumed_pct: number | null;
  zone: Zone | null;
  /** Penetration above the green line, hundredths of a percent. */
  penetration: number | null;
};

export type IgnoredEdge = { src: number; dst: number; reason: string };
/** reason: prev_half_done or starts_soon. */
export type ResourceWarning = { assignee: string; seq: number; after: number; reason: string };

export type Plan = {
  algo_version: number;
  root: number;
  today: HalfDay;
  /** Forecast finish: end of PB. */
  end: HalfDay;
  late_by: number;
  defaulted_pct: number;
  approvable: boolean;
  wsjf_root: number | null;
  flags: string[];
  tasks: TaskPlan[];
  chain: ChainLink[];
  buffers: BufferPlan[];
  ignored_edges: IgnoredEdge[];
  resource_warnings: ResourceWarning[];
};

/** Live plan.scope row (ScopeRow, root_seq is not serialized). */
export type ScopeRow = {
  id: number;
  root: string;
  summary: string;
  due_date: string | null;
  mode: 'forward' | 'backward';
  buffer_method: 'cnp' | 'ssq';
  green_a_pct: number;
  red_a_pct: number;
  zone_slope_pct: number;
  max_defaulted_pct: number;
  valid_from: string;
  actor: string;
  actor_kind: string;
};

/** GET /api/scopes and scopes in /api/backlog (ScopeSummary). */
export type ScopeSummary = ScopeRow & {
  computed_at: string | null;
  plan_end: HalfDay | null;
  wsjf_root: number | null;
  pending: boolean;
  baseline_id: number | null;
};

export type BaselineInfo = {
  id: number;
  approved_at: string;
  approved_by: string;
  approved_kind: string;
  accepted_defaults: boolean;
  defaulted_pct: number;
  max_defaulted_pct: number;
};

/** GET /api/plan/{root}. */
export type PlanResponse = {
  root: string;
  scope: ScopeRow;
  source: 'stored' | 'live';
  computed_at: string | null;
  today: string;
  end_date: string | null;
  pending: boolean;
  baseline: BaselineInfo | null;
  calendar: { unit: 'half_day'; epoch: string; workdays: string };
  plan: Plan;
};

/** Fever point: one per day per buffer. */
export type FeverPoint = {
  day: string;
  buffer_key: string;
  kind: 'project' | 'feeding';
  baseline_id: number;
  chain_complete_pct: number;
  consumed_pct: number;
  zone: Zone | null;
  flags: string[];
};

/** GET /api/plan/{root}/fever. */
export type FeverResponse = { root: string; baseline: BaselineInfo | null; points: FeverPoint[] };

/** Error body of POST /api/plan/{root}/baseline (plan.rs approve): 403, 404 and 409. */
export type ApproveRefusal =
  | { error: 'defaults_over_threshold'; defaulted_pct: number; max_defaulted_pct: number }
  | { error: 'nothing_to_approve' }
  | { error: 'approve_requires_human' }
  | { error: 'no_scope' };

/** GET /api/next?person= (NextOut, WarningOut). */
export type NextWarning = { code: string; after: string; reason: string; scope: string };
export type NextOut = { person: string; code: string | null; scope: string | null; flags: string[]; warnings: NextWarning[] };

/** GET /api/backlog (BacklogItem). */
export type BacklogItem = {
  code: string;
  project: string;
  summary: string;
  status: Status;
  priority: string | null;
  effort_sp: number | null;
  cost_of_delay: number | null;
  wsjf: number | null;
};
export type Backlog = { total: number; items: BacklogItem[]; scopes: ScopeSummary[] };

/** Body of PUT /api/tickets/{code}/estimate (EstimateIn): what is missing is empty in the new version. */
export type EstimateIn = {
  effort_sp: number;
  aggressive_hd?: number;
  safe_hd?: number;
  remaining_hd?: number;
  start_not_before?: string;
};

/** Body of PUT /api/tickets/{code}/wsjf (WsjfBody). */
export type WsjfIn = { business_value: number; time_criticality: number; risk_opportunity: number };

/** GET /api/tickets/{code}/plan: live estimate and WSJF of the ticket, null when absent. */
export interface TicketPlanFields {
  estimate: {
    effort_sp: number;
    aggressive_hd: number | null;
    safe_hd: number | null;
    remaining_hd: number | null;
    start_not_before: string | null;
  } | null;
  wsjf: { business_value: number; time_criticality: number; risk_opportunity: number; wsjf: number | null } | null;
}
