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
// A CHANNEL IS A FACT WHERE ONE WAS RECORDED. `metadata.channel` naming
// a channel is read as recorded; everything else is DERIVED from the
// kind and the reporter, and says so (`basis`), the way the yard's
// production panel says whether a number came from the record or from
// a window. A backlog item naming no source is `unrecorded` — never
// guessed into a lane, because the whole reason this page exists is to
// make "where does our work come from" a fact instead of a feeling.
//
// NO NUMBER THIS SURFACE MAKES UP. A page that truncated (`total` past
// the rows read) is reported as such by the page; a failed read is a
// failure, never an empty track.

import { isHumanActor } from '../../data/actor';
import { fetchRemote, type Remote } from '../../data/remote';

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
// Channel
// ---------------------------------------------------------------------

export type Channel = 'feedback' | 'monitoring' | 'session' | 'design' | 'protocol' | 'unrecorded';

export const CHANNELS: ReadonlyArray<Channel> = [
  'feedback',
  'monitoring',
  'session',
  'design',
  'protocol',
  'unrecorded',
];

export const CHANNEL_LABEL: Readonly<Record<Channel, string>> = {
  feedback: 'Feedback · a person wrote it',
  monitoring: 'Monitoring · alarms and the observer',
  session: 'Sessions · what a builder found',
  design: 'Design queue',
  protocol: 'Protocol steps',
  unrecorded: 'Channel unrecorded',
};

const isChannel = (v: unknown): v is Channel =>
  typeof v === 'string' && (CHANNELS as ReadonlyArray<string>).includes(v);

const DESIGN_KINDS: ReadonlySet<string> = new Set([
  'design-doc',
  'design-doc-review',
  'workflow-design',
  'protocol-retro',
  'protocol-experiment',
]);
const PROTOCOL_KINDS: ReadonlySet<string> = new Set([
  'rotate-a-credential',
  'publish-to-github',
  'approval',
  'join-a-node',
]);
const MONITORING_KINDS: ReadonlySet<string> = new Set(['incident', 'incident-post-mortem']);

export type ChannelReading = Readonly<{ channel: Channel; basis: 'recorded' | 'derived' }>;

/** The recorded channel if the packet carries one; otherwise the rule. */
export function channelOf(job: unknown): ChannelReading {
  const j = (job ?? {}) as Record<string, unknown>;
  const md = (j.metadata ?? {}) as Record<string, unknown>;
  if (isChannel(md.channel)) return { channel: md.channel, basis: 'recorded' };
  const kind = str(j.kind) ?? '';
  const derived = (channel: Channel): ChannelReading => ({ channel, basis: 'derived' });
  if (kind === 'user-feedback') return derived('feedback');
  if (MONITORING_KINDS.has(kind)) return derived('monitoring');
  if (DESIGN_KINDS.has(kind)) return derived('design');
  if (PROTOCOL_KINDS.has(kind)) return derived('protocol');
  // Backlog items name their source in several keys or not at all —
  // the reading the prototype surfaced. Read them in order, then
  // classify the actor: a machine reporter is monitoring, anything
  // else that wrote a source is a session.
  const source =
    str(md.reporter) ?? str(md.filed_by) ?? str(md.source) ?? str(md.submitted_by) ?? null;
  if (source === null) return derived('unrecorded');
  const s = source.toLowerCase();
  if (s.startsWith('automation:') || s.startsWith('cadence') || s === 'conductor') {
    return derived('monitoring');
  }
  return derived('session');
}

// ---------------------------------------------------------------------
// Rows — one parse at the fetch site
// ---------------------------------------------------------------------

export type ReadyStep = Readonly<{ kind: string; who: string | null }>;

export type InboundRow = Readonly<{
  id: string;
  kind: string;
  title: string;
  status: 'open' | 'closed';
  openedOn: string;
  closedOn: string | null;
  priority: string;
  channel: Channel;
  channelBasis: 'recorded' | 'derived';
  ready: ReadonlyArray<ReadyStep>;
}>;

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
    const ch = channelOf(r);
    return [
      {
        id,
        kind,
        title: str(r.title) ?? '',
        status: r.status === 'closed' ? 'closed' : 'open',
        openedOn,
        closedOn: str(r.closed_on),
        priority: str(r.priority) ?? 'standard',
        channel: ch.channel,
        channelBasis: ch.basis,
        ready: steps
          .filter((s) => s.status === 'ready')
          .map((s) => ({ kind: str(s.kind) ?? '', who: str(s.assignee_id) })),
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
  byChannel: Readonly<Partial<Record<Channel, number>>>;
  /** Closed that day — including packets that arrived before the window. */
  left: number;
}>;

export function arrivalsByDay(
  rows: ReadonlyArray<InboundRow>,
  days: ReadonlyArray<string>,
): ReadonlyArray<DayFlow> {
  return days.map((day) => {
    const arrived = rows.filter((r) => r.openedOn === day);
    const byChannel = arrived.reduce<Partial<Record<Channel, number>>>(
      (acc, r) => ({ ...acc, [r.channel]: (acc[r.channel] ?? 0) + 1 }),
      {},
    );
    return {
      day,
      arrived: arrived.length,
      byChannel,
      left: rows.filter((r) => r.closedOn === day).length,
    };
  });
}

export type WaitingRow = InboundRow &
  Readonly<{ age: number; band: AgeBand; holder: Holder }>;

/** Every open packet, oldest first. */
export function waiting(rows: ReadonlyArray<InboundRow>, today: string): ReadonlyArray<WaitingRow> {
  return rows
    .filter((r) => r.status === 'open')
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
  const feedback = standing.filter((r) => r.channel === 'feedback');
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
      count: arrived.filter((r) => r.channel === 'unrecorded').length,
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
 *  window. Real work only — the demo tenant's packets are not inbound. */
export function loadKind(
  kind: string,
  windowDays: number,
  limit: number,
): Promise<Exclude<Remote<JobsPage>, { kind: 'loading' }>> {
  return fetchRemote(
    `/api/jobs?kind=${encodeURIComponent(kind)}&simulated=false&closed_within=${windowDays}&limit=${limit}`,
    parseJobsPage,
  );
}
