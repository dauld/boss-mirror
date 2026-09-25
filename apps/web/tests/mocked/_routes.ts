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
/// included (a Workflow + a marketing asset) because the mock seeds them,
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
  // The Audit Log (catalogued as Monitoring). Deferred until 2026-09-23
  // for want of an object-shaped /api/events/stats fixture, which
  // _smokeMocks.ts now carries (EVENTS_STATS); its live stream answers
  // the floor's 204, so the page falls to its snapshot poll. Its own
  // controls are pinned in audit-log-page.mocked.spec.ts (65a273d5).
  '/it/operate/audit',
  // The yard's FLOORS (design 0524fc95, car 2). /it above is the MAP —
  // eight region cards read from /api/yard/regions — and each yard
  // card opens the Train Yard at /it/yard/<region>, focused on that
  // region's panel; the bare /it/yard is the yard on the track. The
  // page is the same one /it used to mount, so each floor renders the
  // yard's empty states under the mocks and its `load-failed` line
  // under the outage. Receiving and marshalling are floors too since
  // car 4 of design d2154293, and are listed below with the paths
  // their retired pages answered at.
  '/it/yard',
  '/it/yard/dock',
  '/it/yard/gates',
  '/it/yard/track',
  '/it/yard/shed',
  '/it/yard/arrivals',
  '/it/yard/garage',
  // The Marshalling Yard — the upstream third, a REGION of the world
  // since car 4 of design d2154293: the territory draws a platform per
  // station and the board mounts under it. Under the mock's `[]`
  // catch-all, /api/stations/load and /api/stations/flow come back as
  // collections with no rows, so it renders its "every watched station
  // is clear" state. The drift test in route-smoke.mocked.spec.ts
  // enforces the catalog row's path, which is this one.
  '/it/yard/marshalling',
  // The Receiving Yard — the intake floor, a region beside it. Its
  // reads are /api/workflows and `/api/jobs?kind=…&closed_within=…`;
  // under the mock's `[]` catch-all both come back empty, so it
  // renders its no-intake state, and every read goes through
  // fetchRemote, so the outage renders a failure line rather than an
  // empty yard.
  '/it/yard/receiving',
  // The two paths the pages answered at before car 4. They still
  // route — to the same region — and are crawled so a bookmark, a
  // packet or a brief that names one cannot rot unnoticed.
  '/it/operate/marshalling',
  '/it/operate/receiving',
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
  // sidebar row of its own (backlog 04c5bbc0). Its five reads are
  // `/api/jobs?kind=ship-a-change`, `/api/jobs?kind=gate-run`,
  // `/api/yard/status`, `/api/jobs/queue-age` and
  // `/api/jobs?kind=agent-run&status=open` (c87fb59b car 2): all but
  // the yard come back `[]` from the catch-all and the yard from the
  // well-formed empty fixture above, so the crawl renders the board's
  // five empty stage columns, its empty crew list and no runs. Every read
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
