// The design-review lens — /it/design as a view onto a station's
// queue rather than a page with its own idea of what that queue is.
//
// Origin (David, feedback 3f5f7f63): "the Design Review page should
// really just be a custom view onto a particular queue or set of
// queues. That is what many of our pages fundamentally devolve into.
// Here is what is in queue, presented usefully, with context about how
// that queue has been flowing recently."
//
// WHAT CHANGED, AND WHY IT IS NOT COSMETIC
// The page used to define its own queue in the browser —
// `/api/jobs?kind=design-doc-review&status=open` filtered client-side —
// while the `design-review` station defined the same queue as a
// predicate the server evaluates. A client filter and a station's
// evaluated queue are two different definitions of one queue, and they
// drift silently: a packet the station holds can be missing from the
// page that exists to show it. Reading `GET /api/stations/
// design-review/queue` makes the registry row the single definition.
//
// The page's own identity (header, which panels it carries) comes from
// the same envelope, off `stations.lens` — the shape `upstream` proved
// in 119: the row declares it, the queue echoes it, the lens renders
// whatever the row says.

import { readEnvelope } from '../../data/shape';

/** Page context declared on the station row (`stations.lens`,
 *  138-station-lens.sql; `StationLens` in boss-jobs). Optional on the
 *  wire — a cluster whose registry predates the column omits it. */
export type StationLens = Readonly<{
  eyebrow?: string | null;
  title: string;
  subtitle?: string | null;
  /** Renderer keys, in render order. Plain strings for the same reason
   *  discipline keys are: the surface renders the vocabulary the
   *  registry declares. */
  panels?: readonly string[];
}>;

/** A packet as the station queue serves it. Its steps ride beside it
 *  in the envelope's `steps` map, not on it (see `LensStep`).
 *
 *  No `subject`, though the envelope carries one. Nothing on this page
 *  reads it any more — see `queueRows` for the join that used to and
 *  why it could never have worked. */
export type QueuePacket = Readonly<{
  id: string;
  title: string;
  status: string;
  opened_on: string;
  /** Set on a closed packet — the settled rows' date. */
  closed_on?: string | null;
  /** `outcome` is stamped here when a terminal step closes the packet. */
  metadata?: unknown;
}>;

/** A member's step, as the envelope's `steps` map carries it — only
 *  the fields this lens reads. */
export type LensStep = Readonly<{
  id: string;
  kind: string;
  spec_slug?: string | null;
  status: string;
  completed_on?: string | null;
  metadata?: unknown;
}>;

/** The `GET /api/stations/{name}/queue` envelope, design-review's
 *  slice of it. Same struct yard.ts reads — kept as its own type here
 *  rather than imported so one lens cannot break another by widening
 *  what it needs.
 *
 *  `steps` is keyed by job id and present only when the station's lens
 *  declares `with_steps` — design-review since v2 and design-decided
 *  (backlog 08372fdb). A registry still at design-review v1 omits it,
 *  and every row then reads its progress as unread rather than as
 *  untouched. */
export type DesignQueueEnvelope = Readonly<{
  station: string;
  discipline: readonly string[];
  lens?: StationLens | null;
  terminal_window_days?: number | null;
  total: number;
  data: readonly QueuePacket[];
  steps?: Readonly<Record<string, readonly LensStep[]>>;
}>;

/** How far a review has got, read off the review step itself.
 *
 *  WHY NOT THE STEP STATUS (backlog 08372fdb). A review saved but not
 *  completed used to render exactly like one never touched. Status
 *  cannot tell them apart: the review surface's Save moves a step only
 *  from `pending`, and a review in this queue is already `ready`, so it
 *  STAYS `ready` with answers on it — measured 2026-09-24, every open
 *  review was `ready`. The answers are the trace, so they decide it;
 *  `active` still counts, because someone moved it on purpose.
 *
 *  `unread` = no steps on the wire, which is not the same claim as
 *  "not started" and is not rendered as one. */
export type ReviewProgress =
  | Readonly<{ kind: 'unread' }>
  | Readonly<{ kind: 'untouched'; asked: number }>
  | Readonly<{ kind: 'saved'; answered: number; asked: number }>;

/** An open review packet as the queue panel renders it.
 *
 *  No `status` (backlog 84d97547): the station admits only open
 *  packets, so it was one constant, and the column that printed it
 *  went with it. What a row's review has got to is `progress`. */
export type ReviewPacket = Readonly<{
  id: string;
  opened_on: string;
  title: string;
  progress: ReviewProgress;
  /** The review step's id when the envelope carried it — the Review
   *  button then needs no read of its own. */
  reviewStepId: string | null;
}>;

// The header this page rendered as literals before the registry
// carried it. Kept as the fallback rather than deleted: a cluster
// running a binary newer than its schema (the ordinary state during a
// rollout, and the permanent state of any install that has not taken
// 138) must render a named page, not an untitled one.
export const FALLBACK_HEADER = {
  eyebrow: 'System Model · Design review',
  title: 'Design review',
  subtitle: 'Open questions and ADRs',
} as const;

export type PageHeader = Readonly<{
  eyebrow: string;
  title: string;
  subtitle: string;
}>;

/** The page's header, from the row when it declares one. A declared
 *  lens with a blank title is treated as undeclared — an empty
 *  heading is worse than the literal it replaced. */
export function pageHeader(lens: StationLens | null | undefined): PageHeader {
  const title = lens?.title?.trim();
  if (!title) return FALLBACK_HEADER;
  return {
    eyebrow: lens?.eyebrow?.trim() || FALLBACK_HEADER.eyebrow,
    title,
    subtitle: lens?.subtitle?.trim() || '',
  };
}

/** Panel renderers this surface ships, in the order they read when the
 *  registry declares nothing.
 *
 *  There were three, and `queue` is what is left. `rejections` (the
 *  indexer's refusals plus the drifted-status report) went on
 *  2026-09-10 with the write-back half of the tracker; `corpus` (the
 *  table of markdown files under docs/design/) went the same day with
 *  the read half — the corpus index, its parser and the service that
 *  served them (backlog f5da586c). Both panels described FILES. Under
 *  "the packet is the doc" the thing worth rendering is the station's
 *  queue, which this page was already fetching and using only for a
 *  join that could never match: a `design-doc` packet's subject is
 *  `boss-platform`, never a doc path, so `reviewsByDocPath` keyed
 *  nothing and the live packets were invisible on the page that
 *  exists to show them.
 *
 *  `decided` joined on 2026-09-24 (backlog 08372fdb): the page's
 *  WORKING and OUT — designs decided and folding, and designs settled
 *  inside the `design-decided` station's window. Until then a design
 *  left the page the moment its review completed. */
export const KNOWN_PANELS = ['queue', 'decided'] as const;
export type PanelKey = (typeof KNOWN_PANELS)[number];

/** Which panels to render, in the row's declared order.
 *
 *  A key this build does not know is SKIPPED, not rendered as an
 *  error: the registry is ahead of the bundle during any rollout, and
 *  a page that blanks itself over an unknown panel key fails at
 *  exactly the moment someone is publishing a new one.
 *
 *  No lens (or a lens declaring no panels) falls back to everything
 *  this surface ships — the behaviour before the column existed. An
 *  install that has not migrated keeps its whole page.
 *
 *  A row declaring ONLY keys this build does not know falls back the
 *  same way, rather than rendering an empty page. That case stopped
 *  being hypothetical on 2026-09-10: renaming the last panel from
 *  `corpus` to `queue` left both the live row (`["corpus"]`, authored
 *  through the API) and the tree's own seed (`["rejections",
 *  "corpus"]`, 138-station-lens.sql) declaring nothing this build
 *  ships. A migration moves the live row; the fallback is what makes
 *  the window between deploy and migrate — and any install that never
 *  takes the migration — render the page instead of a header over
 *  blank space. Declaring nothing and declaring only unknowns are the
 *  same state from the renderer's side: no honourable instruction. */
export function panelsFor(lens: StationLens | null | undefined): readonly PanelKey[] {
  const declared = lens?.panels;
  if (!declared || declared.length === 0) return KNOWN_PANELS;
  const known = new Set<string>(KNOWN_PANELS);
  const kept = declared.filter((p): p is PanelKey => known.has(p));
  return kept.length > 0 ? kept : KNOWN_PANELS;
}

/** The queue's packets, in the order the station handed them over.
 *
 *  This replaced `reviewsByDocPath` on 2026-09-10. That function keyed
 *  packets by `subject.id` on the belief that a review's subject is
 *  the doc path it is about — true of the `design-doc-review` packets
 *  the corpus page opened, and false of every `design-doc` packet the
 *  station actually holds, whose subject is the literal
 *  `{"custom","boss-platform"}` its Workflow stamps. So the join
 *  silently produced an empty map and the page rendered files instead
 *  of packets. There is no key to join on, which is why the panel
 *  renders the queue directly rather than joining it to anything.
 *
 *  The station's declared discipline (priority, then age) is the
 *  order; this preserves it rather than sorting again, so what the
 *  page shows first is what the station would hand out first.
 *
 *  `steps` is the envelope's map; each row reads its own review step
 *  from it for progress and for the Review button's destination. */
export function queueRows(
  packets: readonly QueuePacket[],
  steps?: Readonly<Record<string, readonly LensStep[]>>,
): readonly ReviewPacket[] {
  return packets.map((p) => {
    const step = reviewStepOf(steps?.[p.id]);
    return {
      id: p.id,
      opened_on: p.opened_on,
      title: p.title,
      progress: reviewProgress(step),
      reviewStepId: step?.id ?? null,
    };
  });
}

/// Step kind backing the review surface (`step_plugins` row
/// 'review-design', tier 0 of the design-doc-review Workflow).
export const REVIEW_STEP_KIND = 'review-design';

/** The station holding designs whose review completed — the `decided`
 *  panel's one read (infra/platform/stations/design-decided.toml). */
export const DECIDED_STATION = 'design-decided';

/** The station this page is the lens of — the `queue` panel's read. */
export const REVIEW_STATION = 'design-review';

/** A station's evaluated queue — the one read shape both panels make. */
export const stationQueuePath = (station: string): string => `/api/stations/${station}/queue`;

/** The review queue's envelope, or a throw naming the read (backlog
 *  67825067). The page cast the body to the envelope, so a list where
 *  the envelope was due read as a queue with no `data` and painted
 *  "Nothing is waiting on a decision."; the page's catch now paints the
 *  throw as its failure line. The envelope's own fields beyond `data`
 *  stay as the server sent them — each has a reader that tolerates its
 *  absence (`pageHeader`, `panelsFor`, `queueRows`). */
export function parseDesignQueue(raw: unknown): DesignQueueEnvelope {
  return readEnvelope(stationQueuePath(REVIEW_STATION), raw).body as unknown as DesignQueueEnvelope;
}

function reviewStepOf(steps: readonly LensStep[] | undefined): LensStep | undefined {
  return steps?.find((s) => s.kind === REVIEW_STEP_KIND);
}

function stepOf(steps: readonly LensStep[] | undefined, slug: string): LensStep | undefined {
  return steps?.find((s) => s.spec_slug === slug);
}

function record(v: unknown): Readonly<Record<string, unknown>> {
  return v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : {};
}

function text(v: unknown): string | null {
  return typeof v === 'string' && v.trim() !== '' ? v : null;
}

/** See `ReviewProgress`. An answer is a non-blank `decision` whose
 *  anchor is one of the questions asked — the same pairing the
 *  Workflow's `covers = "questions"` holds the step to at done. */
export function reviewProgress(step: LensStep | undefined): ReviewProgress {
  if (!step) return { kind: 'unread' };
  const meta = record(step.metadata);
  const questions = Array.isArray(meta.questions) ? meta.questions : [];
  const asked = new Set(questions.map((q) => record(q).anchor).filter((a) => typeof a === 'string'));
  const resolutions = Array.isArray(meta.resolutions) ? meta.resolutions : [];
  const answered = new Set(
    resolutions
      .map(record)
      .filter((r) => typeof r.anchor === 'string' && asked.has(r.anchor) && text(r.decision) !== null)
      .map((r) => r.anchor),
  ).size;
  return answered > 0 || step.status === 'active'
    ? { kind: 'saved', answered, asked: asked.size }
    : { kind: 'untouched', asked: asked.size };
}

export function progressLabel(p: ReviewProgress): string {
  switch (p.kind) {
    case 'unread':
      return '—';
    case 'untouched':
      return p.asked === 0
        ? 'not started · nothing asked'
        : `not started · ${p.asked} question${p.asked === 1 ? '' : 's'}`;
    case 'saved':
      return p.asked === 0 ? 'opened · nothing asked' : `saved · ${p.answered} of ${p.asked} answered`;
  }
}

/** A design whose review has completed, as the `decided` panel draws
 *  it. Every field is read off the packet or its steps; a step the
 *  envelope did not carry leaves its fields null rather than guessed. */
export type DecidedRow = Readonly<{
  id: string;
  title: string;
  /** When the review completed (the review step's `completed_on`). */
  decided_on: string | null;
  /** The `fold` step's status — where the fold has got to. */
  fold_status: string | null;
  closed_on: string | null;
  outcome: string | null;
  /** What the fold recorded as where the decision landed. */
  folded_into: string | null;
}>;

/** The decided station's members, split into WORKING (open: decided,
 *  folding) and OUT (closed inside the station's terminal window),
 *  each in the order the station handed them over. Takes `unknown`
 *  because it is the body of a read, and a body that is not an
 *  envelope THROWS (backlog 67825067): it used to be no rows, which the
 *  panel painted as "Nothing decided is waiting to be folded." and
 *  Settled (0). The panel's catch paints the throw as its failure line. */
export function decidedRows(body: unknown): Readonly<{
  working: readonly DecidedRow[];
  settled: readonly DecidedRow[];
}> {
  const env = readEnvelope(stationQueuePath(DECIDED_STATION), body);
  const data = env.data as unknown as readonly QueuePacket[];
  const steps = record(env.body.steps) as Readonly<Record<string, readonly LensStep[]>>;
  const rows = data.map((p) => {
    const s = Array.isArray(steps[p.id]) ? steps[p.id] : undefined;
    const fold = stepOf(s, 'fold');
    return {
      open: p.status === 'open',
      row: {
        id: p.id,
        title: p.title,
        decided_on: reviewStepOf(s)?.completed_on ?? null,
        fold_status: fold?.status ?? null,
        closed_on: p.closed_on ?? null,
        outcome: text(record(p.metadata).outcome),
        folded_into: text(record(fold?.metadata).folded_into),
      },
    };
  });
  return {
    working: rows.filter((r) => r.open).map((r) => r.row),
    settled: rows.filter((r) => !r.open).map((r) => r.row),
  };
}

/** A date as "today" / "3d ago" / "2mo ago" — both panels' when column.
 *  Moved here from the page on 2026-09-24 so the decided panel reads
 *  the same words rather than a second copy of the function. */
export function relTime(iso: string, now: Date = new Date()): string {
  const days = Math.floor((now.getTime() - new Date(iso).getTime()) / 86_400_000);
  if (days < 1) return 'today';
  if (days === 1) return '1d ago';
  if (days < 30) return `${days}d ago`;
  if (days < 365) return `${Math.floor(days / 30)}mo ago`;
  return `${Math.floor(days / 365)}y ago`;
}

export function foldLabel(status: string | null): string {
  switch (status) {
    case null:
      return '—';
    case 'pending':
      return 'not yet ready';
    case 'ready':
      return 'waiting for a builder';
    case 'active':
      return 'being folded';
    default:
      return status;
  }
}

/** Where Back returns to from the review surface. Without it the step
 *  surface fell back to the job page — the one place the reviewer was
 *  deliberately not sent (David, feedback 40fe7291). */
export const BACK_HERE = `from=${encodeURIComponent('/it/design')}&from_label=${encodeURIComponent('Design Review')}`;

/** The route into a review.
 *
 *  `stepId` is the `review-design` step when the caller has resolved
 *  it. Reading a design doc is the whole point of this Job, and the
 *  job page renders the doc in a panel beside a sidebar and a step
 *  list — so the full-page step surface is the destination, and the
 *  job page is the fallback for a packet caught before its steps
 *  materialized. */
export function reviewHref(jobId: string, stepId?: string | null): string {
  return stepId ? `/jobs/${jobId}/steps/${stepId}?${BACK_HERE}` : `/service/${jobId}`;
}
