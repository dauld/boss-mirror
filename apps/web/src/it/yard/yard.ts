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
  /** The head the packet names — `boarded_head` once boarded, else the
   *  head the gate receipt recorded — shortened to seven, or null when
   *  no record carries one. Painted beside the branch on the floor. */
  head: string | null;
  /** What the packet records about PROVING it — the four `proof_*` keys
   *  and the `proven` step's stamp ([`readCarProof`]). Null when the
   *  packet records none of them; absent on a row built before the
   *  reader existed. The inspection shed is a lens over this and
   *  nothing else. */
  proof?: CarProof | null;
}>;

/** A probe run the forge wrote back onto the car (`proof_attempt`). Only
 *  a run that did NOT settle the car survives here: a run that exits
 *  zero and prints what was claimed completes the `proven` step, which
 *  is recorded as a [`ProvenStamp`] instead. */
export type ProofAttempt = Readonly<{
  at: string | null;
  exit: number | null;
  host: string | null;
  /** The two streams, SEPARATELY, as the runner records them and as the
   *  proof record has always carried them. They used to arrive merged in
   *  one `output`, and the case that cost 18 hours read `output: ""` —
   *  which cannot say whether both streams were empty or the record
   *  dropped them (backlog 4fccc595). An attempt written before the
   *  split carries `output`; it is read into `stdout`, since that is
   *  where most of it came from. */
  stdout: string | null;
  stderr: string | null;
  /** The runner's verdict — one sentence naming what failed, never a
   *  list of possibilities (CLAUDE.md §Diagnosis). The first thing a
   *  reader of a failed attempt wants. */
  why: string | null;
  /** Tools the probe needed and the host did not have. */
  missingTools: readonly string[];
}>;

/** The `proven` step completed — the transition a car leaves the
 *  inspection shed on. `by` is the step's `proven_by` (`run-car-probe`
 *  when the forge drained the request, else whoever ran `boss prove`). */
export type ProvenStamp = Readonly<{ at: string | null; by: string | null }>;

/** Everything a car's packet says about proving it in production.
 *
 *  `probe` / `expect` are what `boss gate --park-probe/--park-expect`
 *  recorded and the arrival rule hands the forge; `event` is the prose a
 *  car carries when only an EVENT can settle it, which no probe can
 *  run; `attempt` is the forge's last unsuccessful run; `stamped` is the
 *  `proven` step completing. Nothing here is derived — five fields, five
 *  places the packet holds them. */
export type CarProof = Readonly<{
  probe: string | null;
  expect: string | null;
  event: string | null;
  attempt: ProofAttempt | null;
  stamped: ProvenStamp | null;
}>;

const text = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);

function proofAttempt(v: unknown): ProofAttempt | null {
  if (typeof v !== 'object' || v === null) return null;
  const a = v as Record<string, unknown>;
  const tools = Array.isArray(a.missing_tools) ? a.missing_tools.filter((t): t is string => typeof t === 'string') : [];
  return {
    at: text(a.at),
    exit: typeof a.exit === 'number' ? a.exit : null,
    host: text(a.host),
    stdout: text(a.stdout) ?? text(a.output),
    stderr: text(a.stderr),
    why: text(a.why),
    missingTools: tools,
  };
}

/** The car's proof record, or null when the packet holds none of it.
 *
 *  Null rather than a row of nulls on purpose: "this packet says nothing
 *  about proving it" and "this packet records an empty probe" are
 *  different facts, and the second one is a defect an operator should
 *  see rather than a shape the floor smooths over. */
export function readCarProof(j: JobLite | null | undefined): CarProof | null {
  if (!j) return null;
  const md = (j.metadata ?? {}) as Record<string, unknown>;
  const proven = step(j, 'proven', 'Proven in production');
  const stamped: ProvenStamp | null =
    proven !== null && proven.status === 'completed'
      ? { at: stampAt(proven) ?? text(proven.completed_on), by: text((proven.metadata ?? {}).proven_by) }
      : null;
  const proof: CarProof = {
    probe: text(md.proof_probe),
    expect: text(md.proof_expect),
    event: text(md.proof_event),
    attempt: proofAttempt(md.proof_attempt),
    stamped,
  };
  const empty =
    proof.probe === null &&
    proof.expect === null &&
    proof.event === null &&
    proof.attempt === null &&
    proof.stamped === null &&
    // An EMPTY probe string is still a record — the packet carries the
    // key, it just says nothing useful. Keep the row so the shed can
    // draw the car rather than silently treat it as unrecorded.
    !('proof_probe' in md);
  return empty ? null : proof;
}

// The protocol palette + kind → hue hash + the sim predicate moved to
// web-kit with the card itself (@boss/web-kit/ui/packet-card) so every
// queue surface colors and marks packets identically. Re-exported so
// the definitions live exactly once (CLAUDE.md §9a) and yard consumers
// need no change.
import { isSim } from '@boss/web-kit/ui/packet-card';
// The server-computed read model. The approach lane's verdict rows are
// ITS lanes, not this lens's derivation — see [`approach`].
import type { YardStatus } from './yard-status';
export { isSim, PROTOCOL_PALETTE, protocolHue } from '@boss/web-kit/ui/packet-card';

export type TrainStatus = 'BOARDING' | 'BOARDED' | 'DEPARTED' | 'CONVERGING' | 'ARRIVED';
export type Lamp = 'green' | 'failing' | 'pending';

export type TrainRow = Readonly<{
  id: string;
  title: string;
  prUrl?: string | null;
  status: TrainStatus;
  lamp: Lamp;
  mergeRef?: string | null;
  deployed?: string | null;
  /** When the converge wait began (the deploy's instant), so the board
   *  can show an elapsed "converging for …". Non-null only while the
   *  train is CONVERGING — deployed, cluster not yet converged. */
  convergingSince?: string | null;
  cars: readonly CarRow[];
  live: boolean;
  /** Why the train closed — `unknown` for one still in flight. */
  outcome: TrainOutcome;
  /** The best evidence of when it arrived, and what that evidence was. */
  arrivedAt: ArrivalStamp;
  /** An estimate, or the phase alone when there is nothing honest to say. */
  eta: Eta;
  /** Non-null when the train is in trouble the board must show. */
  trouble: TrainTrouble | null;
  /** An operator's standing request that the conductor cancel this
   *  train (`metadata.cancel_requested`), read back off the Job so a
   *  reload shows the pending state. */
  cancelRequested: CancelRequest | null;
  /** The conductor stamped `cancel_refused` — the train had already
   *  merged when it looked. A refused request must never render as a
   *  pending one. */
  cancelRefused: boolean;
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

// Where the dock's rows came from: the station registry served, with
// the row's own facts, or it did not — and then the lane has NO
// reading. There is no third case. The dock's membership rule lives in
// the loading-dock row (the predicate, and since 36c3d4ca the
// `metadata_unmarked: ["hold"]` clause that drops a held car), and a
// copy of it here could not follow the row: a RELEASED hold is written
// `false`, not deleted, so absent-semantics get it backwards, and
// `stranded::marked` — the one definition of that rule — is Rust this
// file cannot call. So the client asks, and when the answer does not
// come it says so. The situation that made the old local derivation
// dangerous rather than merely duplicated: `StepMatch` is
// `deny_unknown_fields`, so an image rolled back past that clause
// cannot deserialize the row and `/queue` refuses — a stale copy would
// then list held cars as boardable, to somebody already mid-incident.
export type DockStation =
  | Readonly<{
      source: 'station';
      discipline: readonly string[];
      wipLimit: number | null;
      overLimit: boolean;
      total: number;
      upstream: StationUpstream | null;
    }>
  | Readonly<{ source: 'unavailable' }>;

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
 *  fact off the envelope, or null when the queue did not serve — no
 *  row read, so nothing honest to say about upstream. */
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
  /** Every OPEN car that names a branch, wherever it is. The floor keys
   *  a wagon by its car id so the same token slides from the gate bay
   *  to the dock to the train; a gating branch is matched to its car
   *  here, and only a branch with no car falls back to the gate packet. */
  cars: readonly CarRow[];
  /** The publish dock's queue, mapped 1:1 — branches asking to reach a
   *  gate. The upstream HALF of the approach that is not a gate verdict;
   *  the verdict lanes come from `/api/yard/status` and the two are
   *  composed by [`approach`], wherever both are held. Empty when the
   *  cluster cannot serve the station queue: additive, never a reason
   *  the yard fails to render. */
  publishing: readonly ApproachRow[];
  /** The raw packets the signals panel reads its stamps from — every
   *  train in the window (open and closed, as served: newest first)
   *  and the gate-runs. Held as fetched; nothing here is derived. */
  packets: Readonly<{ trains: readonly JobLite[]; gateRuns: readonly JobLite[] }>;
}>;

/** Where an inbound branch stands, ordered by distance from the dock.
 *  `held` is a gated-green car an operator is deliberately NOT parking
 *  — brake on, not forgotten. `gate-lost` is a run that died before any
 *  verdict: the gate exit, not the garage, because "we do not know" must
 *  read neither as rework nor as fine. */
export type ApproachState = 'publishing' | 'gated-red' | 'gate-lost' | 'gated-green' | 'held';

export type ApproachRow = Readonly<{
  /** The packet behind the row — a publish-request or gate-run Job.
   *  '' when the server sent a lane row without one (an older backend):
   *  the row still draws, it just opens nothing. */
  id: string;
  branch: string;
  /** The head the packet named, when it named one. */
  sha: string | null;
  state: ApproachState;
  /** When the packet opened: the station row's date on a publish row,
   *  and the lane's `since` — an RFC3339 instant when the gate-run
   *  stamped one, else the date — on a gate row. */
  opened_on: string;
  /** Requester on publish rows; nothing yet on gate rows. */
  note: string | null;
  /** The operator's reason for the hold — non-null exactly when `state`
   *  is `held`; a stock phrase when the marker carried no reason. */
  hold: string | null;
  /** The verdict as the gate recorded it (`green` / `failed` / `lost` /
   *  `unreadable`), null on a publish row. */
  verdict: string | null;
}>;

/** The verdict lanes of `/api/yard/status` — the only part of the status
 *  payload the approach reads. Named as exactly what it needs so the
 *  dependency is visible and a test can state four lanes instead of a
 *  whole read model. A `YardStatus` satisfies it structurally. */
export type ApproachLanes = Readonly<
  Pick<YardStatus, 'stranded' | 'held' | 'garage' | 'limbo'>
>;

/** The publish dock's own rows: branches asking to be published to the
 *  forge, which have not reached a gate yet. The station's queue IS the
 *  answer here — membership from the registry predicate, order from the
 *  declared discipline — so this maps 1:1 and judges nothing. */
export function publishRows(publishQueue: StationQueueEnvelope | null): readonly ApproachRow[] {
  return (publishQueue?.data ?? [])
    .filter(j => j.status === 'open')
    .map(j => {
      const md = (j.metadata ?? {}) as {
        branch?: string;
        head_sha?: string;
        requested_by?: string;
      };
      return {
        id: j.id,
        branch: md.branch ?? j.title,
        sha: md.head_sha ?? null,
        state: 'publishing' as const,
        opened_on: j.opened_on,
        note: md.requested_by ?? null,
        hold: null,
        verdict: null,
      };
    });
}

/** The approach to the dock — everything inbound that the dock's own
 *  queue cannot see yet: the publish requests, then the server's verdict
 *  lanes, ordered by distance from the dock.
 *
 *  THIS LENS JUDGES NOTHING. Which gate-runs are spent, green, held or
 *  red is `/api/yard/status`'s answer — `garage`, `limbo`, `stranded`,
 *  `held`, each computed in boss-jobs off the ONE marker list
 *  (`stranded::SPENT_MARKERS`) that four Rust readers already share.
 *  This function used to re-derive that partition from a window of
 *  gate-run packets, with its own notion of "spent" that knew only
 *  `superseded` — so a green stamped `rerailed_to` by `boss rerail`, or
 *  `park_skipped` by the auto-park handler, was correctly ignored by
 *  every Rust reader and drawn here as a stranded green anyway. On
 *  2026-09-10 feat/a-probe-declares-where-it-runs stood on the approach
 *  all day, on every browser, session and device, while the server
 *  reported `stranded: []`. CLAUDE.md §9a: the fact lived twice, so it
 *  drifted. It lives once now, and the only thing left here is ORDER.
 *
 *  ADDITIVE: no status (the endpoint is down, or has not answered yet)
 *  means the gate lanes are unknown, not empty-and-fine — so the
 *  approach shows what the station does say and the page still renders.
 *  A lane an older server does not send reads as empty the same way. */
export function approach(
  publishing: readonly ApproachRow[],
  status: ApproachLanes | null,
): readonly ApproachRow[] {
  const gateRow = (
    lane: Readonly<{ branch: string; packet_id: string; sha: string | null; since: string }>,
    state: ApproachState,
    verdict: string | null,
    hold: string | null = null,
  ): ApproachRow => ({
    id: lane.packet_id,
    branch: lane.branch,
    sha: lane.sha,
    state,
    opened_on: lane.since,
    note: null,
    hold,
    verdict,
  });
  // The garage lane is `failed` by definition — boss-jobs keeps a branch
  // there only on a verdict a check actually judged — and the limbo lane
  // carries the unjudged verdict it was settled with.
  const red = (status?.garage ?? []).map(g => gateRow(g, 'gated-red', 'failed'));
  const lost = (status?.limbo ?? []).map(l => gateRow(l, 'gate-lost', l.verdict));
  const green = (status?.stranded ?? []).map(s => gateRow(s, 'gated-green', 'green'));
  // Held last: a car with its brake on is the furthest from boarding.
  const held = (status?.held ?? []).map(h => gateRow(h, 'held', 'green', h.reason));
  return [...publishing, ...red, ...lost, ...green, ...held];
}

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
export function stampAt(s: StepLite | null): string | null {
  if (!s) return null;
  if (typeof s.completed_at === 'string' && s.completed_at !== '') return s.completed_at;
  const md = (s.metadata as { completed_at?: unknown } | null)?.completed_at;
  return typeof md === 'string' && md !== '' ? md : null;
}

/** The checks a gate receipt says did not pass, in the order it ran them.
 *
 *  TWO SHAPES, BOTH REAL. The gate runner used to reduce `infra/gate.sh`'s
 *  account of a run to `{verdict, head, mode, fails}` before reporting it
 *  — 101 characters on the packet — and everything else died with the
 *  pod. It now reports the whole receipt, whose `checks` array carries
 *  every check with its result AND its duration, and carries no derived
 *  `fails` beside it, because a summary living twice in one document is
 *  a fact that can drift (CLAUDE.md §9a).
 *
 *  Old receipts are on every landed car and are not going to be
 *  rewritten, so `fails` stays readable forever. `checks` wins when both
 *  are present: it is the primary record and `fails` was derived from it.
 *
 *  ONE definition, imported by every surface that renders a receipt —
 *  the failing-check line existed twice and would have silently gone
 *  blank in one of the two places when the receipt widened. */
export function failedChecks(receipt: unknown): readonly string[] {
  const r = receipt as { checks?: unknown; fails?: unknown } | null;
  if (Array.isArray(r?.checks)) {
    return r.checks
      .filter((c): c is { name: unknown; result: unknown } => typeof c === 'object' && c !== null)
      .filter(c => c.result !== 'pass')
      .map(c => c.name)
      .filter((n): n is string => typeof n === 'string');
  }
  return Array.isArray(r?.fails) ? r.fails.filter((f): f is string => typeof f === 'string') : [];
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

export type EtaPhase =
  | 'boarding'
  | 'ci'
  | 'merging'
  | 'deploying'
  | 'converging'
  | 'blocked'
  | 'arrived';

export function etaPhase(status: TrainStatus, lamp: Lamp): EtaPhase {
  switch (status) {
    case 'ARRIVED':
      return 'arrived';
    case 'CONVERGING':
      return 'converging';
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
  | Readonly<{
      kind: 'eta';
      phase: EtaPhase;
      atMs: number;
      basis: string;
      /** How far along the median leg under way the train is, 0–1 —
       *  the floor draws the locomotive this far between two signals.
       *  Clamped: an overdue train sits at the far signal, never past. */
      progress: number;
    }>;

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
    const elapsed = Math.max((nowMs - from) / 1000, 0);
    const left = Math.max(legS - elapsed, 0) + restS;
    const progress = legS > 0 ? Math.min(elapsed / legS, 1) : 1;
    return { kind: 'eta', phase, atMs: nowMs + Math.round(left * 1000), basis, progress };
  };

  // Converging renders an elapsed "converging for …", not an ETA: there
  // is no merge→converge median in `medians`, and inventing a converge
  // duration would read as a promise. The board reads `convergingSince`
  // (carried on the row by `toTrainRow`) for the elapsed instead — the
  // same choice the server model makes (boss-jobs/src/yard.rs, which
  // surfaces convergence as elapsed time, never a projection).
  if (phase === 'converging') return phaseOnly;

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

/** Why a train is in trouble, or `null` when it is simply moving.
 *
 * TROUBLE IS ORTHOGONAL TO PHASE, deliberately. `TrainStatus` answers
 * "how far along" and its four values have no way to say "and it has
 * been stuck there for six hours" — the comment on `splitAtDeparture`
 * even calls transit "boring, as transit should be". On 2026-09-02
 * that assumption broke in the open: a train sat at `converged` for
 * four hours with an urgent overdue packet already filed against it,
 * and rendered exactly like a healthy two-minute transit. Two more sat
 * at a step they could never complete, because a red PR does not
 * merge. David, seeing the board: *"1 stuck in transit still that is
 * pretending to be green when it should be in some sort of error
 * state."*
 *
 * Every signal here is one the conductor already wrote down. This
 * function invents no thresholds of its own — it surfaces alarms that
 * were raised elsewhere and were only ever visible in a packet nobody
 * had reason to open. An arrived or closed train is never troubled:
 * its history is not a live problem.
 */
export type TrainTrouble =
  | { readonly kind: 'ci-red' }
  | { readonly kind: 'converge-overdue' }
  | { readonly kind: 'stalled' };

export function trainTrouble(j: JobLite): TrainTrouble | null {
  const status = trainStatus(j);
  if (status === 'ARRIVED' || j.status === 'closed') return null;
  const md = (j.metadata ?? {}) as {
    converge_alarm_filed?: unknown;
    stalled_since?: unknown;
  };
  // The conductor filed an urgent packet about this train's
  // convergence and then had nowhere to show it.
  if (md.converge_alarm_filed === true || md.converge_alarm_filed === 'true')
    return { kind: 'converge-overdue' };
  // `note_stall` stamped it past the delivery policy's threshold.
  if (typeof md.stalled_since === 'string' && md.stalled_since !== '')
    return { kind: 'stalled' };
  // A returned red verdict is trouble until the train leaves — after
  // the merge the content has landed and the lamp is history.
  if (ciLamp(j) === 'failing' && status !== 'DEPARTED') return { kind: 'ci-red' };
  return null;
}

/** The badge text — short, because it sits beside the phase chip. */
export function troubleLabel(t: TrainTrouble): string {
  switch (t.kind) {
    case 'ci-red':
      return 'CI RED';
    case 'converge-overdue':
      return 'CONVERGE OVERDUE';
    case 'stalled':
      return 'STALLED';
  }
}

// Mirrors `phase_of` in boss-jobs/src/yard.rs — the authoritative
// server model. The `deployed` step completes in seconds, but the real
// ~10-minute wait is the `converged` step ("Cluster converged"). A
// deployed-but-unconverged train used to read as ARRIVED and vanish
// from the board mid-converge (2026-09-02); CONVERGING keeps it live.
export function trainStatus(j: JobLite): TrainStatus {
  if (j.status === 'closed') return 'ARRIVED';
  const deployedDone = done(step(j, 'deployed', 'Deployed to the playground'));
  const converged = step(j, 'converged', 'Cluster converged');
  const hasConverged = converged !== null;
  // ARRIVED when the terminal fired, OR a pre-converged workflow version
  // (no `converged` step — its finish line is `deployed`, so its absence
  // is arrival, not a stuck train), OR the cluster has converged.
  if (
    done(step(j, 'arrived', 'Train arrived')) ||
    (deployedDone && !hasConverged) ||
    (hasConverged && done(converged))
  )
    return 'ARRIVED';
  // Deployed, but the cluster has not converged on the merge yet.
  if (deployedDone) return 'CONVERGING';
  if (done(step(j, 'merged', 'Merged into main'))) return 'DEPARTED';
  if (done(step(j, 'pr', 'Open the batched PR'))) return 'BOARDED';
  return 'BOARDING';
}

/** The departure line is the MERGE (0bba59f7, ratified 2026-08-31):
 *  everything before it is revisable — repair pushes, re-signals,
 *  stall-cancel returning cars to the dock — so red there is work in
 *  progress, not a breakdown en route. Everything after it is
 *  irreversible and green by construction. This splits the open trains
 *  on that line so the page can render IN THE YARD (red = status)
 *  apart from DEPARTED / IN TRANSIT (boring, as transit should be).
 *  Red is NOT softened anywhere — the change is where red lives, not
 *  whether it shows. */
export function splitAtDeparture(trains: readonly TrainRow[]): Readonly<{
  inYard: readonly TrainRow[];
  inTransit: readonly TrainRow[];
}> {
  const departed = (t: TrainRow) =>
    t.status === 'DEPARTED' || t.status === 'CONVERGING' || t.status === 'ARRIVED';
  return {
    inYard: trains.filter(t => !departed(t)),
    inTransit: trains.filter(departed),
  };
}

// ---------------------------------------------------------------------
// The cancel request — the yard's one write (backlog 7a24caf3).
//
// Cancelling a red or stalled train used to be a classifier-gated CLI
// verb (`boss train cancel`). The page gains no verb of its own: it
// leaves a REQUEST STAMP on the train's Job through the metadata merge
// (`PATCH /api/jobs/{id}/metadata` merges top-level keys; a null value
// deletes one), and the conductor's reconcile — every ≤10 min — honours
// it on an open, not-yet-merged train: closes the PR, releases the cars
// to the dock, no strike. A train that had already merged is stamped
// `cancel_refused` instead. The effect is asynchronous by design, and
// the chip that replaces the button says so.
// ---------------------------------------------------------------------

/** The stamp. `by` is the viewer's employee id; `at` is RFC3339 UTC. */
export type CancelRequest = Readonly<{ by: string; reason: string; at: string }>;

/** Which side of the departure line a train block is rendered on — the
 *  page names it at the render site, because the snippet cannot know. */
export type YardPartition = 'in-yard' | 'in-transit';

/** The one role the page offers the button to. Affordance, not the
 *  gate: the gate is the API's `job:update`, which an audit-readonly
 *  guest does not hold. Spelled as `boss_core::roles::PLATFORM_ADMIN_ROLE`
 *  spells it. */
export const CANCEL_ROLE = 'platform-admin';

/** The exact PATCH body, or null when it would carry no reason or no
 *  actor. The reason is read by a human later ("why was this train
 *  pulled?"), so an empty one is refused here, before any request. */
export function cancelRequestBody(
  by: string,
  reason: string,
  at: string,
): Readonly<{ cancel_requested: CancelRequest }> | null {
  const trimmed = reason.trim();
  if (by === '' || trimmed === '') return null;
  return { cancel_requested: { by, reason: trimmed, at } };
}

/** The button appears only where every guard holds: the train is on
 *  the yard side of the departure line (post-merge is irreversible), it
 *  is in trouble the board already shows, the viewer holds the role,
 *  and nobody has asked already. */
export function canOfferCancel(
  row: TrainRow,
  partition: YardPartition,
  viewerPrivileged: boolean,
): boolean {
  return (
    partition === 'in-yard' &&
    row.trouble !== null &&
    viewerPrivileged &&
    row.cancelRequested === null
  );
}

function readCancelRequest(j: JobLite): CancelRequest | null {
  const v = (j.metadata as { cancel_requested?: unknown } | null)?.cancel_requested;
  if (typeof v !== 'object' || v === null) return null;
  const o = v as { by?: unknown; reason?: unknown; at?: unknown };
  const str = (x: unknown): string => (typeof x === 'string' ? x : '');
  return { by: str(o.by), reason: str(o.reason), at: str(o.at) };
}

/** Presence only: the refusal's shape belongs to the conductor. */
function readCancelRefused(j: JobLite): boolean {
  const v = (j.metadata as { cancel_refused?: unknown } | null)?.cancel_refused;
  return v !== undefined && v !== null;
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
  const status = trainStatus(j);
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
      head: car ? headOf(car) : null,
      // A car outside the window says nothing about its own proof, and
      // the arrivals stack must not read that silence as "not proven".
      proof: readCarProof(car),
    };
  });
  return {
    id: j.id,
    title: j.title,
    prUrl: ((pr?.metadata ?? {}) as { pr_url?: string }).pr_url ?? null,
    status,
    lamp: ciLamp(j),
    mergeRef: ((merged?.metadata ?? {}) as { merge_ref?: string }).merge_ref ?? null,
    deployed: ((deployed?.metadata ?? {}) as { deployed?: string }).deployed ?? null,
    // Converging began when the deploy landed — the honest start of the
    // wait. Only carried while the train is actually converging.
    convergingSince: status === 'CONVERGING' ? stampAt(deployed) : null,
    cars,
    live,
    outcome: trainOutcome(j),
    arrivedAt: arrivalStamp(j),
    eta: trainEta(j, medians, nowMs),
    trouble: trainTrouble(j),
    cancelRequested: readCancelRequest(j),
    cancelRefused: readCancelRefused(j),
  };
}

/** The first seven of a full sha, or null for anything that is not one. */
const shortSha = (v: unknown): string | null =>
  typeof v === 'string' && /^[0-9a-f]{7,40}$/i.test(v) ? v.slice(0, 7) : null;

/** The head a car names: `boarded_head` once the conductor boarded it,
 *  else the `head` inside the gate step's receipt (a JSON string the
 *  runner wrote). No record, no sha — the floor paints a dash. */
export function headOf(j: JobLite): string | null {
  const boarded = shortSha((j.metadata as { boarded_head?: unknown } | null)?.boarded_head);
  if (boarded) return boarded;
  const receipt = (step(j, 'gate', 'Gate')?.metadata as { receipt?: unknown } | null)?.receipt;
  if (typeof receipt !== 'string') return null;
  try {
    return shortSha((JSON.parse(receipt) as { head?: unknown }).head);
  } catch {
    return null;
  }
}

// One packet → one card, whoever chose the packet: the station
// envelope, the publish queue, the awaiting-proof set and the open-car
// set all map through here, so the card grammar cannot fork between
// lanes.
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
    head: headOf(j),
    proof: readCarProof(j),
  };
}

/** How many arrivals the board shows, and how many cancellations. */
export const ARRIVALS_SHOWN = 5;
export const CANCELLED_SHOWN = 3;

export function assembleYard(
  trains: readonly JobLite[],
  ships: readonly JobLite[],
  dockQueue: StationQueueEnvelope | null = null,
  nowMs: number = Date.now(),
  // LAST on purpose: 22 call sites pass `nowMs` as the fourth argument,
  // and inserting ahead of it would silently reinterpret a timestamp as
  // a report. Additive parameters go on the end.
  report: TerminalReport | null = null,
  gateRuns: readonly JobLite[] = [],
  publishQueue: StationQueueEnvelope | null = null,
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
    // The envelope is the ONLY source: membership came from the registry
    // predicate and order from the declared discipline — a client
    // re-sort would silently overrule the station row, so the rows map
    // 1:1 in server order. No envelope, no rows: the lane renders
    // "cannot be read" off `dockStation`, which is the honest answer
    // and the one a stale local predicate could not give.
    dock: dockQueue ? dockQueue.data.map(carRow) : [],
    dockStation: dockQueue
      ? {
          source: 'station',
          discipline: dockQueue.discipline,
          wipLimit: dockQueue.wip_limit ?? null,
          overLimit: dockQueue.over_limit,
          total: dockQueue.total,
          upstream: dockQueue.upstream ?? null,
        }
      : { source: 'unavailable' },
    arrivals: arrived
      .slice(0, ARRIVALS_SHOWN)
      .map(t => toTrainRow(t, shipById, false, medians, nowMs)),
    cancelled: closed
      .filter(c => c.outcome !== 'arrived')
      .slice(0, CANCELLED_SHOWN)
      .map(c => toTrainRow(c.t, shipById, false, medians, nowMs)),
    delivery: deliveryStats(report),
    awaitingProof: awaitingProof(ships).map(carRow),
    publishing: publishRows(publishQueue),
    packets: { trains, gateRuns },
    cars: ships
      .filter(j => j.status === 'open')
      .map(carRow)
      .filter(c => c.branch !== ''),
  };
}

// The dock's station row, or null when the cluster can't serve one —
// 404 (no such station), 503 (registry not configured), a network
// fault, or a 200 that isn't the envelope all mean the same thing: no
// reading. Never an error that fails the page; the lane that depends
// on it says it cannot see, and the rest of the yard renders whole.
async function fetchStationQueue(name: string): Promise<StationQueueEnvelope | null> {
  try {
    const r = await fetch(`/api/stations/${name}/queue`);
    if (!r.ok) return null;
    const env = (await r.json()) as StationQueueEnvelope;
    return Array.isArray(env?.data) && Array.isArray(env?.discipline) ? env : null;
  } catch {
    return null;
  }
}

export async function fetchYard(): Promise<YardState | null> {
  const [tr, sr, dockQueue, report, gateRuns, publishQueue] = await Promise.all([
    // 40, not 20: the window has to hold the open trains, the five
    // arrivals the board shows, AND the arrivals the ETA medians are
    // taken over — cancelled trains sit in the same list and would
    // otherwise crowd the samples out.
    fetch('/api/jobs?kind=pr-train&limit=40'),
    fetch('/api/jobs?kind=ship-a-change&limit=200'),
    fetchStationQueue('loading-dock'),
    // The scoreboard is ADDITIVE: a yard that cannot show its stats is
    // still a yard, so this resolves to null rather than failing the
    // whole page. The stats are the thing you read second; the trains
    // are the thing you came for.
    fetch('/api/workflows/ship-a-change/terminal-report')
      .then((r) => (r.ok ? (r.json() as Promise<TerminalReport>) : null))
      .catch(() => null),
    // The gate-run window, additive the same way. The approach lane no
    // longer reads it — that is `/api/yard/status`'s four lanes — so
    // these packets are held for the SIGNALS panel's stamps alone; 60 is
    // two days of heavy gating, which is the window a signal reads over.
    fetch('/api/jobs?kind=gate-run&limit=60')
      .then((r) => (r.ok ? (r.json() as Promise<{ data?: JobLite[] }>) : null))
      .then((b) => b?.data ?? [])
      .catch(() => [] as JobLite[]),
    fetchStationQueue('publish-dock'),
  ]);
  if (!tr.ok || !sr.ok) return null;
  const trains = ((await tr.json()) as { data?: JobLite[] }).data ?? [];
  const ships = ((await sr.json()) as { data?: JobLite[] }).data ?? [];
  return assembleYard(trains, ships, dockQueue, Date.now(), report, gateRuns, publishQueue);
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

/**
 * Cycle medians carry sub-day precision now that packets are stamped
 * with precise open/close instants: under a day reads in hours, a day
 * or longer in days, both to one decimal. A same-day close used to
 * render `0d`, which hid exactly the improvement the scoreboard
 * exists to show.
 */
function days(v: TerminalVersion): string {
  const m = v.cycle_time_days?.median;
  if (m === null || m === undefined) return '—';
  const round1 = (n: number) => Math.round(n * 10) / 10;
  const hours = m * 24;
  // Sub-hour medians in MINUTES (b4c1b53a): at today's cadence a cycle
  // is often under an hour, and `0.8h` makes the reader do arithmetic
  // the scoreboard exists to have already done.
  if (hours < 1) return `${Math.round(hours * 60)}m`;
  // The day boundary compares the ROUNDED hours: 23.98h displays as
  // 24h, and "24h" reads as a day, not as hours (the pin this line
  // briefly broke on the way in).
  const h1 = round1(hours);
  return h1 < 24 ? `${h1}h` : `${round1(m)}d`;
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
