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

/** When a train in flight is expected to arrive — or why that cannot
 *  be said. A discriminated union on `kind`, mirroring the Rust
 *  `TrainEta`: a reader gets either a figure or a reason, never a bare
 *  null to interpret.
 *
 *  MEASURED 2026-09-10 over the live record: the two legs are the same
 *  size (board→merge median 1,206s, merge→arrival 1,183s), so a merged
 *  train told the whole-journey figure is told roughly double what it
 *  has left. That is why `leg` is on the wire. */
export type TrainEta =
  | Readonly<{
      kind: 'estimate';
      /** Which leg the figure covers: `boarding → arrival` before the
       *  merge, `merge → arrival` after it. */
      leg: string;
      remaining_seconds: number;
      /** The 10th and 90th percentiles of the same population — the
       *  spread, published because one number would over-claim. */
      remaining_low_seconds: number;
      remaining_high_seconds: number;
      sample_size: number;
      basis: string;
      /** Elapsed has passed the 90th percentile of everything measured. */
      overdue: boolean;
    }>
  | Readonly<{ kind: 'unknown'; reason: string }>;

export type TrainStatus = Readonly<{
  id: string;
  title: string;
  phase: TrainPhase;
  at_step: string | null;
  block: TrainBlock | null;
  ci_result: string | null;
  pr_url: string | null;
  car_count: number;
  /** When the train boarded (RFC3339, the collect step's stamp). Null
   *  on a server that does not send it; the floor then reads the
   *  boarding minute off the title. */
  boarded_at: string | null;
  /** When this train is expected to arrive, or why that cannot be said.
   *  A server that does not send one reads as `unknown` with a reason —
   *  the page never shows a fabricated zero. */
  eta: TrainEta;
}>;

export type DockCar = Readonly<{
  id: string;
  title: string;
  branch: string | null;
  parked_since: string;
}>;

/** A car standing ON the dock that cannot board: an operator wrote a
 *  `hold` on its review step — a car whose branch is correct but whose
 *  world is not (a node cordoned under it, an object to delete first, a
 *  companion car it must land with).
 *
 *  A SIBLING of `HeldGreen`, not a member of its lane. Both answer "what
 *  cannot move" and they are different answers: a held green has no car
 *  and is released by filing one; a held car is on the dock and is
 *  released by clearing the marker. Their fields would also collide —
 *  `HeldGreen.packet_id` is a gate-run and its `since` is when that run
 *  opened, while a held car's packet is the ship-a-change and its stamp
 *  is when the car parked. The Rust `HeldCar` flattens a `DockCar`, so
 *  this is a dock row plus its reason, field for field. */
export type HeldCar = Readonly<{
  id: string;
  title: string;
  branch: string | null;
  parked_since: string;
  /** Why it cannot board. A bare `hold: true` reads "no reason
   *  recorded" — the same words the held-green lane uses. */
  reason: string;
}>;

/** Whether the server could READ one of the boarding block's inputs, as
 *  it states it on the wire. `null` on a server that does not state it —
 *  the page then reads the fields it qualifies exactly as it always did.
 *
 *  It exists because the nulls it qualifies cannot carry the distinction
 *  themselves: `dock_threshold: null` is "no depth rule is configured" on
 *  a registry that was read and "I could not tell you" on one that was
 *  not, and those ask an operator for different things (31783deb).
 *  `dock_depth` can express its own unread state as a null against a
 *  number; a field whose null is ALREADY a legitimate value cannot. */
export type ReadState = 'read' | 'unread';

export type BoardingPredicate = Readonly<{
  dock_threshold: number | null;
  cooldown_minutes: number | null;
  at_times: readonly string[];
  /** Whether the server read the cadence rows. `unread` says the three
   *  fields above are unknowns, not absences — and the server's own
   *  `summary` then says so in words. */
  cadence_reading: ReadState | null;
  /** How many cars are parked right now, or NULL when the server could
   *  not read the dock — the `loading-dock` station row did not serve.
   *  Never coerced to 0: a zero is a count, and a count nobody took is
   *  the shape that passes an absence check falsely (61085a9e measured
   *  exactly that on this endpoint, and efe6ef10 is the server's own
   *  version of it). Everything below that reads a depth must branch on
   *  the null rather than fall through to "below the threshold". */
  dock_depth: number | null;
  /** Whether the threshold is met — null when the question has NO
   *  ANSWER: no depth rule configured, or the depth unread. Not false. */
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
  /** Whether the server read that firing. `unread` says `last_board_at`
   *  and `cooldown_remaining_minutes` are unknowns — not "it has never
   *  boarded" and not "no cooldown is running", which is how the pair
   *  reads on its own and is the answer that lets something proceed. */
  last_board_reading: ReadState | null;
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

/** A green gate-run no car claims and nobody held — gated, then
 *  forgotten. THE authoritative answer to "is this green still owed a
 *  car?": the marker list that decides it (`superseded`, `rerailed_to`,
 *  `park_skipped`) lives once, in boss-jobs' `stranded` module, and the
 *  row arrives complete enough to draw — packet and head included — so
 *  no lens has to re-derive it from a window of gate-runs. Doing that
 *  is what drew a phantom wagon for a re-railed branch all day on
 *  2026-09-10 (CLAUDE.md §9a). */
export type StrandedGreen = Readonly<{
  branch: string;
  /** The gate-run packet behind the row. '' on a server that predates
   *  the field — a row that cannot be opened, never a fabricated id. */
  packet_id: string;
  sha: string | null;
  /** When the gate-run opened: an RFC3339 instant when the packet
   *  carries one, else the bare date. */
  since: string;
}>;

/** A green gate-run an operator HELD off the dock on purpose — the
 *  other half of the stranded predicate (the Rust `HeldGreen`). It
 *  carries its reason so the page can say "held — <why>" in a neutral
 *  colour: a brake deliberately on is not an alarm. */
export type HeldGreen = Readonly<{
  branch: string;
  reason: string;
  since: string;
  packet_id: string;
  sha: string | null;
}>;

/** One gate currently being assessed — an open gate-run with no verdict
 *  yet. The Approach draws these into its parallel gate SLOTS. */
export type ActiveGate = Readonly<{
  branch: string;
  packet_id: string;
  /** An RFC3339 instant when the packet carries `opened_at`, else the
   *  bare opened_on date. Readers treat it as an instant only when it
   *  contains a `T` and parses — a bare date parses as midnight and
   *  would draw hours of elapsed that never happened. */
  since: string;
  /** The server's own reading that this run has outlived the runner's
   *  usual duration — a Job that died without reporting looks exactly
   *  like a slow one from here, and the bay must say so. Absent on an
   *  older server → false: no fabricated alarm. */
  stale: boolean;
}>;

/** One gate-run WAITING for a slot — filed and ordered, but not running.
 *  `boss gate --wait` takes a place in line when every bay is busy
 *  instead of refusing, and a queued run correctly occupies no bay; with
 *  nothing drawing it, three busy bays and two waiting looked exactly
 *  like three busy bays, and a queued gate was indistinguishable from
 *  one that never launched. Mirrors the Rust `QueuedGate`. */
export type QueuedGate = Readonly<{
  branch: string;
  packet_id: string;
  /** When the place in line was taken (RFC3339) — the ordering key. */
  queued_at: string;
  /** Place in line, 1-based, oldest first. */
  position: number;
  /** How long it has waited, seconds; null when the server had no clock
   *  or no parseable stamp. */
  waiting_seconds: number | null;
  /** How much longer it expects to wait, seconds — derived server-side
   *  from the MEASURED gate duration. Null when nothing was measured. */
  estimated_wait_seconds: number | null;
}>;

/** The gate slots the Approach renders: `capacity` (from the delivery
 *  policy — never a constant baked into the page), the runs occupying
 *  them right now, and the line waiting for one. */
export type Gates = Readonly<{
  capacity: number;
  active: readonly ActiveGate[];
  /** The queue, in the server's order. Empty on a server that predates
   *  the reading — an empty lane, never a fabricated one. */
  queued: readonly QueuedGate[];
  /** The gate duration the server MEASURED (median seconds) that every
   *  estimate above derives from; null when it measured nothing. */
  typical_seconds: number | null;
}>;

/** A car whose most-recent gate-run is red — waiting for rework. */
export type GaragedCar = Readonly<{
  branch: string;
  failed_check: string | null;
  since: string;
  packet_id: string;
  sha: string | null;
}>;

/** A car whose most-recent gate-run was never JUDGED — `lost` (the
 *  runner died before saying anything) or `unreadable` (the receipt
 *  would not parse). The gate exit, not the garage: the change asked a
 *  question and got no answer, which must read neither as rework nor as
 *  fine. Mirrors the Rust `LimboCar`. */
export type LimboCar = Readonly<{
  branch: string;
  /** `lost` / `unreadable`, as recorded. */
  verdict: string;
  since: string;
  packet_id: string;
  sha: string | null;
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
  /** Empty on a server that predates the reading. */
  held: readonly HeldGreen[];
  /** Cars standing ON the dock that cannot board — the other held lane,
   *  and the only surface that names them once the loading-dock station
   *  row stops listing them. Empty on a server that predates it. */
  held_cars: readonly HeldCar[];
  gates: Gates;
  garage: readonly GaragedCar[];
  /** The gate exit. Empty on a server that predates the reading. */
  limbo: readonly LimboCar[];
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

/** The ETA off the wire. An absent field, and a `kind` this build does
 *  not model, both read as "no estimate" WITH a reason rather than
 *  failing the page or inventing a number — the same forgiving shape
 *  `parseBlock` uses for an unknown block kind. */
function parseEta(raw: unknown): TrainEta {
  const o = asObjectOrEmpty(raw);
  if (o.kind === 'estimate') {
    return {
      kind: 'estimate',
      leg: String(o.leg ?? ''),
      remaining_seconds: Number(o.remaining_seconds ?? 0),
      remaining_low_seconds: Number(o.remaining_low_seconds ?? 0),
      remaining_high_seconds: Number(o.remaining_high_seconds ?? 0),
      sample_size: Number(o.sample_size ?? 0),
      basis: String(o.basis ?? ''),
      overdue: o.overdue === true,
    };
  }
  if (o.kind === 'unknown' && typeof o.reason === 'string') {
    return { kind: 'unknown', reason: o.reason };
  }
  return {
    kind: 'unknown',
    reason: 'this server sent no arrival estimate for the train',
  };
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
    boarded_at: typeof o.boarded_at === 'string' ? o.boarded_at : null,
    eta: parseEta(o.eta),
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

/** A held car is a dock row plus its reason — parsed through the same
 *  dock-row reader, so the two cannot drift, with the blank-reason rule
 *  the held-green lane already states. */
function parseHeldCar(raw: unknown): HeldCar {
  const o = asObjectOrEmpty(raw);
  const reason = typeof o.reason === 'string' && o.reason !== '' ? o.reason : 'no reason recorded';
  return { ...parseDockCar(raw), reason };
}

/** A reading the server states, or `null` when it states none. Anything
 *  else — an older server, a missing key, a typo — is "not stated", never
 *  `read`: claiming a read that was never stated is the defect this field
 *  exists to report. */
function parseReadState(raw: unknown): ReadState | null {
  return raw === 'read' || raw === 'unread' ? raw : null;
}

function parseBoarding(raw: unknown): BoardingPredicate {
  const o = asObjectOrEmpty(raw);
  return {
    dock_threshold: typeof o.dock_threshold === 'number' ? o.dock_threshold : null,
    cooldown_minutes: typeof o.cooldown_minutes === 'number' ? o.cooldown_minutes : null,
    at_times: Array.isArray(o.at_times) ? o.at_times.map(String) : [],
    cadence_reading: parseReadState(o.cadence_reading),
    // `?? 0` would turn the server's "I could not read the dock" into
    // "the dock is empty" — the defect in a new place (efe6ef10). An
    // absent key is no reading either.
    dock_depth: typeof o.dock_depth === 'number' ? o.dock_depth : null,
    threshold_met: typeof o.threshold_met === 'boolean' ? o.threshold_met : null,
    summary: String(o.summary ?? ''),
    // The hold: absent → null in every field. Never default a hold that
    // is not there, and never a "boards on the next tick" the server did
    // not say.
    held_because: typeof o.held_because === 'string' ? o.held_because : null,
    cooldown_remaining_minutes:
      typeof o.cooldown_remaining_minutes === 'number' ? o.cooldown_remaining_minutes : null,
    last_board_at: typeof o.last_board_at === 'string' ? o.last_board_at : null,
    last_board_reading: parseReadState(o.last_board_reading),
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

function parseQueuedGate(raw: unknown): QueuedGate {
  const o = asObject(raw, 'queued gate');
  return {
    branch: String(o.branch ?? ''),
    packet_id: String(o.packet_id ?? ''),
    queued_at: String(o.queued_at ?? ''),
    position: Number(o.position ?? 0),
    // A wait the server could not derive stays unknown — never zero,
    // which would read as "about to start".
    waiting_seconds: typeof o.waiting_seconds === 'number' ? o.waiting_seconds : null,
    estimated_wait_seconds:
      typeof o.estimated_wait_seconds === 'number' ? o.estimated_wait_seconds : null,
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
    queued: Array.isArray(o.queued) ? o.queued.map(parseQueuedGate) : [],
    typical_seconds: typeof o.typical_seconds === 'number' ? o.typical_seconds : null,
  };
}

/** The packet a lane row names, and the head it gated. Shared by every
 *  lane because every lane row is one gate-run: '' for a packet id a
 *  server did not send (the row then opens nothing, rather than opening
 *  the wrong thing) and null for a head nobody recorded. */
function parsePacket(o: Record<string, unknown>): { packet_id: string; sha: string | null } {
  return {
    packet_id: typeof o.packet_id === 'string' ? o.packet_id : '',
    sha: typeof o.sha === 'string' && o.sha !== '' ? o.sha : null,
  };
}

function parseGaragedCar(raw: unknown): GaragedCar {
  const o = asObject(raw, 'garaged car');
  return {
    branch: String(o.branch ?? ''),
    failed_check: typeof o.failed_check === 'string' ? o.failed_check : null,
    since: String(o.since ?? ''),
    ...parsePacket(o),
  };
}

function parseLimboCar(raw: unknown): LimboCar {
  const o = asObject(raw, 'limbo car');
  return {
    branch: String(o.branch ?? ''),
    verdict: typeof o.verdict === 'string' ? o.verdict : '',
    since: String(o.since ?? ''),
    ...parsePacket(o),
  };
}

function parseStrandedGreen(raw: unknown): StrandedGreen {
  const o = asObjectOrEmpty(raw);
  return {
    branch: String(o.branch ?? ''),
    since: String(o.since ?? ''),
    ...parsePacket(o),
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

function parseHeldGreen(raw: unknown): HeldGreen {
  const o = asObjectOrEmpty(raw);
  return {
    branch: String(o.branch ?? ''),
    reason: typeof o.reason === 'string' && o.reason !== '' ? o.reason : 'no reason recorded',
    since: String(o.since ?? ''),
    ...parsePacket(o),
  };
}

export function parseYardStatus(raw: unknown): YardStatus {
  const o = asObject(raw, 'yard status');
  return {
    trains: Array.isArray(o.trains) ? o.trains.map(parseTrain) : [],
    dock: Array.isArray(o.dock) ? o.dock.map(parseDockCar) : [],
    boarding: parseBoarding(o.boarding),
    recent: Array.isArray(o.recent) ? o.recent.map(parseRecent) : [],
    stranded: Array.isArray(o.stranded) ? o.stranded.map(parseStrandedGreen) : [],
    held: Array.isArray(o.held) ? o.held.map(parseHeldGreen) : [],
    held_cars: Array.isArray(o.held_cars) ? o.held_cars.map(parseHeldCar) : [],
    gates: parseGates(o.gates),
    garage: Array.isArray(o.garage) ? o.garage.map(parseGaragedCar) : [],
    limbo: Array.isArray(o.limbo) ? o.limbo.map(parseLimboCar) : [],
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
/** A queued run's one line: its place in line, what it has waited, and
 *  the server's measured estimate. Each part is dropped when the record
 *  does not carry it — an unknown wait is left unsaid rather than drawn
 *  as a number the page invented. A zero estimate is a bay standing
 *  free, which is a real state: `boss gate` queues on the count it saw
 *  and a slot can free before its next poll. */
export function queueLabel(q: QueuedGate): string {
  const parts = [`#${q.position} in line`];
  if (q.waiting_seconds !== null) parts.push(`waiting ${journeyText(q.waiting_seconds)}`);
  if (q.estimated_wait_seconds !== null) {
    parts.push(
      q.estimated_wait_seconds === 0
        ? 'a slot is free now'
        : `est. ~${journeyText(q.estimated_wait_seconds)}`,
    );
  }
  return parts.join(' · ');
}

export function gateSlots(gates: Pick<Gates, 'capacity' | 'active'>): readonly GateSlot[] {
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
  // No threshold has two causes — a registry with no depth rule, and a
  // cadence read that failed — and only the SERVER can tell them apart,
  // so the server's sentence is what renders. `cadence_reading` says
  // which one it is for anything that needs to branch.
  if (t === null) return b.summary !== '' ? b.summary : 'no boarding rule configured';
  const cooldown =
    b.cooldown_minutes !== null ? `the cooldown (${b.cooldown_minutes}m) clears` : null;
  const clockOf = (): string =>
    b.at_times.length > 0 ? ` · or by the clock at ${b.at_times.join(' / ')} UTC` : '';
  // NO READING comes first. Without a depth the threshold question has no
  // answer, and "0/4 parked — boards when the dock reaches 4" is the
  // sentence an operator acts on. The clock rule still boards, and it
  // never reads the depth, so it is still quoted.
  if (b.dock_depth === null)
    return `dock depth unread — the ${t}-car threshold cannot be evaluated${clockOf()}`;
  const depth = `${b.dock_depth}/${t} parked`;
  const rule = b.threshold_met
    ? `threshold met — ${depth}; boards ${cooldown ? `when ${cooldown}` : "on the conductor's next pass"}`
    : `${depth} — boards when the dock reaches ${t}${cooldown ? ` and ${cooldown}` : ''}`;
  return rule + clockOf();
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
  /** The last board as a clock time, `not read` when the server could
   *  not read the firing, or null when it has never boarded. A bare
   *  absence would render as "never boarded", which is the server's old
   *  permissive answer moved into the page. */
  lastBoard: string | null;
}>;

/** Why the dock is not boarding right now, from the server's own hold.
 *  Null when the server sent none (an older build): the page then keeps
 *  stating the rule, and never says "boards on the next tick" on its
 *  own authority. Every line is the server's sentence — the depth rule
 *  has no clock, so nothing here is a time of day. */
export function boardHold(b: BoardingPredicate): BoardHoldView | null {
  // Any input the server could not read makes this row a NON-READING, and
  // a non-reading is never painted as health — not green, and not the
  // plain colour a working hold wears. The server's own sentences already
  // say which input; this is the lens agreeing with them.
  const unread =
    b.cadence_reading === 'unread' || b.last_board_reading === 'unread' || b.dock_depth === null;
  const lastBoard = b.last_board_reading === 'unread' ? 'not read' : clockText(b.last_board_at);
  if (b.held_because !== null) {
    return {
      primary: { tone: unread ? 'muted' : null, text: `held: ${b.held_because}` },
      next: b.next_board,
      lastBoard,
    };
  }
  if (b.next_board !== null) {
    // An unread input is never `ok`. The server's sentence already says
    // "cannot say", and painting it green is the same defect in the lens
    // — the conductor lamp's "no reading" posture applies here too.
    const known = b.dock_threshold !== null && !unread;
    return {
      primary: { tone: known ? 'ok' : 'muted', text: b.next_board },
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

/** The ETA as a chip: the median remaining time with the measured
 *  10th–90th band beside it, or a stated refusal.
 *
 *  ALWAYS A BAND, never a bare number. Measured 2026-09-10 over 143
 *  arrivals, the spread WITHIN a single car count ran 770s to 4,274s
 *  while the medians between car counts differed by 10% — so the
 *  variation an operator needs to see is the spread, and a point
 *  estimate would read as a promise the pipeline never made.
 *
 *  An overdue train stops counting down and says so, in an alarm tone: a
 *  state past its own measured threshold must LOOK past it, not render
 *  like a healthy transit (CLAUDE.md §Diagnosis). */
export function etaReading(eta: TrainEta): Reading {
  if (eta.kind === 'unknown') return { tone: 'muted', text: 'no ETA' };
  if (eta.overdue) {
    const slowest = journeyText(eta.remaining_high_seconds);
    return {
      tone: 'err',
      text: `overdue — past the slowest of ${eta.sample_size} measured (${slowest})`,
    };
  }
  const band = `${journeyText(eta.remaining_low_seconds)}–${journeyText(eta.remaining_high_seconds)}`;
  return { tone: 'ok', text: `~${journeyText(eta.remaining_seconds)} left (${band})` };
}

/** The long form, for a tooltip: WHICH LEG the figure covers and what it
 *  was measured from — or, with no estimate, the reason in full. The leg
 *  is load-bearing: board→arrival and merge→arrival are materially
 *  different lengths, and a reader guessing which one they are looking at
 *  has no estimate at all. */
export function etaDetail(eta: TrainEta): string {
  if (eta.kind === 'unknown') return eta.reason;
  return `${eta.leg} — ${eta.basis}`;
}
