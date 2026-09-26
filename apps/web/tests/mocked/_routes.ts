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
/// assert surfaces RENDER, not that forms submit. Detail routes are
/// included (a Workflow, a marketing asset, an employee) because the mock seeds them,
/// and that is where the omitted-field crashes live — and one per
/// parameterised catalog path (the rule editor), because a pattern is
/// not a URL a crawl can open.
///
/// Pinned against the route catalog by route-smoke.mocked.spec.ts, so a
/// newly registered surface is crawled by default — by BOTH crawls.
export const ROUTES: ReadonlyArray<string> = [
  // User Experiences perspective — bare / is the public home alias; the
  // operator surfaces are re-rooted under /ux/*.
  '/', '/ux/me', '/ux/inbox', '/ux/views', '/ux/jobs', '/ux/accounts', '/ux/vendors', '/ux/people', '/ux/parts',
  // An employee's page — the persona's own row, which _smokeMocks.ts
  // seeds at EMPLOYEE_DETAIL (backlog 1a83fe98). Until then only
  // employee-page-roster-read.mocked.spec.ts checked what it says when
  // its reads fail, and no crawl opened it at all.
  '/ux/people/emp-001',
  '/ux/products', '/ux/shipping', '/ux/assets', '/ux/catalog',
  '/ux/marketing-assets', '/ux/marketing-assets/ma-1', '/ux/calendar/me',
  '/ux/support', '/ux/service', '/ux/qa', '/ux/hr', '/ux/sales',
  '/ux/shop', '/ux/manual',
  // Finance — DEFERRED until 2026-09-25 for want of object-shaped
  // statement fixtures, which _smokeMocks.ts now carries (page audit
  // 3f964c57, backlog e0732f75). Under the empty leg every statement
  // answers its well-formed empty body; under the outage, Overview's
  // two failure lines wear the marker.
  '/ux/finance',
  // The IT department — six surfaces, families as tabs (1f6d55e0).
  // /system is GONE (David's Q1/Q4: no legacy users, no redirects), so
  // this list crawls exactly what the catalog declares and nothing
  // else answers.
  '/it', '/it/registry/subjects', '/it/registry/dispatcher', '/it/registry/rules',
  // The rule editor — a detail route, because its catalog path is the
  // pattern /it/registry/rules/:ruleName (car 3071e235) and a pattern
  // is not a URL. route-smoke's drift test holds every parameterised
  // catalog path to a row here that routes to it (backlog d7732e88).
  // Under the mock's `[]` catch-all the versions read answers empty, so
  // the editor paints "No versions found"; under the outage it paints
  // `load-failed`.
  '/it/registry/rules/auto-park-on-gate-green',
  '/it/operate/perf',
  '/it/operate/atlas', '/it/registry/step-plugins', '/it/kb', '/it/design',
  '/it/design/experiments',
  // Modeling + admin surfaces (System Model).
  '/it/registry', '/it/registry/new',
  '/it/registry/seasonal-release', '/it/registry/policy', '/it/auth-admin',
  // '/it/design/feedback' and '/it/design/backlog' were crawled here
  // until car N3 of design e765b3fc (2026-09-25): each board is a
  // station's panel on the Department Map now, crawled below with the
  // station that carries it.
  // Incidents (the Operate landing) renders both panels' empty states
  // under the mock's `[]` catch-all — chrome + empty states, no crash.
  '/it/operate',
  // Bottlenecks (was Fleet) renders its no-Workflows empty state under
  // the mock's empty /api/workflows — page chrome + picker. The map
  // and flow pages died into the Atlas tab (already crawled above).
  '/it/operate/bottlenecks',
  // The Audit Log (catalogued as Monitoring). Deferred until 2026-09-23
  // for want of an object-shaped /api/events/stats fixture, which
  // _smokeMocks.ts now carries (EVENTS_STATS); its live stream answers
  // the floor's 204, so the page falls to its snapshot poll. Its own
  // controls are pinned in audit-log-page.mocked.spec.ts (65a273d5).
  '/it/operate/audit',
  // THE DEPARTMENT MAP'S STATIONS (design e765b3fc, car N3). Each was a
  // floor page at /it/yard/<region> — and the shop floor was the Crew
  // Board at /it/crew — until car N3 made each one a SELECTION on the
  // map, its floor, its board and the boards of the pages that retired
  // into it (yard status, the conductor's feed, the feedback and backlog
  // boards; panel.ts `STATION_BOARDS`) drawn in its panel under the map.
  // So the crawl opens the selections, one per station the floors
  // covered: the panel renders each board's empty states under the
  // mocks and its `load-failed` line under the outage, exactly as the
  // pages did. The old paths are not found (router.test.ts pins each).
  '/it?at=dock',
  '/it?at=gates',
  '/it?at=track',
  '/it?at=shed',
  '/it?at=arrivals',
  '/it?at=garage',
  '/it?at=receiving',
  '/it?at=marshalling',
  '/it?at=shop-floor',
  // The codebase — its own sidebar row since feedback 9827c699
  // (2026-09-14; a Design tab before, backlog 06048ade). Its one read is
  // `/api/jobs?kind=maintenance-codebase-metrics`; under the mock's `[]`
  // catch-all it renders "no packet carries a measurement" as a bordered
  // notice, and under the outage it renders `load-failed`. The row's
  // path is what the catalog registers; the older tab path,
  // /it/design/codebase, retired with car N3 of design e765b3fc.
  '/it/codebase',
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
  // A department's jobs view — in / working / out (cc76f755). One
  // surface for every department the Class registry declares, so it
  // has no catalog entry and is crawled here by one code; the mock's
  // `[]` catch-all for `/api/jobs?department=sales` renders the "no
  // jobs in Sales" state, and the outage renders `load-failed`.
  '/ux/departments/sales',
  // The landing page (the System Model live view). It was reachable only
  // through the router's catch-all, crawled here as '/ux/unknown-path',
  // until design ee3a3a2f gave it this door.
  '/ux/system-model',
  // The router's catch-all — see NOT_FOUND_ROW.
  '/ux/accounts/agreements/x',
];

/// The one ROUTES entry the router does NOT serve, on purpose: an
/// unmatched path renders the not-found page, naming the path (design
/// ee3a3a2f). It is spelled as one of the dead links that motivated that
/// page — a greedy `/accounts/(.+)` used to render it as the account page
/// for a missing account "agreements/x". Before that the catch-all
/// rendered the landing page, and until 2026-09-18 this row was spelled
/// '/ux/refurb' while both crawls believed they were crawling a refurb
/// page. interaction-crawl pins every OTHER row to the router (f2b8a01c).
export const NOT_FOUND_ROW = '/ux/accounts/agreements/x';

/// THE PAGES THE DEPARTMENT MAP REPLACED — design e765b3fc, car N3
/// (2026-09-25). Each path answered until then and is not found now, like
/// any path nothing serves: its content is a selection's panel on the
/// map (the floors and the Crew Board are their stations', yard status
/// the track's, dock's and garage's, the conductor's feed the track's,
/// the feedback and backlog boards receiving's and marshalling's —
/// src/it/yard/panel.ts `STATION_BOARDS`), and David's word was no
/// aliases and no redirects before 1.0.0. So none of these may creep
/// back into ROUTES, and each must render the not-found page with its
/// one door, the Department Map: router.test.ts pins the route and the
/// door, and it-department-map-stations.mocked.spec.ts opens each one.
export const RETIRED_ROUTES: ReadonlyArray<string> = [
  '/it/yard', '/it/yard/dock', '/it/yard/gates', '/it/yard/track', '/it/yard/shed',
  '/it/yard/arrivals', '/it/yard/garage', '/it/yard/publish', '/it/yard/shop-floor',
  '/it/yard/receiving', '/it/yard/marshalling', '/it/crew',
  '/it/operate/receiving', '/it/operate/marshalling', '/it/operate/yard-status',
  '/it/operate/conductor', '/it/design/feedback', '/it/design/backlog', '/it/design/codebase',
];

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
  // '/it/operate/audit' left on 2026-09-23 (page audit 65a273d5, gap
  // 0398c4d0): the fixture it waited for is EVENTS_STATS in
  // _smokeMocks.ts, and audit-log-page.mocked.spec.ts pins the page.
  // '/ux/finance' left on 2026-09-25 (page audit 3f964c57, backlog
  // e0732f75): its statements' object-shaped fixtures are in
  // _smokeMocks.ts (COMMERCE_SUMMARY, AP_AGING, LEDGER_STATEMENTS), and
  // finance-page.mocked.spec.ts pins the page's own controls.
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
