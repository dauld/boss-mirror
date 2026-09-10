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
 *  and `design-review`'s predicate is a kind match.
 *
 *  No `subject` either, though the envelope carries one. Nothing on
 *  this page reads it any more — see `queueRows` for the join that
 *  used to and why it could never have worked. */
export type QueuePacket = Readonly<{
  id: string;
  title: string;
  status: string;
  opened_on: string;
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

/** An open review packet as the queue panel renders it. */
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
 *  exists to show them. */
export const KNOWN_PANELS = ['queue'] as const;
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
 *  page shows first is what the station would hand out first. */
export function queueRows(packets: readonly QueuePacket[]): readonly ReviewPacket[] {
  return packets.map((p) => ({
    id: p.id,
    status: p.status,
    opened_on: p.opened_on,
    title: p.title,
  }));
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
