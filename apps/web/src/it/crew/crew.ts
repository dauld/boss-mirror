// The Crew Board's read model — the MIDDLE third of the operator
// surface (backlog 04c5bbc0, answered by David 2026-09-11).
//
// The Train Yard shows the last third of a car's life and the
// Marshalling Yard shows the first. This shows the interval in between,
// which had no surface at all: an actor building something, while it
// happens. Prototype on a real snapshot, 2026-09-08:
// https://claude.ai/code/artifact/36b9a721-73fb-49f8-bb7d-701c43da76a5
//
// NO NUMBER THIS SURFACE MAKES UP. The rule the Marshalling Yard's
// module states and this one inherits. Every figure below is folded out
// of a payload measured against the system of record on 2026-09-11:
// `/api/yard/status`, `/api/jobs?kind=ship-a-change`,
// `/api/jobs?kind=gate-run` and `/api/jobs/queue-age`. Where a figure
// cannot be counted it is `null` and the page renders the reason — never
// a zero, which on a board about who is working reads as "nobody is".
//
// WHAT IS DELIBERATELY ABSENT: the prototype's writes-per-hour strip.
// David's decision put the audit tail last, "behind whatever the
// events-readability decision turns out to be... If the audit tail is
// the blocker, ship the board without the writes strip rather than
// waiting for it." Measured 2026-09-11: `/api/events/*` is served by
// boss-events-api behind the gateway's auth-gated proxy and answers 401
// without a browser session; `TailQuery` has no actor parameter, the
// per-actor identity lives only inside `payload._actor`, `limit` is
// clamped to 500, and `/api/events/stats` breaks down by day and kind
// and not by actor. A per-actor rate here could only be fabricated, so
// it is absent. `crew.test.ts` pins its absence.

import { fetchRemote, type Remote } from '../../data/remote';
import { standingAt } from '../../jobs/position';
import { partitionOf, type Partition } from '@boss/web-kit/ui/packet-card';
import { isHumanActor } from '../../data/actor';

// ---------------------------------------------------------------------
// Small readers. Absence is never coerced to a value.
// ---------------------------------------------------------------------

const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);
const num = (v: unknown): number | null =>
  typeof v === 'number' && Number.isFinite(v) ? v : null;

/// `/api/jobs` answers `{data, total}`; `/api/yard/status` answers a bare
/// object; some reads answer a bare array. Accept all three.
const rows = (raw: unknown): ReadonlyArray<Record<string, unknown>> => {
  if (Array.isArray(raw)) return raw as ReadonlyArray<Record<string, unknown>>;
  const data = (raw as { data?: unknown } | null)?.data;
  return Array.isArray(data) ? (data as ReadonlyArray<Record<string, unknown>>) : [];
};

const meta = (r: Record<string, unknown>): Record<string, unknown> =>
  (r.metadata && typeof r.metadata === 'object' ? r.metadata : {}) as Record<string, unknown>;

// ---------------------------------------------------------------------
// Actor identity
// ---------------------------------------------------------------------

/// The four lanes the board draws, which are the four David's decision
/// names: human / agent / automation / sim.
export type ActorLane = 'human' | 'agent' | 'automation' | 'sim';

/// Which lane an actor's work belongs in.
///
/// The human/machine split is NOT re-derived here — `data/actor.ts` owns
/// it ("the one definition of that rule on the client", CLAUDE.md §9a)
/// and this function delegates to it. What it adds is the sub-split the
/// board needs, which `actor.ts` does not answer: automation versus
/// agent, and the sim lane.
///
/// `simulated` comes from the PACKET, not the id: `emp-sim-brewer` is an
/// ordinary employee id, and the brewery's event clock runs about a
/// thousand times faster than the wall — simulated work measured against
/// a real clock cannot share a lane with the real crew.
///
/// The address form (`claude@algedonic.dev`, an agent's session login)
/// is NOT an exception any more: `isHumanActor` reads an `@` as a
/// machine since fix/one-actor-one-spelling, so it lands in the agent
/// lane through the same delegation as every other machine id. The
/// local `includes('@')` branch this function once carried is gone —
/// it had become unreachable, and its comment sent readers to look for
/// a fix that had already landed (2178203d).
export function actorLane(actorId: string, simulated: boolean): ActorLane {
  if (simulated) return 'sim';
  if (actorId.startsWith('automation:')) return 'automation';
  return isHumanActor(actorId) ? 'human' : 'agent';
}

// ---------------------------------------------------------------------
// Cars — `GET /api/jobs?kind=ship-a-change`
// ---------------------------------------------------------------------

/// One completion, as the steps table records it since 2026-09-08
/// (`a-completion-names-its-actor`). This is the board's only evidence
/// of output, and it is first-party: the server stamps `completed_by`
/// and `completed_at` at the flip to `completed` and freezes them.
export type Completion = Readonly<{
  actorId: string;
  /// RFC3339 instant. `null` on a step completed before the column
  /// existed — deliberately not backfilled, so absence means unknown.
  at: string | null;
  /// The day-granular column, which is SIM-dated like the rest of the
  /// packet. "Today" is compared against this.
  on: string | null;
}>;

export type Car = Readonly<{
  id: string;
  title: string;
  /// The branch the build produced. `null` is the whole residue signal —
  /// see `isBuilding`.
  branch: string | null;
  open: boolean;
  /// The packet's partition (508cc38c), parsed at this boundary;
  /// `simulated` is derived from it — not-real — so a shadow car sits
  /// in the sim lane until car 4 draws it as its own.
  partition: Partition;
  simulated: boolean;
  abandoned: boolean;
  /// The train this car boarded, when it has boarded one.
  train: string | null;
  /// Whether the car's branch reached main.
  merged: boolean;
  openedAt: string | null;
  /// Actors currently holding a step on this packet.
  assignees: ReadonlyArray<string>;
  completions: ReadonlyArray<Completion>;
}>;

function parseJobLike(r: Record<string, unknown>): Car {
  const m = meta(r);
  const steps = Array.isArray(r.steps)
    ? (r.steps as ReadonlyArray<Record<string, unknown>>)
    : [];
  const partition = partitionOf(r);
  const simulated = partition !== 'real';
  return {
    id: str(r.id) ?? '',
    title: str(r.title) ?? '',
    branch: str(m.branch),
    open: str(r.status) === 'open',
    partition,
    simulated,
    abandoned: m.abandoned === true,
    train: str(m.train),
    merged: m.merged === true,
    openedAt: str(m.opened_at) ?? str(r.opened_on),
    assignees: steps
      .filter((s) => str(s.status) === 'ready' || str(s.status) === 'active')
      .map((s) => str(s.assignee_id))
      .filter((a): a is string => a !== null),
    completions: steps
      .map((s) => ({
        actorId: str(s.completed_by),
        at: str(s.completed_at),
        on: str(s.completed_on),
      }))
      .filter((c): c is Completion => c.actorId !== null),
  };
}

export function parseCars(raw: unknown): ReadonlyArray<Car> {
  return rows(raw)
    .map(parseJobLike)
    .filter((c) => c.id !== '');
}

// ---------------------------------------------------------------------
// The gate registry — `GET /api/jobs?kind=gate-run`
// ---------------------------------------------------------------------

export type GateRun = Readonly<{
  id: string;
  branch: string | null;
  /// `passed` / `failed` / `null` while the verdict is not yet recorded.
  outcome: string | null;
  open: boolean;
  openedAt: string | null;
  completions: ReadonlyArray<Completion>;
}>;

export function parseGateRuns(raw: unknown): ReadonlyArray<GateRun> {
  return rows(raw)
    .map((r) => {
      const c = parseJobLike(r);
      return {
        id: c.id,
        branch: c.branch,
        outcome: str(meta(r).outcome),
        open: c.open,
        openedAt: c.openedAt,
        completions: c.completions,
      };
    })
    .filter((g) => g.id !== '');
}

/// Every branch the gate registry has a run for.
///
/// This set is the BUILDING predicate's discriminator, and it is the
/// reason the gate registry is read at all: it is first in David's
/// stated order of confidence, and it is the only record that says a
/// branch has stopped being built and started being assessed.
export function gatedBranches(runs: ReadonlyArray<GateRun>): ReadonlySet<string> {
  return new Set(runs.map((g) => g.branch).filter((b): b is string => b !== null));
}

// ---------------------------------------------------------------------
// The yard — `GET /api/yard/status`
// ---------------------------------------------------------------------

export type ActiveGate = Readonly<{
  branch: string;
  packetId: string;
  since: string | null;
  /// The server's own word for a run that has outlived a gate Job's
  /// `activeDeadlineSeconds`: it is not gating, it is a corpse holding
  /// a bay. Rendered as trouble, never as a healthy run.
  stale: boolean;
}>;

export type QueuedGate = Readonly<{
  branch: string;
  packetId: string;
  position: number | null;
  waitingSeconds: number | null;
  estimatedWaitSeconds: number | null;
}>;

export type DockCar = Readonly<{
  id: string;
  branch: string;
  title: string;
  parkedSince: string | null;
}>;

export type GarageCar = Readonly<{
  branch: string;
  packetId: string;
  failedCheck: string | null;
  since: string | null;
}>;

export type YardLanes = Readonly<{
  now: string | null;
  gatesCapacity: number | null;
  active: ReadonlyArray<ActiveGate>;
  queued: ReadonlyArray<QueuedGate>;
  dock: ReadonlyArray<DockCar>;
  garage: ReadonlyArray<GarageCar>;
}>;

export function parseYard(raw: unknown): YardLanes {
  const o = (raw ?? {}) as Record<string, unknown>;
  const gates = (o.gates ?? {}) as Record<string, unknown>;
  const list = (v: unknown): ReadonlyArray<Record<string, unknown>> =>
    Array.isArray(v) ? (v as ReadonlyArray<Record<string, unknown>>) : [];
  return {
    now: str(o.now),
    gatesCapacity: num(gates.capacity),
    active: list(gates.active)
      .map((g) => ({
        branch: str(g.branch) ?? '',
        packetId: str(g.packet_id) ?? '',
        since: str(g.since),
        stale: g.stale === true,
      }))
      .filter((g) => g.branch !== ''),
    queued: list(gates.queued)
      .map((g) => ({
        branch: str(g.branch) ?? '',
        packetId: str(g.packet_id) ?? '',
        position: num(g.position),
        waitingSeconds: num(g.waiting_seconds),
        estimatedWaitSeconds: num(g.estimated_wait_seconds),
      }))
      .filter((g) => g.branch !== ''),
    dock: list(o.dock)
      .map((d) => ({
        id: str(d.id) ?? '',
        branch: str(d.branch) ?? '',
        title: str(d.title) ?? '',
        parkedSince: str(d.parked_since),
      }))
      .filter((d) => d.branch !== ''),
    garage: list(o.garage)
      .map((g) => ({
        branch: str(g.branch) ?? '',
        packetId: str(g.packet_id) ?? '',
        failedCheck: str(g.failed_check),
        since: str(g.since),
      }))
      .filter((g) => g.branch !== ''),
  };
}

// ---------------------------------------------------------------------
// Agent runs — `GET /api/jobs?kind=agent-run&status=open`
// ---------------------------------------------------------------------

/// One agent's run of one protocol step, as `boss dispatch` files it
/// (design c87fb59b car 2, backlog 39d0b528): the packet it executes,
/// the settings it launched under, where it runs, and the step the run
/// itself is at. The FIRST first-party record of a build while it
/// happens — until this kind existed the BUILDING lane above could only
/// infer one from a branch with no gate-run behind it.
export type AgentRun = Readonly<{
  id: string;
  title: string;
  /// The job whose step this run executes, and that step's slug.
  packet: string | null;
  step: string | null;
  /// The actor the run signs as.
  agent: string | null;
  model: string | null;
  budgetUsd: number | null;
  effort: string | null;
  host: string | null;
  /// The run's own open step, as the standing phrase — `Report recorded
  /// (ready, not yet done)` — or `null` between states.
  at: string | null;
  openedAt: string | null;
  /// The work-session the run was dispatched from (design 511fa7d4 car
  /// 2b: written by `boss dispatch --from-hook`), or `null` for a run
  /// dispatched by hand.
  session: string | null;
  /// When the packet last MOVED: the newest `completed_at` across its
  /// completed steps, else `metadata.opened_at` — the instant the
  /// age-out rule measures silence from (backlog 5082a08b). `null` when
  /// the record holds neither.
  lastMovedAt: string | null;
  /// `building` is open (ready or active) — the one state the age-out
  /// rule reads, so the only one silence is stated for.
  building: boolean;
}>;

export function parseAgentRuns(raw: unknown): ReadonlyArray<AgentRun> {
  return rows(raw)
    .map((r) => {
      const m = meta(r);
      const steps = Array.isArray(r.steps)
        ? (r.steps as ReadonlyArray<Record<string, unknown>>)
        : [];
      const open = steps.find((s) => str(s.status) === 'ready' || str(s.status) === 'active');
      return {
        id: str(r.id) ?? '',
        title: str(r.title) ?? '',
        packet: str(m.packet),
        step: str(m.step),
        agent: str(m.agent),
        model: str(m.model),
        budgetUsd: num(m.budget_usd),
        effort: str(m.effort),
        host: str(m.host),
        // Its title with its status beside it: the run's titles are
        // perfect-tense (`Report recorded`), and the bare slug
        // `reported` beside a run still waiting to report read as done
        // (3102fe7a, after 648a68a9).
        at: open
          ? standingAt(str(open.title) ?? str(open.spec_slug) ?? '', str(open.status) ?? '')
          : null,
        openedAt: str(m.opened_at) ?? str(r.opened_on),
        session: str(m.session),
        // The handler's own reading, not a variant of it: the stamps
        // are RFC 3339 UTC from one server clock, so the newest is the
        // largest instant; `opened_on` (a date) is not an instant and
        // is not a fallback here.
        lastMovedAt:
          steps
            .filter((s) => str(s.status) === 'completed')
            .map((s) => str(s.completed_at))
            .filter((t): t is string => t !== null && Number.isFinite(Date.parse(t)))
            .reduce<string | null>(
              (a, t) => (a === null || Date.parse(t) > Date.parse(a) ? t : a),
              null,
            ) ?? str(m.opened_at),
        building: steps.some(
          (s) =>
            str(s.spec_slug) === 'building' &&
            (str(s.status) === 'ready' || str(s.status) === 'active'),
        ),
      };
    })
    .filter((a) => a.id !== '');
}

// ---------------------------------------------------------------------
// Silence — which open runs have not moved (backlog 5082a08b)
// ---------------------------------------------------------------------

/// The hours a run's `building` may stand with the packet unmoved
/// before `agent-run-dies-when-building-is-silent` completes it `died`:
/// that rule's `hours` arg. It lives twice because a browser cannot
/// read the rules directory, so crew.test.ts holds the two equal
/// (CLAUDE.md 9a) — the board must never call a run silent on a
/// different clock from the rule that kills it.
export const SILENT_BOUND_HOURS = 4;

export type Silence = Readonly<{ hours: number; past: boolean }>;

/// How long an open run has stood unmoved, against the bound. `null`
/// for a run whose `building` is not open (the rule does not age it —
/// a run waiting at `reported` is held by the gate, not silent) and for
/// one whose record holds no instant: an unmeasured silence is not
/// zero. Past the bound the run is one the hourly rule has not yet
/// reached, and the board says so rather than waiting for the tick.
export function silence(run: AgentRun, nowIso: string): Silence | null {
  if (!run.building || run.lastMovedAt === null) return null;
  const ms = Date.parse(nowIso) - Date.parse(run.lastMovedAt);
  if (!Number.isFinite(ms)) return null;
  const hours = Math.round((Math.max(0, ms) / 3_600_000) * 10) / 10;
  return { hours, past: hours > SILENT_BOUND_HOURS };
}

export function silenceText(s: Silence): string {
  return s.past
    ? `${s.hours}h unmoved — past the ${SILENT_BOUND_HOURS}h bound`
    : `${s.hours}h unmoved`;
}

// ---------------------------------------------------------------------
// The finish record — `GET /api/agent-runs` (backlog 5082a08b)
// ---------------------------------------------------------------------

/// One row of `agent_runs`: a run that reached a terminal, with what it
/// cost. `boss dispatch --report` writes it, keyed by the agent-run
/// packet's id, and ONLY at a terminal — the row is insert-once
/// (8f1de7bf) — so an OPEN run has no cost to show yet, and this is a
/// list of finished runs rather than a column on the open ones.
///
/// Two limits of the record ride on the rows rather than being papered
/// over: rows before the effort instrumentation carry no `effort`
/// (fd5ce137's era boundary), and rows before 65c9c05a carry no branch.
/// Each renders as not recorded, never as a guess.
export type RunRecord = Readonly<{
  runId: string;
  actor: string | null;
  model: string | null;
  outcome: string | null;
  finishedAt: string | null;
  /// Wall-clock minutes, start to finish; `null` without both stamps.
  minutes: number | null;
  tokens: number | null;
  /// Priced cost in micro-dollars; `null` is UNPRICED, never free.
  usdMicros: number | null;
  branch: string | null;
  packet: string | null;
  effort: string | null;
}>;

export function parseRunRecords(raw: unknown): ReadonlyArray<RunRecord> {
  return rows(raw)
    .map((r) => {
      const detail = (r.detail && typeof r.detail === 'object' ? r.detail : {}) as Record<
        string,
        unknown
      >;
      const started = Date.parse(str(r.started_at) ?? '');
      const finished = Date.parse(str(r.finished_at) ?? '');
      return {
        runId: str(r.run_id) ?? '',
        actor: str(r.actor_id),
        model: str(r.model),
        outcome: str(r.outcome),
        finishedAt: str(r.finished_at),
        minutes:
          Number.isFinite(started) && Number.isFinite(finished)
            ? Math.round((finished - started) / 60_000)
            : null,
        tokens: num(r.total_tokens),
        usdMicros: num(r.usd_micros),
        branch: str(r.branch),
        packet: str(r.job_id),
        effort: str(detail.effort),
      };
    })
    .filter((r) => r.runId !== '');
}

/// A run's cost as the board prints it. Unpriced is said in words: a
/// `$0.00` for a run nobody priced would read as a free one.
export function costText(r: RunRecord): string {
  return r.usdMicros === null ? 'not priced' : `$${(r.usdMicros / 1_000_000).toFixed(2)}`;
}

// ---------------------------------------------------------------------
// Sessions — `GET /api/jobs?kind=work-session&status=open`
// ---------------------------------------------------------------------

/// One operator's session, as the SessionStart hook files it and the
/// prompt hook heartbeats it (design 511fa7d4 car 2b, backlog
/// da925366). The shop floor: sessions are the crews, and the runs
/// linked to a session are the cars that crew is building.
export type Session = Readonly<{
  id: string;
  title: string;
  actor: string | null;
  host: string | null;
  cwd: string | null;
  startedAt: string | null;
  /// The heartbeat — the last prompt's instant. `null` until the first
  /// prompt after SessionStart.
  lastActiveAt: string | null;
  promptCount: number | null;
  /// Agent-tool calls the dispatch hook could parse no packet from.
  untrackedRuns: number | null;
}>;

export function parseSessions(raw: unknown): ReadonlyArray<Session> {
  return rows(raw)
    .map((r) => {
      const m = meta(r);
      return {
        id: str(r.id) ?? '',
        title: str(r.title) ?? '',
        actor: str(m.actor),
        host: str(m.host),
        cwd: str(m.cwd),
        startedAt: str(m.started_at) ?? str(m.opened_at),
        lastActiveAt: str(m.last_active_at),
        promptCount: num(m.prompt_count),
        untrackedRuns: num(m.untracked_runs),
      };
    })
    .filter((s) => s.id !== '');
}

/// A session with the runs it dispatched. `idle` is the design's own
/// threshold — silent past an hour, measured from the heartbeat (or the
/// start, before the first prompt) — and `null` when there is no clock
/// to measure against: an unknown is never drawn as "at work".
export type Crew = Readonly<{
  session: Session;
  runs: ReadonlyArray<AgentRun>;
  idle: boolean | null;
}>;

/// Silent past this many milliseconds, a crew is drawn idle. The clock
/// rule ends a session at six hours; an hour is where the board stops
/// calling it "working".
export const IDLE_AFTER_MS = 60 * 60 * 1000;

const epoch = (iso: string | null): number | null => {
  if (iso === null) return null;
  const t = Date.parse(iso);
  return Number.isFinite(t) ? t : null;
};

/// Sessions folded with their runs, oldest session first, plus the runs
/// no listed session claims — a hand dispatch, or a session outside the
/// read window — which the runs table still shows.
export function crews(
  sessions: ReadonlyArray<Session>,
  runs: ReadonlyArray<AgentRun>,
  now: string | null,
): Readonly<{ crews: ReadonlyArray<Crew>; unlinked: ReadonlyArray<AgentRun> }> {
  const nowMs = epoch(now);
  const ids = new Set(sessions.map((s) => s.id));
  const folded = [...sessions]
    .sort((a, b) => (a.startedAt ?? '').localeCompare(b.startedAt ?? ''))
    .map((session) => {
      const since = epoch(session.lastActiveAt) ?? epoch(session.startedAt);
      return {
        session,
        runs: runs.filter((r) => r.session === session.id),
        idle: nowMs === null || since === null ? null : nowMs - since > IDLE_AFTER_MS,
      };
    });
  return {
    crews: folded,
    unlinked: runs.filter((r) => r.session === null || !ids.has(r.session)),
  };
}

// ---------------------------------------------------------------------
// Waits — `GET /api/jobs/queue-age`
// ---------------------------------------------------------------------

/// One open obligation and how long it has sat there. The projection's
/// own shape, including the honesty flag: `exact: false` means the stamp
/// is an `updated_at` fallback and the age is a LOWER BOUND.
export type Wait = Readonly<{
  jobId: string;
  jobKind: string;
  jobTitle: string;
  stepId: string | null;
  stepTitle: string;
  status: string;
  assigneeId: string | null;
  waitingDays: number;
  exact: boolean;
  partition: Partition;
  simulated: boolean;
  since: string | null;
}>;

export function parseWaits(raw: unknown): ReadonlyArray<Wait> {
  return rows(raw)
    .map((r) => ({
      jobId: str(r.job_id) ?? '',
      jobKind: str(r.job_kind) ?? '',
      jobTitle: str(r.job_title) ?? '',
      stepId: str(r.step_id),
      stepTitle: str(r.step_title) ?? '',
      status: str(r.status) ?? '',
      assigneeId: str(r.assignee_id),
      waitingDays: num(r.waiting_days) ?? 0,
      exact: r.exact === true,
      partition: partitionOf(r),
      simulated: partitionOf(r) !== 'real',
      since: str(r.since),
    }))
    .filter((w) => w.jobId !== '');
}

// ---------------------------------------------------------------------
// BUILDING — the predicate the residue problem forced
// ---------------------------------------------------------------------

/// Is this packet a car being built RIGHT NOW?
///
/// "An open `ship-a-change` that has not been gated" is the obvious
/// reading and it is WRONG, which is the whole reason this function
/// exists. On 2026-09-10 three such packets sat at `scope` with no
/// branch: abandoned cadence sweeps (21edde87), which that predicate
/// would have drawn as three actors mid-build.
///
/// The predicate is **a branch, with no gate-run behind it**:
///
///  - **a branch must exist.** A build produces a branch; the sweeps
///    never had one. This is the half that rejects the residue, and it
///    rejects it on what a build IS rather than on a cleanup marker
///    that a future sweep might forget to set.
///  - **the gate registry must not have seen it.** The first record in
///    David's order of confidence, and the moment a branch stops being
///    built and starts being assessed. This is the half that keeps a
///    car from being drawn in two columns at once.
///
/// Three cheap guards sit alongside, each rejecting a packet that has a
/// branch but is demonstrably not under construction: `abandoned`
/// (belt-and-braces with the branch test — the residue tripped both),
/// and `train` / `merged`, which say the car is further down the line.
export function isBuilding(car: Car, gated: ReadonlySet<string>): boolean {
  if (!car.open) return false;
  if (car.branch === null) return false;
  if (car.abandoned) return false;
  if (car.train !== null || car.merged) return false;
  return !gated.has(car.branch);
}

// ---------------------------------------------------------------------
// The pipeline track
// ---------------------------------------------------------------------

export type TrackStage = 'building' | 'gating' | 'parked' | 'boarded' | 'landed';

export type TrackCar = Readonly<{
  branch: string;
  title: string;
  /// Where it sits, in the surface's own words. Never a bare stage name:
  /// a gate holding a bay and a gate waiting for one are the same stage
  /// and different facts, and three busy bays with two waiting must not
  /// read as three busy bays.
  detail: string;
  /// True when the server itself flags this position as troubled.
  troubled: boolean;
  packetId: string | null;
}>;

export type PipelineTrack = Readonly<{
  building: ReadonlyArray<TrackCar>;
  gating: ReadonlyArray<TrackCar>;
  parked: ReadonlyArray<TrackCar>;
  boarded: ReadonlyArray<TrackCar>;
  landed: ReadonlyArray<TrackCar>;
  /// Not a stage on the track — a siding. A car whose gate failed is out
  /// of the pipeline until somebody acts, and hiding it would make the
  /// track look healthier than it is.
  garage: ReadonlyArray<TrackCar>;
}>;

/// BUILDING → GATING → PARKED → BOARDED → LANDED, each lane from the
/// record that actually knows.
///
/// The yard is authoritative for GATING and PARKED: it reads the gate
/// registry and the dock station server-side and already distinguishes a
/// bay from a queue and a live run from a corpse. The car packets answer
/// BOARDED and LANDED from their own `train` / `merged` markers. BUILDING
/// is the only derived lane, and `isBuilding` is its whole definition.
///
/// A branch appears in exactly ONE lane. Lanes are claimed left to right
/// from the yard outwards, and a branch already claimed is skipped — so
/// a car whose gate-run has aged out of the read window still cannot be
/// drawn as building while the yard says it is in a bay.
export function pipelineTrack(
  cars: ReadonlyArray<Car>,
  gateRuns: ReadonlyArray<GateRun>,
  yard: YardLanes,
): PipelineTrack {
  const claimed = new Set<string>();
  const claim = (c: TrackCar): TrackCar => {
    claimed.add(c.branch);
    return c;
  };
  const titleOf = (branch: string, fallback: string): string =>
    cars.find((c) => c.branch === branch)?.title ?? fallback;

  const gating: ReadonlyArray<TrackCar> = [
    ...yard.active.map((g) =>
      claim({
        branch: g.branch,
        title: titleOf(g.branch, g.branch),
        detail: g.stale
          ? 'holding a gate bay past the limit a gate Job can live — not gating'
          : 'in a gate bay',
        troubled: g.stale,
        packetId: g.packetId,
      }),
    ),
    ...yard.queued.map((g) =>
      claim({
        branch: g.branch,
        title: titleOf(g.branch, g.branch),
        detail:
          g.position === null
            ? 'queued for a gate bay'
            : `queued for a gate bay, position ${g.position}`,
        troubled: false,
        packetId: g.packetId,
      }),
    ),
  ];

  const parked: ReadonlyArray<TrackCar> = yard.dock
    .filter((d) => !claimed.has(d.branch))
    .map((d) =>
      claim({
        branch: d.branch,
        title: d.title || titleOf(d.branch, d.branch),
        detail: d.parkedSince === null ? 'parked on the dock' : `parked since ${d.parkedSince}`,
        troubled: false,
        packetId: d.id,
      }),
    );

  const garage: ReadonlyArray<TrackCar> = yard.garage
    .filter((g) => !claimed.has(g.branch))
    .map((g) =>
      claim({
        branch: g.branch,
        title: titleOf(g.branch, g.branch),
        detail:
          g.failedCheck === null
            ? 'gate failed — the failing check was not recorded'
            : `gate failed on ${g.failedCheck}`,
        troubled: true,
        packetId: g.packetId,
      }),
    );

  const gated = gatedBranches(gateRuns);

  const landed: ReadonlyArray<TrackCar> = cars
    .filter((c) => c.branch !== null && c.merged && !claimed.has(c.branch))
    .map((c) =>
      claim({
        branch: c.branch!,
        title: c.title,
        detail: c.train === null ? 'merged to main' : 'merged to main on a train',
        troubled: false,
        packetId: c.id,
      }),
    );

  const boarded: ReadonlyArray<TrackCar> = cars
    .filter((c) => c.branch !== null && c.train !== null && !c.merged && !claimed.has(c.branch))
    .map((c) =>
      claim({
        branch: c.branch!,
        title: c.title,
        detail: 'boarded a train, in transit',
        troubled: false,
        packetId: c.id,
      }),
    );

  const building: ReadonlyArray<TrackCar> = cars
    .filter((c) => isBuilding(c, gated) && !claimed.has(c.branch!))
    .map((c) =>
      claim({
        branch: c.branch!,
        title: c.title,
        detail: 'branch opened, no gate run yet',
        troubled: false,
        packetId: c.id,
      }),
    );

  return { building, gating, parked, boarded, landed, garage };
}

// ---------------------------------------------------------------------
// Actor cards
// ---------------------------------------------------------------------

export type ActorCard = Readonly<{
  id: string;
  lane: ActorLane;
  /// Open steps this actor is holding, across ALL open work — the scope
  /// `/api/jobs/queue-age` answers. Labelled as such on the card: it is
  /// a wider scope than the output figure beside it, and a card that
  /// blurred the two would be making a claim neither read supports.
  holds: number;
  /// Steps this actor completed TODAY on car-pipeline packets — the
  /// `ship-a-change` and `gate-run` window this board reads, and nothing
  /// beyond it. Not "writes": the audit tail is unreadable from here.
  completedToday: number;
  /// The latest completion stamp actually seen. `null` means no write
  /// was recorded in the window, which the card renders as "not
  /// recorded" — never as a date, and never as zero.
  lastWriteAt: string | null;
  /// What the actor is holding right now, longest wait first.
  holding: ReadonlyArray<Wait>;
}>;

/// The crew, folded out of the reads — because there is no roster to read.
///
/// There is no `agents` table and no browser-reachable actor registry.
/// `AgentSpec` is a type plus a TOML file with only an in-memory
/// implementation; the observability service served `/api/agents`, but
/// that prefix was never among the ones the gateway proxies, and the
/// service retired on 2026-09-23 (467175e7). The
/// `/api/agent-runs` surface on the jobs API — which DOES have a table,
/// an `actor_id` and a `branch`, and is the richest "what is this actor
/// building and what did it cost" read in the system — is likewise
/// unrouted at the gateway, so it cannot be reached from a browser
/// without a server change this car is not allowed to make. The people
/// roster covers HUMANS only: every row in `employees` is a person, and
/// the machine actors that do most of the work are not employees.
///
/// So the crew is derived, which is the honest answer and is stated on
/// the page: an actor exists here because it holds a step or signed a
/// completion. Nothing is listed that was not seen doing something.
export function actorCards(input: {
  cars: ReadonlyArray<Car>;
  gateRuns: ReadonlyArray<GateRun>;
  waits: ReadonlyArray<Wait>;
  today: string;
}): ReadonlyArray<ActorCard> {
  const { cars, gateRuns, waits, today } = input;

  type Acc = {
    holds: number;
    completedToday: number;
    lastWriteAt: string | null;
    simulated: boolean;
    holding: Wait[];
  };
  const acc = new Map<string, Acc>();
  const get = (id: string): Acc => {
    const existing = acc.get(id);
    if (existing) return existing;
    const fresh: Acc = {
      holds: 0,
      completedToday: 0,
      lastWriteAt: null,
      simulated: false,
      holding: [],
    };
    acc.set(id, fresh);
    return fresh;
  };

  // Output and last write, from the car-pipeline packets.
  const packets: ReadonlyArray<{
    simulated: boolean;
    assignees: ReadonlyArray<string>;
    completions: ReadonlyArray<Completion>;
  }> = [
    ...cars,
    ...gateRuns.map((g) => ({ simulated: false, assignees: [], completions: g.completions })),
  ];
  for (const p of packets) {
    for (const a of p.assignees) {
      const e = get(a);
      if (p.simulated) e.simulated = true;
    }
    for (const c of p.completions) {
      const e = get(c.actorId);
      if (p.simulated) e.simulated = true;
      if (c.on === today) e.completedToday += 1;
      // Latest stamp wins. RFC3339 from one server sorts lexically.
      if (c.at !== null && (e.lastWriteAt === null || c.at > e.lastWriteAt)) {
        e.lastWriteAt = c.at;
      }
    }
  }

  // What each actor holds, across all open work.
  for (const w of waits) {
    if (w.assigneeId === null) continue;
    const e = get(w.assigneeId);
    e.holds += 1;
    e.holding.push(w);
    if (w.simulated) e.simulated = true;
  }

  return [...acc.entries()]
    .map(([id, e]) => ({
      id,
      lane: actorLane(id, e.simulated),
      holds: e.holds,
      completedToday: e.completedToday,
      lastWriteAt: e.lastWriteAt,
      holding: [...e.holding].sort((a, b) => b.waitingDays - a.waitingDays),
    }))
    // Busiest first, so the board leads with who is working. Ties break
    // on what they are holding, then on the id, so the order is stable
    // across refreshes rather than shuffling under the reader.
    .sort(
      (a, b) =>
        b.completedToday - a.completedToday ||
        b.holds - a.holds ||
        a.id.localeCompare(b.id),
    );
}

// ---------------------------------------------------------------------
// Taken, not progressed
// ---------------------------------------------------------------------

/// Steps somebody claimed and has not moved.
///
/// Claimed is the point: an unassigned step that has waited ten days is a
/// queue problem and belongs on the Marshalling Yard. A step a named
/// actor TOOK and has not advanced is a crew problem, which is this
/// board's question.
///
/// Simulated waits are excluded — the brewery's event clock runs about a
/// thousand times faster than the wall, so a simulated wait measured on a
/// real clock is a meaningless number that would sit permanently at the
/// top of this table.
export function takenNotProgressed(
  waits: ReadonlyArray<Wait>,
  minDays: number,
): ReadonlyArray<Wait> {
  return waits
    .filter((w) => w.assigneeId !== null && !w.simulated && w.waitingDays >= minDays)
    .sort((a, b) => b.waitingDays - a.waitingDays);
}

/// How long it has waited — and, when the stamp is a fallback, that the
/// figure is a floor rather than the fact.
export function waitText(w: Wait): string {
  const magnitude =
    w.waitingDays >= 1
      ? `${Math.floor(w.waitingDays)} d`
      : `${Math.round(w.waitingDays * 24)} h`;
  return w.exact ? magnitude : `at least ${magnitude}`;
}

// ---------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------

/// How many car packets and gate runs to read. A WINDOW, and the page
/// says so: the LANDED column can only show what this window holds, and
/// a limit is not a filter.
export const CAR_WINDOW = 60;

export type CrewState = Readonly<{
  cars: Exclude<Remote<ReadonlyArray<Car>>, { kind: 'loading' }>;
  gateRuns: Exclude<Remote<ReadonlyArray<GateRun>>, { kind: 'loading' }>;
  yard: Exclude<Remote<YardLanes>, { kind: 'loading' }>;
  waits: Exclude<Remote<ReadonlyArray<Wait>>, { kind: 'loading' }>;
  /// OPEN runs only: a run that landed, was refused or died is history
  /// the packet's own page tells; this board is about now.
  agentRuns: Exclude<Remote<ReadonlyArray<AgentRun>>, { kind: 'loading' }>;
  /// OPEN sessions: the crews on the floor right now.
  sessions: Exclude<Remote<ReadonlyArray<Session>>, { kind: 'loading' }>;
  /// The newest finished runs from `agent_runs`, with what each cost.
  runRecords: Exclude<Remote<ReadonlyArray<RunRecord>>, { kind: 'loading' }>;
}>;

export async function fetchCrew(): Promise<CrewState> {
  const [cars, gateRuns, yard, waits, agentRuns, sessions, runRecords] = await Promise.all([
    fetchRemote(`/api/jobs?kind=ship-a-change&limit=${CAR_WINDOW}`, parseCars),
    fetchRemote(`/api/jobs?kind=gate-run&limit=${CAR_WINDOW}`, parseGateRuns),
    fetchRemote('/api/yard/status', parseYard),
    fetchRemote('/api/jobs/queue-age', parseWaits),
    fetchRemote(`/api/jobs?kind=agent-run&status=open&limit=${CAR_WINDOW}`, parseAgentRuns),
    fetchRemote(`/api/jobs?kind=work-session&status=open&limit=${CAR_WINDOW}`, parseSessions),
    fetchRemote(`/api/agent-runs?limit=${CAR_WINDOW}`, parseRunRecords),
  ]);
  return { cars, gateRuns, yard, waits, agentRuns, sessions, runRecords };
}
