// The yard status read-model client — "what is the yard doing, and
// why?" from one server-computed payload (the-cluster-is-the-system.md
// Phase 0). The backend (`GET /api/yard/status`) does the aggregation:
// it reads the pr-train Jobs, the dock, the LIVE cadence rows and the
// delivery policy, and surfaces the block reason a stuck train used to
// bury in step metadata. This module deserializes that payload ONCE and
// exposes pure derive helpers for rendering.
//
// Contrast with `yard.ts` (the departure board): that page composes
// several endpoints client-side and watches trains move. This one asks
// the narrower operational question — where is each train, and if it is
// stuck, why — and gets the answer, block reason and boarding predicate
// included, computed server-side from the system of record.

import { fetchRemote, type Remote } from '../../data/remote';

// ---------------------------------------------------------------------
// Wire types — the shape of GET /api/yard/status. Parsed once, below.
// ---------------------------------------------------------------------

export type TrainPhase =
  | 'boarding'
  | 'awaiting-ci'
  | 'awaiting-merge'
  | 'deploying'
  | 'converging'
  | 'arrived';

/** Why a train is not moving — the fact the conductor wrote down
 *  somewhere an operator had no reason to look. A discriminated union
 *  on `kind`, mirroring the Rust `TrainBlock`. */
export type TrainBlock =
  | { readonly kind: 'deploy-blocked'; readonly reason: string; readonly since: string | null }
  | { readonly kind: 'ci-red'; readonly checks: string | null }
  | { readonly kind: 'converge-overdue' }
  | { readonly kind: 'stalled'; readonly since: string };

export type TrainStatus = Readonly<{
  id: string;
  title: string;
  phase: TrainPhase;
  at_step: string | null;
  block: TrainBlock | null;
  ci_result: string | null;
  pr_url: string | null;
  car_count: number;
}>;

export type DockCar = Readonly<{
  id: string;
  title: string;
  branch: string | null;
  parked_since: string;
}>;

export type BoardingPredicate = Readonly<{
  dock_threshold: number | null;
  cooldown_minutes: number | null;
  at_times: readonly string[];
  dock_depth: number;
  threshold_met: boolean | null;
  summary: string;
}>;

export type RecentTrain = Readonly<{
  id: string;
  title: string;
  outcome: string;
  journey_seconds: number | null;
}>;

export type StrandedGreen = Readonly<{ branch: string }>;

/** One gate currently being assessed — an open gate-run with no verdict
 *  yet. The Approach draws these into its parallel gate SLOTS. */
export type ActiveGate = Readonly<{
  branch: string;
  packet_id: string;
  since: string;
}>;

/** The gate slots the Approach renders: `capacity` (from the delivery
 *  policy — never a constant baked into the page) and the runs occupying
 *  them right now. */
export type Gates = Readonly<{
  capacity: number;
  active: readonly ActiveGate[];
}>;

/** A car whose most-recent gate-run is red — waiting for rework. */
export type GaragedCar = Readonly<{
  branch: string;
  failed_check: string | null;
  since: string;
}>;

export type PolicyThresholds = Readonly<{
  stall_hours: number | null;
  max_red_trains: number | null;
}>;

export type YardStatus = Readonly<{
  trains: readonly TrainStatus[];
  dock: readonly DockCar[];
  boarding: BoardingPredicate;
  recent: readonly RecentTrain[];
  stranded: readonly StrandedGreen[];
  gates: Gates;
  garage: readonly GaragedCar[];
  policy: PolicyThresholds;
  now: string;
}>;

// ---------------------------------------------------------------------
// Parse — once, at the fetch site (house style). Throws on a bad shape
// so an outage renders `failed`, never a false-empty yard.
// ---------------------------------------------------------------------

function asObject(raw: unknown, where: string): Record<string, unknown> {
  if (typeof raw !== 'object' || raw === null) {
    throw new Error(`${where}: expected an object`);
  }
  return raw as Record<string, unknown>;
}

/** Like `asObject`, but a missing/null section defaults to `{}` rather
 *  than throwing — the top-level payload must be an object (an outage is
 *  a failure), but an absent optional SECTION degrades to its empty
 *  shape. */
function asObjectOrEmpty(raw: unknown): Record<string, unknown> {
  return typeof raw === 'object' && raw !== null ? (raw as Record<string, unknown>) : {};
}

function parseBlock(raw: unknown): TrainBlock | null {
  if (raw === null || raw === undefined) return null;
  const o = asObject(raw, 'block');
  switch (o.kind) {
    case 'deploy-blocked':
      return {
        kind: 'deploy-blocked',
        reason: String(o.reason ?? ''),
        since: typeof o.since === 'string' ? o.since : null,
      };
    case 'ci-red':
      return { kind: 'ci-red', checks: typeof o.checks === 'string' ? o.checks : null };
    case 'converge-overdue':
      return { kind: 'converge-overdue' };
    case 'stalled':
      return { kind: 'stalled', since: String(o.since ?? '') };
    default:
      // An unknown block kind is not a reason to fail the whole page —
      // a newer conductor may write one this build doesn't model — so it
      // renders as "no known block" rather than throwing. The phase
      // still tells the operator where the train is.
      return null;
  }
}

function parseTrain(raw: unknown): TrainStatus {
  const o = asObject(raw, 'train');
  return {
    id: String(o.id ?? ''),
    title: String(o.title ?? ''),
    phase: o.phase as TrainPhase,
    at_step: typeof o.at_step === 'string' ? o.at_step : null,
    block: parseBlock(o.block),
    ci_result: typeof o.ci_result === 'string' ? o.ci_result : null,
    pr_url: typeof o.pr_url === 'string' ? o.pr_url : null,
    car_count: Number(o.car_count ?? 0),
  };
}

function parseDockCar(raw: unknown): DockCar {
  const o = asObject(raw, 'dock car');
  return {
    id: String(o.id ?? ''),
    title: String(o.title ?? ''),
    branch: typeof o.branch === 'string' ? o.branch : null,
    parked_since: String(o.parked_since ?? ''),
  };
}

function parseBoarding(raw: unknown): BoardingPredicate {
  const o = asObjectOrEmpty(raw);
  return {
    dock_threshold: typeof o.dock_threshold === 'number' ? o.dock_threshold : null,
    cooldown_minutes: typeof o.cooldown_minutes === 'number' ? o.cooldown_minutes : null,
    at_times: Array.isArray(o.at_times) ? o.at_times.map(String) : [],
    dock_depth: Number(o.dock_depth ?? 0),
    threshold_met: typeof o.threshold_met === 'boolean' ? o.threshold_met : null,
    summary: String(o.summary ?? ''),
  };
}

function parseRecent(raw: unknown): RecentTrain {
  const o = asObject(raw, 'recent train');
  return {
    id: String(o.id ?? ''),
    title: String(o.title ?? ''),
    outcome: String(o.outcome ?? 'unknown'),
    journey_seconds: typeof o.journey_seconds === 'number' ? o.journey_seconds : null,
  };
}

function parsePolicy(raw: unknown): PolicyThresholds {
  const o = asObjectOrEmpty(raw);
  return {
    stall_hours: typeof o.stall_hours === 'number' ? o.stall_hours : null,
    max_red_trains: typeof o.max_red_trains === 'number' ? o.max_red_trains : null,
  };
}

function parseActiveGate(raw: unknown): ActiveGate {
  const o = asObject(raw, 'active gate');
  return {
    branch: String(o.branch ?? ''),
    packet_id: String(o.packet_id ?? ''),
    since: String(o.since ?? ''),
  };
}

/** The gate section. Capacity comes from the policy server-side; an
 *  absent section degrades to zero capacity + no active gates rather
 *  than throwing — a build talking to an older backend still renders. */
function parseGates(raw: unknown): Gates {
  const o = asObjectOrEmpty(raw);
  return {
    capacity: typeof o.capacity === 'number' ? o.capacity : 0,
    active: Array.isArray(o.active) ? o.active.map(parseActiveGate) : [],
  };
}

function parseGaragedCar(raw: unknown): GaragedCar {
  const o = asObject(raw, 'garaged car');
  return {
    branch: String(o.branch ?? ''),
    failed_check: typeof o.failed_check === 'string' ? o.failed_check : null,
    since: String(o.since ?? ''),
  };
}

export function parseYardStatus(raw: unknown): YardStatus {
  const o = asObject(raw, 'yard status');
  return {
    trains: Array.isArray(o.trains) ? o.trains.map(parseTrain) : [],
    dock: Array.isArray(o.dock) ? o.dock.map(parseDockCar) : [],
    boarding: parseBoarding(o.boarding),
    recent: Array.isArray(o.recent) ? o.recent.map(parseRecent) : [],
    stranded: Array.isArray(o.stranded)
      ? o.stranded.map((s) => ({ branch: String(asObjectOrEmpty(s).branch ?? '') }))
      : [],
    gates: parseGates(o.gates),
    garage: Array.isArray(o.garage) ? o.garage.map(parseGaragedCar) : [],
    policy: parsePolicy(o.policy),
    now: String(o.now ?? ''),
  };
}

export async function fetchYardStatus(): Promise<Remote<YardStatus>> {
  return fetchRemote('/api/yard/status', parseYardStatus);
}

// ---------------------------------------------------------------------
// Pure derive helpers — testable without a DOM.
// ---------------------------------------------------------------------

/** The phase, spelled for a human. Mirrors the Rust `TrainPhase::label`
 *  so the two ends read the same. */
export function phaseLabel(phase: TrainPhase): string {
  switch (phase) {
    case 'boarding':
      return 'boarding';
    case 'awaiting-ci':
      return 'awaiting CI';
    case 'awaiting-merge':
      return 'awaiting merge';
    case 'deploying':
      return 'deploying';
    case 'converging':
      return 'awaiting cluster convergence';
    case 'arrived':
      return 'arrived';
  }
}

/** The one-line explanation of a block, for the operator — the thing
 *  the incident wanted surfaced. `null` when the train is not blocked. */
export function blockLabel(block: TrainBlock | null): string | null {
  if (!block) return null;
  switch (block.kind) {
    case 'deploy-blocked':
      return `DEPLOY BLOCKED — ${block.reason}`;
    case 'ci-red':
      return block.checks ? `CI RED — ${block.checks}` : 'CI RED';
    case 'converge-overdue':
      return 'CONVERGE OVERDUE — the cluster has not converged past threshold';
    case 'stalled':
      return 'STALLED — no step completed inside the policy window';
  }
}

/** A blocked train reads as an error; everything in transit is boring by
 *  construction (green). Used to pick the row tone. */
export function trainTone(t: TrainStatus): 'err' | 'active' | 'muted' {
  if (t.block) return 'err';
  if (t.phase === 'arrived') return 'muted';
  return 'active';
}

/** Journey time as a short human string — minutes under an hour, else
 *  hours to one decimal. `null` when the record carried no timing. */
export function journeyText(seconds: number | null): string {
  if (seconds === null) return '—';
  if (seconds < 3600) return `${Math.round(seconds / 60)}m`;
  return `${Math.round((seconds / 3600) * 10) / 10}h`;
}

/** One gate slot: either the car being assessed in it, or empty. */
export type GateSlot =
  | { readonly kind: 'occupied'; readonly gate: ActiveGate }
  | { readonly kind: 'empty' };

/** The `capacity` slots, filled left-to-right by the active gates (the
 *  server already sorts them deterministically by `since` then branch),
 *  the rest empty. Over-admission is possible against a race — the count
 *  is best-effort — so more active gates than capacity widen the array to
 *  hold them all rather than dropping a running gate off the visual: a
 *  slot the operator can't see is worse than one more than the bound. */
export function gateSlots(gates: Gates): readonly GateSlot[] {
  const n = Math.max(gates.capacity, gates.active.length, 0);
  return Array.from({ length: n }, (_, i) => {
    const gate = gates.active[i];
    return gate ? ({ kind: 'occupied', gate } as const) : ({ kind: 'empty' } as const);
  });
}
