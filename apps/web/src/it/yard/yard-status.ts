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
  /** Why the dock is not boarding RIGHT NOW — `track occupied (…)`,
   *  `cooldown — M min left`, `below threshold (…)` — or null when it
   *  boards on the conductor's next tick. Derived server-side from the
   *  conductor's own facts (the board rule's last firing, the open
   *  trains, the dock). Null on an older server, with `next_board` null
   *  beside it; the page then states the rule and never a hold. */
  held_because: string | null;
  cooldown_remaining_minutes: number | null;
  /** When the board rule last fired (RFC3339), released or not. */
  last_board_at: string | null;
  /** "boards on the next tick once …" — never a time of day; the depth
   *  rule has no clock. Null on a server that does not send it. */
  next_board: string | null;
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
  /** The server's own reading that this run has outlived the runner's
   *  usual duration — a Job that died without reporting looks exactly
   *  like a slow one from here, and the bay must say so. Absent on an
   *  older server → false: no fabricated alarm. */
  stale: boolean;
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

/** The conductor's liveness, read from its own firing record (the Rust
 *  `ConductorHealth`). `silent` is THE field the board defers to: while
 *  it is true, every section the conductor writes is last-known-good,
 *  not current. Every unknown stays null — no firing or no clock is
 *  "cannot tell", never dressed up as health or alarm. */
export type ConductorHealth = Readonly<{
  last_seen: string | null;
  silent_for_minutes: number | null;
  expected_every_minutes: number | null;
  silent: boolean;
  /** The verb of the heartbeat rule (`reconcile`), read from its
   *  registry row. It used to carry the rule's NAME (`train-reconcile`)
   *  under a "last rule" label; the label now matches the fact. */
  last_verb: string | null;
  last_rc: number | null;
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
  /** `null` on a server that predates the reading — rendered as "no
   *  reading", never as a healthy conductor. */
  conductor: ConductorHealth | null;
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
    // The hold: absent → null in every field. Never default a hold that
    // is not there, and never a "boards on the next tick" the server did
    // not say.
    held_because: typeof o.held_because === 'string' ? o.held_because : null,
    cooldown_remaining_minutes:
      typeof o.cooldown_remaining_minutes === 'number' ? o.cooldown_remaining_minutes : null,
    last_board_at: typeof o.last_board_at === 'string' ? o.last_board_at : null,
    next_board: typeof o.next_board === 'string' ? o.next_board : null,
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
    stale: o.stale === true,
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

/** The conductor block. Absent on an older server → `null` (the page
 *  says "no reading"); present with unknowns → nulls, never defaults. */
function parseConductor(raw: unknown): ConductorHealth | null {
  if (typeof raw !== 'object' || raw === null) return null;
  const o = raw as Record<string, unknown>;
  return {
    last_seen: typeof o.last_seen === 'string' ? o.last_seen : null,
    silent_for_minutes: typeof o.silent_for_minutes === 'number' ? o.silent_for_minutes : null,
    expected_every_minutes:
      typeof o.expected_every_minutes === 'number' ? o.expected_every_minutes : null,
    silent: o.silent === true,
    last_verb: typeof o.last_verb === 'string' ? o.last_verb : null,
    last_rc: typeof o.last_rc === 'number' ? o.last_rc : null,
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
    conductor: parseConductor(o.conductor),
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

// ---------------------------------------------------------------------
// The CONDUCTOR block's readings. Each is a tone + one line, pure, so
// "what does a silent conductor look like" is a test, not a screenshot.
// ---------------------------------------------------------------------

export type Reading = Readonly<{ tone: 'ok' | 'warn' | 'err' | 'muted'; text: string }>;

/** Liveness, against the conductor's OWN declared heartbeat. Silence is
 *  an error; no record is unknown (warn — worth a look, not an outage
 *  claim); no block at all is an older server, and says so. */
export function conductorReading(c: ConductorHealth | null): Reading {
  if (!c) {
    return {
      tone: 'muted',
      text: 'no liveness reading — this server does not report the conductor',
    };
  }
  const every =
    c.expected_every_minutes !== null ? ` · expects every ${c.expected_every_minutes}m` : '';
  if (c.silent) {
    const since =
      c.silent_for_minutes !== null
        ? `${c.silent_for_minutes}m since it last fired`
        : 'past its declared heartbeat';
    return { tone: 'err', text: `SILENT — ${since}${every}` };
  }
  if (c.silent_for_minutes !== null) {
    return { tone: 'ok', text: `last seen ${c.silent_for_minutes}m ago${every}` };
  }
  if (c.last_seen !== null) return { tone: 'muted', text: `last seen ${c.last_seen}${every}` };
  return { tone: 'warn', text: 'no firing on record — liveness unknown' };
}

/** The last verb the conductor ran and how that went. A conductor that
 *  is running but FAILING every pass looks identical to a healthy one
 *  unless the exit code is on the surface. */
export function lastVerbReading(c: ConductorHealth | null): Reading {
  if (!c || c.last_verb === null) return { tone: 'muted', text: 'no verb on record' };
  if (c.last_rc === null) return { tone: 'muted', text: `${c.last_verb} · rc unknown` };
  if (c.last_rc === 0) return { tone: 'ok', text: `${c.last_verb} · rc 0` };
  return { tone: 'err', text: `${c.last_verb} · rc ${c.last_rc} — the last pass failed` };
}

/** When the next train boards, as the RULE — depth reached, cooldown
 *  cleared — and never as a time. The board rule is queue-depth
 *  triggered; it has no next-fire clock, and inventing one is exactly
 *  the "the board said fine and it was not" the page exists to stop.
 *  A clock rule beside it is quoted verbatim from the registry row. */
export function boardsWhen(b: BoardingPredicate): string {
  const t = b.dock_threshold;
  if (t === null) return b.summary !== '' ? b.summary : 'no boarding rule configured';
  const cooldown =
    b.cooldown_minutes !== null ? `the cooldown (${b.cooldown_minutes}m) clears` : null;
  const depth = `${b.dock_depth}/${t} parked`;
  const rule = b.threshold_met
    ? `threshold met — ${depth}; boards ${cooldown ? `when ${cooldown}` : "on the conductor's next pass"}`
    : `${depth} — boards when the dock reaches ${t}${cooldown ? ` and ${cooldown}` : ''}`;
  const clock = b.at_times.length > 0 ? ` · or by the clock at ${b.at_times.join(' / ')} UTC` : '';
  return rule + clock;
}

/** An RFC3339 stamp as the clock time it names, `HH:MM UTC` — the
 *  registry's own `at_times` idiom — or null when absent or unparseable. */
export function clockText(stamp: string | null): string | null {
  if (!stamp) return null;
  const ms = Date.parse(stamp);
  if (Number.isNaN(ms)) return null;
  const d = new Date(ms);
  const hh = String(d.getUTCHours()).padStart(2, '0');
  const mm = String(d.getUTCMinutes()).padStart(2, '0');
  return `${hh}:${mm} UTC`;
}

/** The "boards" row: the hold in force as the primary line, the "next"
 *  sentence beneath it, the last board as a clock time. */
export type BoardHoldView = Readonly<{
  /** `held: <why>` in the row's plain colour (`tone: null`) — a hold is
   *  the protocol working, not an alarm; "boards on the next tick" in
   *  ok; a no-depth-rule line muted. */
  primary: Readonly<{ tone: Reading['tone'] | null; text: string }>;
  next: string | null;
  lastBoard: string | null;
}>;

/** Why the dock is not boarding right now, from the server's own hold.
 *  Null when the server sent none (an older build): the page then keeps
 *  stating the rule, and never says "boards on the next tick" on its
 *  own authority. Every line is the server's sentence — the depth rule
 *  has no clock, so nothing here is a time of day. */
export function boardHold(b: BoardingPredicate): BoardHoldView | null {
  const lastBoard = clockText(b.last_board_at);
  if (b.held_because !== null) {
    return {
      primary: { tone: null, text: `held: ${b.held_because}` },
      next: b.next_board,
      lastBoard,
    };
  }
  if (b.next_board !== null) {
    return {
      primary: { tone: b.dock_threshold !== null ? 'ok' : 'muted', text: b.next_board },
      next: null,
      lastBoard,
    };
  }
  return null;
}

/** Elapsed since an RFC3339 stamp, in the `journeyText` idiom. An absent
 *  or unparseable stamp is `null` — no number rather than a fabricated
 *  zero. */
export function elapsedText(since: string | null | undefined, nowMs: number): string | null {
  if (!since) return null;
  const started = Date.parse(since);
  if (Number.isNaN(started)) return null;
  return journeyText(Math.max(nowMs - started, 0) / 1000);
}
