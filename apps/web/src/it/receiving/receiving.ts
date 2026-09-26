// The Receiving Yard's read model — the INBOUND third of the operator
// surface, upstream of the Marshalling Yard and the Crew Board.
//
// In a rail yard the receiving yard is where inbound trains stand
// before anything is sorted: this page is every packet that ASKS the
// platform for something — feedback, an alarm, a finding a session
// filed, a design question, a protocol step — from the moment it is
// opened until an actor takes it into a build, a review or a decision.
// David, 2026-09-12: "We haven't fixed up visualization upstream of the
// Crew Board that helps us see inbound jobs better." The prototype this
// ports ran on a live read and found eight of his own feedback packets
// standing, the oldest 21 days, and 67 of a week's 267 arrivals naming
// no channel at all.
//
// WHAT COUNTS AS INBOUND is derived from the workflow registry, not
// listed here by kind: a platform-category kind that is neither a chore
// (`maintenance-*`) nor the delivery pipeline. The pipeline set below
// is the one list this file keeps, and it is short because the
// pipeline's kinds are the ones the Train Yard already shows.
//
// A LANE IS WHAT THE SERVER READ, AND ONLY WHAT THE FILER RECORDED.
// Each row carries `lane: {lane, basis}` because this page asks the list
// for it (`lane=true`): the server reads the lane the filer recorded, in
// the one vocabulary (`boss_jobs::channels::lane_of`), and a packet whose
// filer named none is `unclassified` — never guessed into a lane, because
// the whole reason this page exists is to make "where does our work come
// from" a fact instead of a feeling. Until backlog 1eea4554 this file
// kept a six-lane vocabulary and three kind lists of its own and read a
// key no filer writes, so 0 of 1,626 arrivals read as recorded while 492
// of them had recorded one; `regions/receiving.rs` pins that no rule
// grows back here.
//
// NO NUMBER THIS SURFACE MAKES UP. A page that truncated (`total` past
// the rows read) is reported as such by the page; a failed read is a
// failure, never an empty track.

import { isHumanActor } from '../../data/actor';
import { fetchRemote, type Remote } from '../../data/remote';
import { failedVerb, type FailedVerb } from '../../steps/failedVerb';

// ---------------------------------------------------------------------
// Which kinds are inbound
// ---------------------------------------------------------------------

/** The delivery pipeline: a packet of these kinds is a car in transit or
 *  the machinery moving it, never a request. The Train Yard shows them. */
export const PIPELINE_KINDS: ReadonlySet<string> = new Set([
  'pr-train',
  'gate-run',
  'ship-a-change',
  'ops-request',
  'park-a-job',
  'emergency-merge',
  'repair-a-train',
  'publish-request',
  'regenerate-deployment',
]);

export type WorkflowRow = Readonly<{ kind: string; category: string; status: string }>;

const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);

export function parseWorkflows(raw: unknown): ReadonlyArray<WorkflowRow> {
  const list = Array.isArray(raw)
    ? raw
    : Array.isArray((raw as { data?: unknown } | null)?.data)
      ? ((raw as { data: unknown[] }).data)
      : [];
  return list.flatMap((w) => {
    const r = w as Record<string, unknown>;
    const kind = str(r.kind);
    return kind
      ? [{ kind, category: str(r.category) ?? '', status: str(r.status) ?? 'active' }]
      : [];
  });
}

/** Active platform kinds that are neither chores nor the pipeline, sorted. */
export function inboundKinds(workflows: ReadonlyArray<WorkflowRow>): ReadonlyArray<string> {
  return [
    ...new Set(
      workflows
        .filter((w) => w.status === 'active' && w.category === 'platform')
        .map((w) => w.kind)
        .filter((k) => !k.startsWith('maintenance-') && !PIPELINE_KINDS.has(k)),
    ),
  ].sort();
}

// ---------------------------------------------------------------------
// Lane — the server's reading, drawn as it came
// ---------------------------------------------------------------------

export type LaneBasis = 'recorded' | 'unclassified';

export type LaneReading = Readonly<{ lane: string; basis: LaneBasis }>;

/** What the server answers for a packet whose filer recorded no lane. */
export const UNCLASSIFIED: LaneReading = { lane: 'unclassified', basis: 'unclassified' };

/** The `lane` the server put on a listed row. The read asks for it, and a
 *  server that cannot answer `lane=true` refuses the whole read (400),
 *  so a row without one is not something this page's read returns; it
 *  is drawn unclassified rather than dropped, because a lost packet
 *  would shrink every count on the page. */
export function laneOf(row: unknown): LaneReading {
  const l = ((row ?? {}) as { lane?: unknown }).lane as Record<string, unknown> | undefined;
  const lane = str(l?.lane);
  return lane !== null && l?.basis === 'recorded' ? { lane, basis: 'recorded' } : UNCLASSIFIED;
}

/** The lanes to draw: every recorded lane the rows carry, in the
 *  server's spelling and alphabetical, then `unclassified` — always,
 *  because it is the reading this page exists to make, and an empty
 *  track there is an answer. */
export function lanesOf(rows: ReadonlyArray<InboundRow>): ReadonlyArray<string> {
  const recorded = [
    ...new Set(rows.filter((r) => r.laneBasis === 'recorded').map((r) => r.lane)),
  ].sort();
  return [...recorded, UNCLASSIFIED.lane];
}

/** A hue per lane, by the lane's place in [`lanesOf`], so one reading
 *  paints a lane alike in the chart, the tracks and the manifest. Ten,
 *  for the ten lanes a filer may name; `unclassified` is the faint one,
 *  because it is the absence of an answer. */
const LANE_COLORS: ReadonlyArray<string> = [
  'var(--signal)',
  'var(--err)',
  'var(--warn)',
  'var(--ok)',
  'var(--busy)',
  'var(--map-line-publish)',
  'var(--map-line-tenant)',
  'var(--fog)',
  'var(--map-line-delivery)',
  'var(--map-line-siding)',
];

export function laneColor(lane: string, lanes: ReadonlyArray<string>): string {
  const i = lanes.indexOf(lane);
  return lane === UNCLASSIFIED.lane || i < 0
    ? 'var(--text-faint)'
    : (LANE_COLORS[i % LANE_COLORS.length] ?? 'var(--text-faint)');
}

/** The kind a person files through the feedback door. The feedback
 *  reading counts it by KIND, not by lane: measured 2026-09-26, 17 of the
 *  19 user-feedback packets in the window recorded no lane, and reading a
 *  lane off the kind is the rule this file stopped keeping. */
export const FEEDBACK_KIND = 'user-feedback';

// ---------------------------------------------------------------------
// Rows — one parse at the fetch site
// ---------------------------------------------------------------------

/** A ready step, and — when the ops verb it waits on FAILED — the
 *  note the dispatcher left on it (backlog 074e1287): the step is
 *  still ready, the packet is troubled, and the manifest must say so. */
export type ReadyStep = Readonly<{ kind: string; who: string | null; failed: FailedVerb | null }>;

export type InboundRow = Readonly<{
  id: string;
  kind: string;
  title: string;
  status: 'open' | 'closed';
  openedOn: string;
  closedOn: string | null;
  priority: string;
  /** The lane the server read ([`laneOf`]). */
  lane: string;
  laneBasis: LaneBasis;
  ready: ReadonlyArray<ReadyStep>;
  /** An actor has picked it up: a step other than its trigger has
   *  completed ([`takenIn`]). From then on it is marshalling's, not
   *  standing here (design 62de32ae decision 4). */
  takenIn: boolean;
}>;

/** THE INTAKE RULE — `regions.rs::taken_in`, the server's partition
 *  between receiving and marshalling, read the same way here so the
 *  receiving floor stands what the region's header counts. The intake
 *  step is the first step after the trigger, read off the step kinds:
 *  a `triage`, a `decide`, whatever a protocol put first. The trigger
 *  completes at admission, so it takes nothing in; a skipped step is
 *  not a completed one. Measured 2026-09-24: 216 of 232 open
 *  backlog-items were triaged and waiting on `build`, and this board
 *  stood every one of them. */
export const TRIGGER_STEP_KIND = 'trigger';

export function takenIn(steps: ReadonlyArray<Readonly<Record<string, unknown>>>): boolean {
  return steps.some((s) => s.status === 'completed' && s.kind !== TRIGGER_STEP_KIND);
}

export type JobsPage = Readonly<{ rows: ReadonlyArray<InboundRow>; total: number }>;

export function parseJobsPage(raw: unknown): JobsPage {
  const env = raw as { data?: unknown; total?: unknown } | null;
  const data = Array.isArray(env?.data) ? (env.data as ReadonlyArray<unknown>) : [];
  const total = typeof env?.total === 'number' ? env.total : 0;
  const rows = data.flatMap((j): InboundRow[] => {
    const r = j as Record<string, unknown>;
    const id = str(r.id);
    const kind = str(r.kind);
    const openedOn = str(r.opened_on);
    if (!id || !kind || !openedOn) return [];
    const steps = Array.isArray(r.steps) ? (r.steps as ReadonlyArray<Record<string, unknown>>) : [];
    const ln = laneOf(r);
    return [
      {
        id,
        kind,
        title: str(r.title) ?? '',
        status: r.status === 'closed' ? 'closed' : 'open',
        openedOn,
        closedOn: str(r.closed_on),
        priority: str(r.priority) ?? 'standard',
        lane: ln.lane,
        laneBasis: ln.basis,
        ready: steps
          .filter((s) => s.status === 'ready')
          .map((s) => ({
            kind: str(s.kind) ?? '',
            who: str(s.assignee_id),
            failed: failedVerb(s.metadata),
          })),
        takenIn: takenIn(steps),
      },
    ];
  });
  return { rows, total };
}

// ---------------------------------------------------------------------
// Holder, age, band
// ---------------------------------------------------------------------

export type Holder = Readonly<{ who: 'agent' | 'human' | 'nobody'; label: string }>;

/** Who the packet waits on. A person on any ready step outranks the
 *  agent: the packet is standing for THEIR answer. */
export function holderOf(row: InboundRow): Holder {
  if (row.ready.length === 0) return { who: 'nobody', label: 'no ready step' };
  const human = row.ready.find((s) => s.who !== null && isHumanActor(s.who));
  if (human?.who) return { who: 'human', label: human.who };
  const agent = row.ready.find((s) => s.who !== null);
  if (agent) return { who: 'agent', label: 'the agent' };
  return { who: 'nobody', label: 'unassigned' };
}

/** The first ready step whose verb FAILED, or null for a packet with
 *  none: what the manifest's troubled reading and the track's car
 *  title both read. Measured 2026-09-19: publish 254177e2's open-pr
 *  sat annotated for hours and this yard drew a healthy publish. */
export function failedStep(row: InboundRow): ReadyStep | null {
  return row.ready.find((s) => s.failed !== null) ?? null;
}

const DAY_MS = 86_400_000;
const utc = (day: string): number => Date.parse(`${day}T00:00:00Z`);

export function ageDays(openedOn: string, today: string): number {
  return Math.max(0, Math.round((utc(today) - utc(openedOn)) / DAY_MS));
}

/** The Marshalling Yard's proposed thresholds, shown here until the
 *  packet carries its own: triage within 3 days, anything within 14. */
export const AGE_THRESHOLDS = { aging: 3, stale: 14 } as const;

export type AgeBand = 'fresh' | 'aging' | 'stale';

export function ageBand(days: number): AgeBand {
  if (days > AGE_THRESHOLDS.stale) return 'stale';
  if (days > AGE_THRESHOLDS.aging) return 'aging';
  return 'fresh';
}

/** ISO days, oldest first, the last `n` ending on `today`. */
export function daysEndingOn(today: string, n: number): ReadonlyArray<string> {
  return Array.from({ length: n }, (_, i) =>
    new Date(utc(today) - (n - 1 - i) * DAY_MS).toISOString().slice(0, 10),
  );
}

// ---------------------------------------------------------------------
// The two views: arrivals per day, and what is standing
// ---------------------------------------------------------------------

export type DayFlow = Readonly<{
  day: string;
  arrived: number;
  byLane: Readonly<Record<string, number>>;
  /** Closed that day — including packets that arrived before the window. */
  left: number;
}>;

export function arrivalsByDay(
  rows: ReadonlyArray<InboundRow>,
  days: ReadonlyArray<string>,
): ReadonlyArray<DayFlow> {
  return days.map((day) => {
    const arrived = rows.filter((r) => r.openedOn === day);
    const byLane = arrived.reduce<Record<string, number>>(
      (acc, r) => ({ ...acc, [r.lane]: (acc[r.lane] ?? 0) + 1 }),
      {},
    );
    return {
      day,
      arrived: arrived.length,
      byLane,
      left: rows.filter((r) => r.closedOn === day).length,
    };
  });
}

export type WaitingRow = InboundRow &
  Readonly<{ age: number; band: AgeBand; holder: Holder }>;

/** A packet standing in receiving: open, and not yet taken in. */
export const standsHere = (r: InboundRow): boolean => r.status === 'open' && !r.takenIn;

/** Every packet standing in receiving, oldest first. */
export function waiting(rows: ReadonlyArray<InboundRow>, today: string): ReadonlyArray<WaitingRow> {
  return rows
    .filter(standsHere)
    .map((r) => {
      const age = ageDays(r.openedOn, today);
      return { ...r, age, band: ageBand(age), holder: holderOf(r) };
    })
    .sort((a, b) => b.age - a.age || a.kind.localeCompare(b.kind));
}

// ---------------------------------------------------------------------
// What the snapshot says
// ---------------------------------------------------------------------

export type Readings = Readonly<{
  feedbackStanding: Readonly<{ count: number; oldestDays: number }>;
  onOneActor: Readonly<{ count: number; of: number; actor: string }>;
  unrecorded: Readonly<{ count: number; of: number }>;
}>;

export function readings(
  rows: ReadonlyArray<InboundRow>,
  today: string,
  days: ReadonlyArray<string>,
): Readings {
  const standing = waiting(rows, today);
  const feedback = standing.filter((r) => r.kind === FEEDBACK_KIND);
  const holders = standing.map((r) => r.holder.label);
  const top = [...new Set(holders)]
    .map((label) => ({ label, n: holders.filter((h) => h === label).length }))
    .sort((a, b) => b.n - a.n)[0];
  const window = new Set(days);
  const arrived = rows.filter((r) => window.has(r.openedOn));
  return {
    feedbackStanding: {
      count: feedback.length,
      oldestDays: feedback.reduce((m, r) => Math.max(m, r.age), 0),
    },
    onOneActor: { count: top?.n ?? 0, of: standing.length, actor: top?.label ?? '—' },
    unrecorded: {
      count: arrived.filter((r) => r.laneBasis === 'unclassified').length,
      of: arrived.length,
    },
  };
}

// ---------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------

/** The registry: which kinds exist and what category each is. */
export function loadWorkflows(): Promise<
  Exclude<Remote<ReadonlyArray<WorkflowRow>>, { kind: 'loading' }>
> {
  return fetchRemote('/api/workflows', parseWorkflows);
}

/** One kind's packets: everything open, plus what closed inside the
 *  window. Real work only — the demo tenant's packets are not inbound.
 *  `lane=true` asks the server to put its lane reading on each row. */
export function loadKind(
  kind: string,
  windowDays: number,
  limit: number,
  offset = 0,
): Promise<Exclude<Remote<JobsPage>, { kind: 'loading' }>> {
  return fetchRemote(
    `/api/jobs?kind=${encodeURIComponent(kind)}&simulated=false&closed_within=${windowDays}&lane=true&limit=${limit}&offset=${offset}`,
    parseJobsPage,
  );
}

/** EVERY packet of one kind in the window, page after page until the
 *  server's `total` — a limit is not a filter (design 62de32ae
 *  decision 4). This board read one page of 500 and, measured
 *  2026-09-24, backlog-item had 822 in the window: every count below
 *  it was a floor, said in a notice nobody reads as a number. A row a
 *  shifting page repeats is kept once. Any page failing fails the
 *  whole read — half a kind is not a kind. */
export async function loadEveryPage(
  kind: string,
  windowDays: number,
  pageSize: number,
  load: typeof loadKind = loadKind,
): Promise<Exclude<Remote<JobsPage>, { kind: 'loading' }>> {
  const read = async (
    offset: number,
    got: ReadonlyArray<InboundRow>,
  ): Promise<Exclude<Remote<JobsPage>, { kind: 'loading' }>> => {
    const page = await load(kind, windowDays, pageSize, offset);
    if (page.kind === 'failed') return page;
    const seen = new Set(got.map((r) => r.id));
    const rows = [...got, ...page.data.rows.filter((r) => !seen.has(r.id))];
    const next = offset + page.data.rows.length;
    return page.data.rows.length === 0 || next >= page.data.total
      ? { kind: 'ready', data: { rows, total: page.data.total } }
      : read(next, rows);
  };
  return read(0, []);
}
