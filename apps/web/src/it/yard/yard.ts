// The train yard's data — a lens over the pipeline's queues
// (departure-board.md; pages-as-lenses). Every row derives from
// pr-train and ship-a-change Jobs the conductor already writes;
// audit-readonly reads only, so the guest landing renders whole.
//
// The loading dock is the first registry-backed lens (stations.md):
// its rows come from `GET /api/stations/loading-dock/queue` — the
// station's predicate and discipline evaluated server-side — with the
// old hand-rolled derivation kept as fallback for clusters that
// predate the registry.

export type StepLite = Readonly<{
  spec_slug?: string | null;
  title: string;
  status: string;
  metadata?: Record<string, unknown> | null;
  completed_on?: string | null;
  /** RFC3339 instant, when the conductor stamped one. The steps table
   *  carries only the day-granular `completed_on` column, so today the
   *  instant arrives in the step's metadata; read both so the lens
   *  needs no change when it becomes a column. */
  completed_at?: string | null;
}>;

/** Anything carrying a step list — a yard JobLite or a jobs/types Job.
 *  The step readers below take this so the job-detail page can reuse
 *  them without importing the yard's own Job shape. */
export type WithSteps = Readonly<{ steps?: readonly StepLite[] | null }>;

export type JobLite = Readonly<{
  id: string;
  kind: string;
  title: string;
  status: string;
  opened_on: string;
  tags?: readonly string[];
  metadata?: Record<string, unknown> | null;
  steps?: readonly StepLite[];
  /** Admission-fixed sim-vs-real flag on the Job row itself. */
  simulated?: boolean;
}>;

// A car in the yard is a job packet, and it renders as a card (David's
// call, 2026-08-12): protocol names the color, tags ride along, and a
// simulated packet is visibly not a real one. The same card grammar is
// meant to travel to every queue lens, so everything here derives from
// packet data — no per-kind code paths.
export type CarRow = Readonly<{
  id: string;
  kind: string;
  branch: string;
  title: string;
  tags: readonly string[];
  sim: boolean;
  skipReason?: string | null;
}>;

// The protocol palette + kind → hue hash + the sim predicate moved to
// web-kit with the card itself (@boss/web-kit/ui/packet-card) so every
// queue surface colors and marks packets identically. Re-exported so
// the definitions live exactly once (CLAUDE.md §9a) and yard consumers
// need no change.
import { isSim } from '@boss/web-kit/ui/packet-card';
export { isSim, PROTOCOL_PALETTE, protocolHue } from '@boss/web-kit/ui/packet-card';

export type TrainStatus = 'BOARDING' | 'BOARDED' | 'DEPARTED' | 'ARRIVED';
export type Lamp = 'green' | 'failing' | 'pending';

export type TrainRow = Readonly<{
  id: string;
  title: string;
  prUrl?: string | null;
  status: TrainStatus;
  lamp: Lamp;
  mergeRef?: string | null;
  deployed?: string | null;
  cars: readonly CarRow[];
  live: boolean;
  /** Why the train closed — `unknown` for one still in flight. */
  outcome: TrainOutcome;
  /** The best evidence of when it arrived, and what that evidence was. */
  arrivedAt: ArrivalStamp;
  /** An estimate, or the phase alone when there is nothing honest to say. */
  eta: Eta;
}>;

// The `GET /api/stations/{name}/queue` envelope (stations.md; the
// StationQueue struct in boss-jobs/src/station_queue.rs). Discipline
// keys and station kinds stay plain strings on this side: the lens
// renders whatever vocabulary the registry declares — a key published
// tomorrow needs zero code change here.
export type StationQueueEnvelope = Readonly<{
  station: string;
  kind: string;
  discipline: readonly string[];
  wip_limit?: number | null;
  over_limit: boolean;
  /** The station's declared upstream, when the row names one.
   *  Optional on the wire: a cluster whose registry predates
   *  119-station-upstream.sql omits the key entirely. */
  upstream?: StationUpstream | null;
  total: number;
  data: readonly JobLite[];
}>;

// Where the queue that FEEDS a station is read, exactly as the
// registry row declares it (`stations.upstream`; StationUpstream in
// boss-jobs). One object, not two loose strings: a label with no href
// is a dead button and an href with no label is an unlabelled one.
export type StationUpstream = Readonly<{ label: string; href: string }>;

// Where the dock's rows came from, plus the station's own facts when
// the registry served them. `derived` is the fallback for a deployed
// cluster that predates the station registry — the yard renders
// whole either way, it just can't show ordering rule or bandwidth
// state it never received.
export type DockStation =
  | Readonly<{
      source: 'station';
      discipline: readonly string[];
      wipLimit: number | null;
      overLimit: boolean;
      total: number;
      upstream: StationUpstream | null;
    }>
  | Readonly<{ source: 'derived' }>;

// Q2's resolution rendered: the ordering rule sits in the lens
// header in the mono-caps idiom — an operator should never wonder
// why the queue is in this order.
export function disciplineLabel(discipline: readonly string[]): string {
  return discipline.map(k => k.toUpperCase()).join(' → ');
}

// Q3's resolution rendered: `wip_limit` is advisory — a lens warning,
// never enforcement. Chip text only when the station declared a limit
// AND the server's verdict says the queue exceeds it.
export function wipAdvisory(station: DockStation): string | null {
  if (station.source !== 'station') return null;
  if (!station.overLimit || station.wipLimit === null) return null;
  return `WIP ${station.total}/${station.wipLimit}`;
}

// The walk upstream (David, feedback 3ccb79f5): "navigating to the
// upstream queues when jobs aren't materializing as expected. That is
// how our actual operators will diagnose the running system too."
//
// A lens that cannot walk upstream forces the operator out of the
// system to guess. So the button is NAVIGATION, not content: it adds
// no packet, no count, no state — it points at where this queue's
// traffic comes from.
//
// Everything about it is the station row's business. The lens supplies
// the "walk upstream" framing (the arrow, the word) and the row
// supplies the destination, so any station that declares an upstream
// gets the affordance with zero code here — and one that declares none
// renders nothing rather than a guess.
export type UpstreamButton = Readonly<{ label: string; href: string; title: string }>;

/** The button for a declared upstream, or null when there is nothing
 *  honest to point at. A half-declared pointer (label without href, or
 *  the reverse) is treated as absent: a dead navigational aid is worse
 *  than none, because it fails at exactly the moment it is trusted. */
export function upstreamButton(up: StationUpstream | null | undefined): UpstreamButton | null {
  if (!up) return null;
  const label = up.label.trim();
  const href = up.href.trim();
  if (label === '' || href === '') return null;
  const name = label.toUpperCase();
  return {
    label: `↑ UPSTREAM: ${name}`,
    href,
    title: `Walk upstream to ${name} — the queue that feeds this station`,
  };
}

/** The dock's walk upstream. Same shape as `wipAdvisory`: a station
 *  fact off the envelope, or null for the derived fallback — which has
 *  no registry row and therefore nothing to say about upstream. */
export function dockUpstream(station: DockStation): UpstreamButton | null {
  return station.source === 'station' ? upstreamButton(station.upstream) : null;
}

export type YardState = Readonly<{
  inFlight: readonly TrainRow[];
  dock: readonly CarRow[];
  dockStation: DockStation;
  arrivals: readonly TrainRow[];
  /** Closed without arriving. Kept visible — a train that cancelled is
   *  a fact about the day, it just isn't an arrival. */
  cancelled: readonly TrainRow[];
  /** The scoreboard. Empty when the report is unavailable or has
   *  resolved nothing — the yard renders nothing rather than zeros. */
  delivery: readonly DeliveryStat[];
  /** Merged, deployed, awaiting an in-production check. These belong to
   *  none of the yard's other three partitions, which is why seven of
   *  them were invisible on 2026-08-28. */
  awaitingProof: readonly CarRow[];
}>;

function step(j: WithSteps, slug: string, titleFallback: string): StepLite | null {
  return (
    j.steps?.find(
      s => (s.spec_slug ?? '') === slug || s.title === titleFallback,
    ) ?? null
  );
}

const done = (s: StepLite | null) =>
  !!s && (s.status === 'completed' || s.status === 'skipped');

// A terminal reads STRICTLY completed. `done` counts `skipped` — right
// for progress (a skipped step is settled) and wrong here: the
// terminal close marks every step it did NOT fire as skipped, so a
// cancelled train carries a skipped `arrived` step. Reading that as
// done is how an empty train reached the top of the arrivals board.
const completed = (s: StepLite | null) => !!s && s.status === 'completed';

// ---------------------------------------------------------------------------
// Arrivals — which trains arrived, and when.
//
// `close_job_on_terminal` (boss-jobs/src/http/steps.rs) stamps
// `job.metadata.outcome` from the Workflow's terminal step, so the
// pr-train spec's two terminals surface as `arrived` / `cancelled`.
// Trains that closed before the terminals existed carry neither; the
// completed `deployed` step is the same evidence the `arrived`
// terminal is gated on (`steps.deployed.done AND steps.ci.done`), so
// it stands in.
// ---------------------------------------------------------------------------

export type TrainOutcome = 'arrived' | 'cancelled' | 'unknown';

export function trainOutcome(j: JobLite): TrainOutcome {
  const stamped = (j.metadata as { outcome?: unknown } | null)?.outcome;
  if (stamped === 'arrived') return 'arrived';
  if (stamped === 'cancelled') return 'cancelled';
  if (completed(step(j, 'arrived', 'Train arrived'))) return 'arrived';
  if (completed(step(j, 'cancelled', 'Cancelled — nothing to board'))) return 'cancelled';
  if (completed(step(j, 'deployed', 'Deployed to the playground'))) return 'arrived';
  return 'unknown';
}

/** The conductor's RFC3339 stamp on a step, column or metadata. */
function stampAt(s: StepLite | null): string | null {
  if (!s) return null;
  if (typeof s.completed_at === 'string' && s.completed_at !== '') return s.completed_at;
  const md = (s.metadata as { completed_at?: unknown } | null)?.completed_at;
  return typeof md === 'string' && md !== '' ? md : null;
}

export type ArrivalBasis = 'completed_at' | 'completed_on' | 'opened_on';
export type ArrivalStamp = Readonly<{ ms: number; at: string; basis: ArrivalBasis }>;

// Order the board by the best available arrival instant. `opened_on`
// is day-granular, so ordering by it ties every train opened on the
// same day — which is how a ported train from March outranked this
// morning's arrival on an arbitrary tie-break.
export function arrivalStamp(j: JobLite): ArrivalStamp {
  const arrived = step(j, 'arrived', 'Train arrived');
  const deployed = step(j, 'deployed', 'Deployed to the playground');
  const candidates: ReadonlyArray<readonly [string | null | undefined, ArrivalBasis]> = [
    [stampAt(arrived), 'completed_at'],
    [stampAt(deployed), 'completed_at'],
    [arrived?.completed_on, 'completed_on'],
    [deployed?.completed_on, 'completed_on'],
    [j.opened_on, 'opened_on'],
  ];
  for (const [at, basis] of candidates) {
    if (typeof at !== 'string' || at === '') continue;
    const ms = Date.parse(at);
    if (Number.isNaN(ms)) continue;
    return { ms, at, basis };
  }
  return { ms: 0, at: '', basis: 'opened_on' };
}

// ---------------------------------------------------------------------------
// The landing report the conductor writes into the arrived step's
// metadata. Every field may be null and nothing here fills one in: a
// report with no `merged_sha` renders without a merged_sha.
// ---------------------------------------------------------------------------

export type ArrivalCar = Readonly<{
  car_id_short: string | null;
  title: string | null;
  branch: string | null;
}>;
export type ArrivalSkip = Readonly<{ car_id_short: string | null; reason: string | null }>;
export type ArrivalTimings = Readonly<{
  boarded_at: string | null;
  merged_at: string | null;
  deployed_at: string | null;
  arrived_at: string | null;
  board_to_merge_s: number | null;
  merge_to_deploy_s: number | null;
  total_s: number | null;
}>;
export type ArrivalReport = Readonly<{
  consist: readonly ArrivalCar[];
  left_behind: readonly ArrivalSkip[];
  generation: string | null;
  merged_sha: string | null;
  timings: ArrivalTimings | null;
}>;

const asObject = (v: unknown): Record<string, unknown> | null =>
  typeof v === 'object' && v !== null && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
const asText = (v: unknown): string | null =>
  typeof v === 'string' && v !== ''
    ? v
    : typeof v === 'number' && Number.isFinite(v)
      ? String(v)
      : null;
const asNumber = (v: unknown): number | null =>
  typeof v === 'number' && Number.isFinite(v) ? v : null;

function timings(v: unknown): ArrivalTimings | null {
  const t = asObject(v);
  if (!t) return null;
  return {
    boarded_at: asText(t.boarded_at),
    merged_at: asText(t.merged_at),
    deployed_at: asText(t.deployed_at),
    arrived_at: asText(t.arrived_at),
    board_to_merge_s: asNumber(t.board_to_merge_s),
    merge_to_deploy_s: asNumber(t.merge_to_deploy_s),
    total_s: asNumber(t.total_s),
  };
}

/** The train's landing report, or null for a train that has none —
 *  every train that arrived before the conductor started writing one. */
export function arrivalReport(j: WithSteps): ArrivalReport | null {
  const steps = j.steps ?? [];
  const arrived = step(j, 'arrived', 'Train arrived');
  const raw = [arrived, ...steps]
    .map(s => asObject((s?.metadata as { arrival_report?: unknown } | null)?.arrival_report))
    .find(o => o !== null);
  if (!raw) return null;
  return {
    consist: (Array.isArray(raw.consist) ? raw.consist : []).map(c => {
      const o = asObject(c) ?? {};
      return {
        car_id_short: asText(o.car_id_short),
        title: asText(o.title),
        branch: asText(o.branch),
      };
    }),
    left_behind: (Array.isArray(raw.left_behind) ? raw.left_behind : []).map(c => {
      const o = asObject(c) ?? {};
      return { car_id_short: asText(o.car_id_short), reason: asText(o.reason) };
    }),
    generation: asText(raw.generation),
    merged_sha: asText(raw.merged_sha),
    timings: timings(raw.timings),
  };
}

// ---------------------------------------------------------------------------
// ETAs. The batching the train does is exactly the coordination cost an
// ETA answers — but an estimate that looks like a promise is worse than
// none. So: the numbers come from what recent trains ACTUALLY did
// (median board→merge, median merge→deploy), a train with no started-at
// evidence gets its phase and no time, and the chip always wears `~`.
// ---------------------------------------------------------------------------

export type ArrivalMedians = Readonly<{
  boardToMergeS: number | null;
  mergeToDeployS: number | null;
  /** How many arrivals contributed a usable leg — the estimate's basis. */
  samples: number;
}>;

export const NO_MEDIANS: ArrivalMedians = {
  boardToMergeS: null,
  mergeToDeployS: null,
  samples: 0,
};

/** How many recent arrivals the medians are taken over. */
export const ARRIVAL_SAMPLE_WINDOW = 5;

function legSeconds(from: string | null, to: string | null): number | null {
  if (from === null || to === null) return null;
  const a = Date.parse(from);
  const b = Date.parse(to);
  if (Number.isNaN(a) || Number.isNaN(b) || b < a) return null;
  return (b - a) / 1000;
}

function median(xs: readonly number[]): number | null {
  // One sample is an anecdote, not a median.
  if (xs.length < 2) return null;
  const sorted = [...xs].sort((a, b) => a - b);
  const mid = sorted.length >> 1;
  return sorted.length % 2 === 1 ? sorted[mid]! : (sorted[mid - 1]! + sorted[mid]!) / 2;
}

/** Medians over the most recent arrivals (newest first, as the board
 *  orders them). The conductor's own measured timings win when it
 *  reported them; otherwise the legs are re-derived from step stamps. */
export function arrivalMedians(
  arrived: readonly JobLite[],
  window: number = ARRIVAL_SAMPLE_WINDOW,
): ArrivalMedians {
  const boardToMerge: number[] = [];
  const mergeToDeploy: number[] = [];
  let samples = 0;
  for (const j of arrived.slice(0, window)) {
    const t = arrivalReport(j)?.timings ?? null;
    const bm =
      t?.board_to_merge_s ??
      legSeconds(
        stampAt(step(j, 'pr', 'Open the batched PR')),
        stampAt(step(j, 'merged', 'Merged into main')),
      );
    const md =
      t?.merge_to_deploy_s ??
      legSeconds(
        stampAt(step(j, 'merged', 'Merged into main')),
        stampAt(step(j, 'deployed', 'Deployed to the playground')),
      );
    if (bm !== null) boardToMerge.push(bm);
    if (md !== null) mergeToDeploy.push(md);
    if (bm !== null || md !== null) samples += 1;
  }
  return {
    boardToMergeS: median(boardToMerge),
    mergeToDeployS: median(mergeToDeploy),
    samples,
  };
}

export type EtaPhase = 'boarding' | 'ci' | 'merging' | 'deploying' | 'blocked' | 'arrived';

export function etaPhase(status: TrainStatus, lamp: Lamp): EtaPhase {
  switch (status) {
    case 'ARRIVED':
      return 'arrived';
    case 'DEPARTED':
      return 'deploying';
    case 'BOARDED':
      // Red CI is not a slow leg, it's a stopped one.
      return lamp === 'failing' ? 'blocked' : lamp === 'green' ? 'merging' : 'ci';
    case 'BOARDING':
      return 'boarding';
  }
}

export type Eta =
  | Readonly<{ kind: 'phase'; phase: EtaPhase }>
  | Readonly<{ kind: 'eta'; phase: EtaPhase; atMs: number; basis: string }>;

export function trainEta(j: JobLite, medians: ArrivalMedians, nowMs: number): Eta {
  const phase = etaPhase(trainStatus(j), ciLamp(j));
  const phaseOnly: Eta = { kind: 'phase', phase };
  const basis = `median of last ${medians.samples} arrivals`;
  const { boardToMergeS, mergeToDeployS } = medians;

  // Remaining seconds on the leg under way, plus whatever legs follow.
  // `startedAt` is the evidence requirement: no stamp, no estimate —
  // an invented CI duration is exactly the kind of number that reads
  // as a promise.
  const project = (startedAt: string | null, legS: number, restS: number): Eta => {
    if (startedAt === null) return phaseOnly;
    const from = Date.parse(startedAt);
    if (Number.isNaN(from)) return phaseOnly;
    const left = Math.max(legS - (nowMs - from) / 1000, 0) + restS;
    return { kind: 'eta', phase, atMs: nowMs + Math.round(left * 1000), basis };
  };

  if (phase === 'ci' || phase === 'merging') {
    if (boardToMergeS === null || mergeToDeployS === null) return phaseOnly;
    return project(
      stampAt(step(j, 'pr', 'Open the batched PR')),
      boardToMergeS,
      mergeToDeployS,
    );
  }
  if (phase === 'deploying') {
    if (mergeToDeployS === null) return phaseOnly;
    return project(stampAt(step(j, 'merged', 'Merged into main')), mergeToDeployS, 0);
  }
  return phaseOnly;
}

export function trainStatus(j: JobLite): TrainStatus {
  if (done(step(j, 'deployed', 'Deployed to the playground')) || j.status === 'closed')
    return 'ARRIVED';
  if (done(step(j, 'merged', 'Merged into main'))) return 'DEPARTED';
  if (done(step(j, 'pr', 'Open the batched PR'))) return 'BOARDED';
  return 'BOARDING';
}

export function ciLamp(j: JobLite): Lamp {
  const ci = step(j, 'ci', 'CI verdict');
  const result = (ci?.metadata as { result?: string } | null)?.result;
  if (result === 'green') return 'green';
  if (result === 'failing') return 'failing';
  return 'pending';
}

export function toTrainRow(
  j: JobLite,
  shipById: ReadonlyMap<string, JobLite>,
  live: boolean,
  medians: ArrivalMedians = NO_MEDIANS,
  nowMs: number = Date.now(),
): TrainRow {
  const md = (j.metadata ?? {}) as {
    boarded_jobs?: string[];
  };
  const pr = step(j, 'pr', 'Open the batched PR');
  const merged = step(j, 'merged', 'Merged into main');
  const deployed = step(j, 'deployed', 'Deployed to the playground');
  const cars: CarRow[] = (md.boarded_jobs ?? []).map(id => {
    const car = shipById.get(id);
    const cmd = (car?.metadata ?? {}) as {
      branch?: string;
      skip_reason?: string;
    };
    return {
      id,
      kind: car?.kind ?? 'ship-a-change',
      branch: cmd.branch ?? id.slice(0, 8),
      title: car?.title ?? '(car not in window)',
      tags: car?.tags ?? [],
      sim: car ? isSim(car) : false,
      skipReason: cmd.skip_reason ?? null,
    };
  });
  return {
    id: j.id,
    title: j.title,
    prUrl: ((pr?.metadata ?? {}) as { pr_url?: string }).pr_url ?? null,
    status: trainStatus(j),
    lamp: ciLamp(j),
    mergeRef: ((merged?.metadata ?? {}) as { merge_ref?: string }).merge_ref ?? null,
    deployed: ((deployed?.metadata ?? {}) as { deployed?: string }).deployed ?? null,
    cars,
    live,
    outcome: trainOutcome(j),
    arrivedAt: arrivalStamp(j),
    eta: trainEta(j, medians, nowMs),
  };
}

// One packet → one card, whoever chose the packet. Both dock paths —
// the station envelope and the local derivation — map through here,
// so the card grammar cannot fork between them.
function carRow(j: JobLite): CarRow {
  const md = (j.metadata ?? {}) as { branch?: string; skip_reason?: string };
  return {
    id: j.id,
    kind: j.kind,
    branch: md.branch ?? '',
    title: j.title,
    tags: j.tags ?? [],
    sim: isSim(j),
    skipReason: md.skip_reason ?? null,
  };
}

// The fallback derivation: the loading-dock predicate hand-rolled in
// code, kept only for clusters that predate the station registry.
// When `GET /api/stations/loading-dock/queue` serves, the registry
// row (predicate + discipline) replaces all of this.
export function dockRows(ships: readonly JobLite[]): CarRow[] {
  return ships
    .filter(j => {
      const md = (j.metadata ?? {}) as { branch?: string; train?: string };
      if (j.status !== 'open' || !md.branch || md.train) return false;
      const review = step(j, 'review', 'Open for review');
      return !!review && (review.status === 'ready' || review.status === 'active');
    })
    .map(carRow);
}

/** How many arrivals the board shows, and how many cancellations. */
const ARRIVALS_SHOWN = 5;
const CANCELLED_SHOWN = 3;

export function assembleYard(
  trains: readonly JobLite[],
  ships: readonly JobLite[],
  dockQueue: StationQueueEnvelope | null = null,
  nowMs: number = Date.now(),
  // LAST on purpose: 22 call sites pass `nowMs` as the fourth argument,
  // and inserting ahead of it would silently reinterpret a timestamp as
  // a report. Additive parameters go on the end.
  report: TerminalReport | null = null,
): YardState {
  const shipById = new Map(ships.map(j => [j.id, j]));
  const open = trains.filter(t => t.status === 'open');
  // Arrivals are trains that ARRIVED, newest first by the best instant
  // each one carries. Everything else that closed is kept aside rather
  // than dropped — a cancelled train is a fact about the day.
  const closed = trains
    .filter(t => t.status === 'closed')
    .map(t => ({ t, outcome: trainOutcome(t), stamp: arrivalStamp(t) }))
    .sort((a, b) => b.stamp.ms - a.stamp.ms);
  const arrived = closed.filter(c => c.outcome === 'arrived').map(c => c.t);
  const medians = arrivalMedians(arrived);
  // The one signal-green element: the oldest still-moving train.
  const liveId = open.find(t => trainStatus(t) !== 'ARRIVED')?.id;
  return {
    inFlight: open.map(t => toTrainRow(t, shipById, t.id === liveId, medians, nowMs)),
    // The envelope is authoritative when it served: membership came
    // from the registry predicate and order from the declared
    // discipline — a client re-sort would silently overrule the
    // station row, so the rows map 1:1 in server order.
    dock: dockQueue ? dockQueue.data.map(carRow) : dockRows(ships),
    dockStation: dockQueue
      ? {
          source: 'station',
          discipline: dockQueue.discipline,
          wipLimit: dockQueue.wip_limit ?? null,
          overLimit: dockQueue.over_limit,
          total: dockQueue.total,
          upstream: dockQueue.upstream ?? null,
        }
      : { source: 'derived' },
    arrivals: arrived
      .slice(0, ARRIVALS_SHOWN)
      .map(t => toTrainRow(t, shipById, false, medians, nowMs)),
    cancelled: closed
      .filter(c => c.outcome !== 'arrived')
      .slice(0, CANCELLED_SHOWN)
      .map(c => toTrainRow(c.t, shipById, false, medians, nowMs)),
    delivery: deliveryStats(report),
    awaitingProof: awaitingProof(ships).map(carRow),
  };
}

// The dock's station row, or null when the cluster can't serve one —
// 404 (no such station), 503 (registry not configured), a network
// fault, or a 200 that isn't the envelope all mean the same thing:
// fall back to deriving the dock locally. Never an error the yard
// surfaces; the fallback costs nothing because the ships list is
// fetched anyway for the consist join.
async function fetchDockQueue(): Promise<StationQueueEnvelope | null> {
  try {
    const r = await fetch('/api/stations/loading-dock/queue');
    if (!r.ok) return null;
    const env = (await r.json()) as StationQueueEnvelope;
    return Array.isArray(env?.data) && Array.isArray(env?.discipline) ? env : null;
  } catch {
    return null;
  }
}

export async function fetchYard(): Promise<YardState | null> {
  const [tr, sr, dockQueue, report] = await Promise.all([
    // 40, not 20: the window has to hold the open trains, the five
    // arrivals the board shows, AND the arrivals the ETA medians are
    // taken over — cancelled trains sit in the same list and would
    // otherwise crowd the samples out.
    fetch('/api/jobs?kind=pr-train&limit=40'),
    fetch('/api/jobs?kind=ship-a-change&limit=200'),
    fetchDockQueue(),
    // The scoreboard is ADDITIVE: a yard that cannot show its stats is
    // still a yard, so this resolves to null rather than failing the
    // whole page. The stats are the thing you read second; the trains
    // are the thing you came for.
    fetch('/api/workflows/ship-a-change/terminal-report')
      .then((r) => (r.ok ? (r.json() as Promise<TerminalReport>) : null))
      .catch(() => null),
  ]);
  if (!tr.ok || !sr.ok) return null;
  const trains = ((await tr.json()) as { data?: JobLite[] }).data ?? [];
  const ships = ((await sr.json()) as { data?: JobLite[] }).data ?? [];
  return assembleYard(trains, ships, dockQueue, Date.now(), report);
}

// ---------------------------------------------------------------------
// Delivery stats — the yard's scoreboard.
// ---------------------------------------------------------------------

/** One version's row from `/api/workflows/{kind}/terminal-report`. */
export type TerminalVersion = Readonly<{
  version: number;
  total: number;
  by_status?: Readonly<Record<string, number>> | null;
  outcomes?: Readonly<Record<string, number>> | null;
  cycle_time_days?: Readonly<{
    median: number | null;
    p90: number | null;
    samples: number;
  }> | null;
}>;

export type TerminalReport = Readonly<{
  kind: string;
  versions?: readonly TerminalVersion[] | null;
}>;

/** What the yard shows at the top: a number, and the direction it moved. */
export type DeliveryStat = Readonly<{
  label: string;
  value: string;
  /** The comparison version's value, or null when there is nothing to compare. */
  previous: string | null;
  /** How many packets the CURRENT value is computed from. */
  samples: number;
  /** true when `samples` is too small to read as a trend. */
  provisional: boolean;
}>;

/** Below this, a rate is noise dressed as a measurement. */
export const MIN_SAMPLES = 5;

function resolved(v: TerminalVersion): number {
  const o = v.outcomes ?? {};
  return Object.values(o).reduce((a, b) => a + b, 0);
}

function abandonRate(v: TerminalVersion): number | null {
  const n = resolved(v);
  if (n === 0) return null;
  return ((v.outcomes?.abandoned ?? 0) / n) * 100;
}

/**
 * The most recent version that has RESOLVED anything, and the most
 * recent one before it that also has.
 *
 * A version with packets still in flight reports no rate at all — which
 * is the common case for the version published an hour ago, and is
 * exactly when a naive "latest version" reading would print 0% and look
 * like a triumph. On 2026-08-28 v24 and v25 held 8 packets between them
 * with zero resolved.
 */
export function comparableVersions(
  report: TerminalReport | null,
): { current: TerminalVersion | null; previous: TerminalVersion | null } {
  const withOutcomes = (report?.versions ?? [])
    .filter((v) => resolved(v) > 0)
    .slice()
    .sort((a, b) => b.version - a.version);
  return { current: withOutcomes[0] ?? null, previous: withOutcomes[1] ?? null };
}

function pct(n: number | null): string {
  return n === null ? '—' : `${Math.round(n)}%`;
}

function days(v: TerminalVersion): string {
  const m = v.cycle_time_days?.median;
  return m === null || m === undefined ? '—' : `${m}d`;
}

/**
 * The yard's headline numbers.
 *
 * WHY ABANDON RATE LEADS. David, 2026-08-28: "We should have these stats
 * at the top of the Train Yard if they are what matter." Abandon rate is
 * the one that moves for protocol reasons rather than luck — a car
 * abandoned is a change that was written, gated and then thrown away
 * with its history. Cycle time sits beside it because a rate that
 * improves by shipping slower is not an improvement.
 *
 * EVERY NUMBER CARRIES ITS SAMPLE COUNT, and is marked provisional below
 * MIN_SAMPLES. A 50% abandon rate over two packets is not a trend, and a
 * scoreboard that cannot say so invites exactly the wrong reaction.
 */
export function deliveryStats(report: TerminalReport | null): readonly DeliveryStat[] {
  const { current, previous } = comparableVersions(report);
  if (!current) return [];
  const n = resolved(current);
  return [
    {
      label: 'abandon rate',
      value: pct(abandonRate(current)),
      previous: previous ? pct(abandonRate(previous)) : null,
      samples: n,
      provisional: n < MIN_SAMPLES,
    },
    {
      label: 'median cycle',
      value: days(current),
      previous: previous ? days(previous) : null,
      samples: current.cycle_time_days?.samples ?? 0,
      provisional: (current.cycle_time_days?.samples ?? 0) < MIN_SAMPLES,
    },
    {
      label: 'delivered',
      value: String(current.outcomes?.merged ?? 0),
      previous: previous ? String(previous.outcomes?.merged ?? 0) : null,
      samples: n,
      provisional: false,
    },
  ];
}

/**
 * Cars that merged and are waiting on an in-production check.
 *
 * These appear NOWHERE in the yard today: it partitions into open
 * trains, arrived trains and the dock, and a merged-but-unproven car is
 * none of those. Seven were invisible on 2026-08-28, which is the state
 * we had agreed was the bottleneck.
 */
export function awaitingProof(cars: readonly JobLite[]): readonly JobLite[] {
  return cars.filter((c) => {
    if (c.status !== 'open') return false;
    const step = (c.steps ?? []).find((s) => s.status === 'ready' || s.status === 'active');
    return step?.spec_slug === 'proven';
  });
}
