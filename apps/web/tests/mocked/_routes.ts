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
  '/ux/support', '/ux/service', '/ux/qa', '/ux/hr', '/ux/sales',
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
  // The codebase — its own sidebar row since feedback 9827c699
  // (2026-09-14; a Design tab before, backlog 06048ade). Its one read is
  // `/api/jobs?kind=maintenance-codebase-metrics`; under the mock's `[]`
  // catch-all it renders "no packet carries a measurement" as a bordered
  // notice, and under the outage it renders `load-failed`. The row's
  // path is what the catalog registers; the older tab path still routes
  // and is crawled so a bookmark cannot rot unnoticed.
  '/it/codebase',
  '/it/design/codebase',
  // Protocol drift — a Registry tab (4ae9969e). Its one read is
  // `/api/jobs?kind=maintenance-protocol-drift`; under the mock's `[]`
  // catch-all it renders "the 05:20 measurement has not filed" as a
  // bordered notice, and under the outage it renders `load-failed`.
  '/it/registry/drift',
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
  // A department's jobs view — in / working / out (cc76f755). One
  // surface for every department the Class registry declares, so it
  // has no catalog entry and is crawled here by one code; the mock's
  // `[]` catch-all for `/api/jobs?department=sales` renders the "no
  // jobs in Sales" state, and the outage renders `load-failed`.
  '/ux/departments/sales',
  // The router's catch-all — see LANDING_FALLBACK.
  '/ux/unknown-path',
];

/// The one ROUTES entry the router does NOT serve, on purpose: an
/// unknown path renders LandingPage (the System Model live view) as the
/// catch-all, and nothing else reaches that page. Until 2026-09-18 this
/// row was spelled '/ux/refurb' and both crawls believed they were
/// crawling a refurb page — there is no refurb route, and the outage
/// roster explained its silence with reads the landing page makes.
/// interaction-crawl pins every OTHER row to the router (f2b8a01c).
export const LANDING_FALLBACK = '/ux/unknown-path';

/// Routes the crawls cannot cover yet, each with why. Shrinking this
/// list is the work; adding to it is a decision.
///
/// It lives beside ROUTES for the same reason ROUTES lives here: two
/// specs walk the catalog (route-smoke.mocked.spec.ts and
/// interaction-crawl.mocked.spec.ts, since f2b8a01c) and a route
/// deferred in one and not the other would be crash-checked and never
/// interaction-checked, or the reverse, with nothing to say so. One
/// definition cannot disagree with itself (CLAUDE.md §9a).
export const DEFERRED: ReadonlyMap<string, string> = new Map([
  ['/it/operate/audit', 'aggregation dashboard: snapshot .length needs a faithful fixture'],
  ['/ux/finance', 'statements .reduce needs object-shaped fixtures'],
  ['/ux/warehouse', 'summary.below_reorder_count needs a faithful fixture'],
  ['/ux/exec', '.find/.length over object-shaped summaries'],
  // '/system/os-map' deferral dropped: the page retired with the
  // pre-network framing and its catalog entry is gone.
]);

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
