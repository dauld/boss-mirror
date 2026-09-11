// The roster of crawled surfaces, and the ONE marker a surface uses to
// say a read failed. Both are read by two specs, so both live here.
//
// WHY THIS FILE EXISTS (CLAUDE.md §9a). `ROUTES` used to live inside
// route-smoke.mocked.spec.ts. outage-crawl.mocked.spec.ts needs the same
// roster, and a second copy of it would reproduce — exactly — the defect
// route-smoke's own drift test was written to stop: a crawl that
// silently omits a surface reports success for a page it never loaded.
// A route added to one list and not the other would be crash-checked and
// never outage-checked, and nothing would say so. So the list is
// collapsed to one definition rather than pinned by an equality test.

/// Every top-level surface a ceo persona reaches, from the router's
/// exact-match routes. Pure-action / form-submit routes (/login,
/// /finance/new, /finance/journal-entries/new) are excluded — the crawls
/// assert surfaces RENDER, not that forms submit. Two detail routes are
/// included (a Workflow + a marketing asset) because the mock seeds them,
/// and that is where the omitted-field crashes live.
///
/// Pinned against the route catalog by route-smoke.mocked.spec.ts, so a
/// newly registered surface is crawled by default — by BOTH crawls.
export const ROUTES: ReadonlyArray<string> = [
  // User Experiences perspective — bare / is the public home alias; the
  // operator surfaces are re-rooted under /ux/*.
  '/', '/ux/me', '/ux/inbox', '/ux/views', '/ux/jobs', '/ux/accounts', '/ux/vendors', '/ux/people', '/ux/parts',
  '/ux/products', '/ux/shipping', '/ux/assets', '/ux/catalog',
  '/ux/marketing-assets', '/ux/marketing-assets/ma-1', '/ux/calendar', '/ux/calendar/me',
  '/ux/support', '/ux/service', '/ux/refurb', '/ux/qa', '/ux/hr', '/ux/sales',
  '/ux/shop', '/ux/manual',
  // The IT department — six surfaces, families as tabs (1f6d55e0).
  // /system is GONE (David's Q1/Q4: no legacy users, no redirects), so
  // this list crawls exactly what the catalog declares and nothing
  // else answers.
  '/it', '/it/registry/subjects', '/it/registry/dispatcher', '/it/registry/rules',
  '/it/operate/perf',
  '/it/operate/atlas', '/it/registry/step-plugins', '/it/kb', '/it/design',
  '/it/design/experiments',
  // Modeling + admin surfaces (System Model).
  '/it/registry', '/it/registry/new',
  '/it/registry/seasonal-release', '/it/registry/policy', '/it/auth-admin',
  // IT surfaces added since the app split. They were absent for three
  // releases and the crawl reported success the whole time — see the
  // drift test in route-smoke.mocked.spec.ts for why that can no longer
  // happen quietly.
  '/it/design/feedback',
  '/it/design/backlog',
  // Incidents (the Operate landing) renders both panels' empty states
  // under the mock's `[]` catch-all — chrome + empty states, no crash.
  '/it/operate',
  // Bottlenecks (was Fleet) renders its no-Workflows empty state under
  // the mock's empty /api/workflows — page chrome + picker. The map
  // and flow pages died into the Atlas tab (already crawled above).
  '/it/operate/bottlenecks',
  // Yard status renders the empty yard under the mock's `[]` catch-all
  // for /api/yard/status — chrome + "no trains / no cars", no crash.
  '/it/operate/yard-status',
  // The Marshalling Yard — the upstream third. Under the mock's `[]`
  // catch-all, /api/stations/load and /api/stations/flow come back as
  // collections with no rows, so the page renders its "every watched
  // station is clear" state. Crawled here rather than via a catalog
  // entry because it is a tab, not a sidebar row (same as yard-status
  // above): pages live in their department.
  '/it/operate/marshalling',
  // The risk watchlist. Since CAR-6 it HAS a catalog entry, so the
  // drift test in route-smoke.mocked.spec.ts now enforces its presence
  // here instead of this line being the whole of its coverage.
  '/watchlist',
  // HR and the operator manual joined the catalog with the watchlist
  // (CAR-6): HR renders its empty-roster states under the mock's `[]`
  // catch-all; the manual renders its docs chrome with the fetch
  // failing honestly. Both pin chrome + no-crash, same bar as every
  // other row.
  '/hr',
  '/manual',
  // The estate page under the mock's catch-all: every /api/estate/*
  // fetch fails or reads empty, and the page's whole design is that
  // absence renders as bordered failure notices, never an empty
  // estate - chrome + three honest failure states, no crash. Its own
  // unit suite pins the failed-never-empty arms; this crawl pins that
  // the route actually mounts.
  '/it/estate',
  // The Crew Board — the middle third of the operator surface, and a
  // sidebar row of its own (backlog 04c5bbc0). Its four reads are
  // `/api/jobs?kind=ship-a-change`, `/api/jobs?kind=gate-run`,
  // `/api/yard/status` and `/api/jobs/queue-age`: the first, second and
  // fourth come back `[]` from the catch-all and the third from the
  // well-formed empty yard fixture above, so the crawl renders the
  // board's five empty stage columns and its empty crew list. Every read
  // goes through fetchRemote, so an unreachable backend renders a
  // bordered failure line per lane rather than an idle pipeline — the
  // same failed-never-empty bar as the estate row above.
  '/it/crew',
];

/// The ONE class a surface puts on a line that says "this read failed".
///
/// It has to be one name, and this is why: on 2026-09-11 a census of
/// every failure line in apps/web found `load-failed` on 37 elements and
/// roughly thirty-five OTHER bespoke names beside it — `.estate-fail`,
/// `.my-fail`, `.ys-fail`, `.crew-fail`, `.tb-err` among them. Four of
/// those pages render a failed read perfectly honestly and NOTHING could
/// assert that they do, because there was no name to assert on. A
/// vocabulary of thirty-five synonyms is not a marker; it is the reason
/// the false-empty class had to be found four times by hand.
///
/// So: one name, asserted by outage-crawl.mocked.spec.ts. Per-page
/// styling keeps its own class alongside — `class="estate-fail
/// load-failed"` — because the crawl reads the shared name and the
/// stylesheet reads the local one. A new surface that invents a
/// thirty-sixth synonym and no `load-failed` does not get a lint; it
/// fails the outage crawl, which is the same mechanism that catches a
/// surface with no failure line at all.
export const FAILURE_MARKER = '.load-failed';
