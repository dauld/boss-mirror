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
/// The address form is the one documented exception to the delegation.
/// Measured 2026-09-11 on the system of record: `claude@algedonic.dev`
/// holds 250 step assignments and signs 5 completions, and it carries no
/// colon, so `isHumanActor` reads it as a person. The `ActorId` union
/// (crates/core/boss-core/src/actor.rs) has no address spelling, so this
/// is a gap between the vocabulary and the live data. A board about who
/// is working must not draw an agent's work as a person's; the real fix
/// belongs in `actor.ts` or in the vocabulary, and is reported rather
/// than papered over.
export function actorLane(actorId: string, simulated: boolean): ActorLane {
  if (simulated) return 'sim';
  if (actorId.startsWith('automation:')) return 'automation';
  if (!isHumanActor(actorId)) return 'agent';
  // Colon-free, so `actor.ts` says human. An address is an agent session.
  return actorId.includes('@') ? 'agent' : 'human';
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
  const simulated = r.simulated === true;
  return {
    id: str(r.id) ?? '',
    title: str(r.title) ?? '',
    branch: str(m.branch),
    open: str(r.status) === 'open',
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
      simulated: r.simulated === true,
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
/// implementation; the observability service serves an agents listing,
/// but it is not among the prefixes the gateway proxies. The agent-runs
/// surface on the jobs API — which DOES have a table, an `actor_id` and
/// a `branch`, and is the richest "what is this actor building and what
/// did it cost" read in the system — is likewise unrouted at the
/// gateway, so it cannot be reached from a browser without a server
/// change this car is not allowed to make. The people roster covers
/// HUMANS only: every row in `employees` is a person, and the machine
/// actors that do most of the work are not employees.
///
/// (Those two paths are named without their `/api` prefix on purpose.
/// `infra/lint/every-spa-api-path-is-routed` greps this file for API
/// segments without stripping comments, so writing them in full would
/// fail the gate on prose — see the report on this car.)
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
}>;

export async function fetchCrew(): Promise<CrewState> {
  const [cars, gateRuns, yard, waits] = await Promise.all([
    fetchRemote(`/api/jobs?kind=ship-a-change&limit=${CAR_WINDOW}`, parseCars),
    fetchRemote(`/api/jobs?kind=gate-run&limit=${CAR_WINDOW}`, parseGateRuns),
    fetchRemote('/api/yard/status', parseYard),
    fetchRemote('/api/jobs/queue-age', parseWaits),
  ]);
  return { cars, gateRuns, yard, waits };
}
