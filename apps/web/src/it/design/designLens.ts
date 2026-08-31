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

/** A packet as the station queue serves it. No `steps`: the queue
 *  endpoint fetches steps only when the predicate reads step state,
 *  and `design-review`'s predicate is a kind match. */
export type QueuePacket = Readonly<{
  id: string;
  title: string;
  status: string;
  opened_on: string;
  subject?: Readonly<{ id?: string }> | null;
}>;

/** The `GET /api/stations/{name}/queue` envelope, design-review's
 *  slice of it. Same struct yard.ts reads — kept as its own type here
 *  rather than imported so one lens cannot break another by widening
 *  what it needs. */
export type DesignQueueEnvelope = Readonly<{
  station: string;
  discipline: readonly string[];
  lens?: StationLens | null;
  total: number;
  data: readonly QueuePacket[];
}>;

/** An open review packet, keyed to the doc it is about. */
export type ReviewPacket = Readonly<{
  id: string;
  status: string;
  opened_on: string;
  title: string;
}>;

// The header this page rendered as literals before the registry
// carried it. Kept as the fallback rather than deleted: a cluster
// running a binary newer than its schema (the ordinary state during a
// rollout, and the permanent state of any install that has not taken
// 138) must render a named page, not an untitled one.
export const FALLBACK_HEADER = {
  eyebrow: 'System Model · Design review',
  title: 'Design review',
  subtitle: 'Open questions, pending decisions, ADRs',
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
 *  registry declares nothing. `rejections` first is deliberate: it
 *  names docs the indexer refused, so the corpus below it is known to
 *  be incomplete until that panel is empty. */
export const KNOWN_PANELS = ['rejections', 'corpus'] as const;
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
 *  install that has not migrated keeps its whole page. */
export function panelsFor(lens: StationLens | null | undefined): readonly PanelKey[] {
  const declared = lens?.panels;
  if (!declared || declared.length === 0) return KNOWN_PANELS;
  const known = new Set<string>(KNOWN_PANELS);
  return declared.filter((p): p is PanelKey => known.has(p));
}

/** Open review packets keyed by the doc path they are about.
 *
 *  The doc path IS the packet's subject id (identity-first Subject),
 *  which is why this join needs no metadata read. A packet whose
 *  subject carries no id is dropped: it is a review of nothing this
 *  page can show a row for. */
export function reviewsByDocPath(
  packets: readonly QueuePacket[],
): Readonly<Record<string, ReviewPacket>> {
  const byPath: Record<string, ReviewPacket> = {};
  for (const p of packets) {
    const path = p.subject?.id;
    if (!path) continue;
    // First wins. The queue arrives in the station's declared
    // discipline (priority, then age), so when two packets somehow
    // exist for one doc the operator is sent to the one the station
    // would hand out first — not to whichever the loop saw last.
    if (byPath[path] === undefined) {
      byPath[path] = {
        id: p.id,
        status: p.status,
        opened_on: p.opened_on,
        title: p.title,
      };
    }
  }
  return byPath;
}

/// Step kind backing the review surface (`step_plugins` row
/// 'review-design', tier 0 of the design-doc-review Workflow).
export const REVIEW_STEP_KIND = 'review-design';

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
