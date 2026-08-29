// Route smoke harness — the gated catch-all for "page renders real data
// and throws". Crawls every top-level surface against an adversarial
// mocked backend (see _smokeMocks.ts) and FAILS on any uncaught
// exception (pageerror) or a shell that never paints. This is the layer
// that was missing when StepDagEditor (omitted `terminal`) and the
// marketing-assets page crashed in the browser: pure-logic unit tests
// never mount the component, and svelte-check passes when the *type* is
// the thing that's wrong.
//
// Run: bunx playwright test -c playwright.mocked.config.ts tests/mocked/route-smoke.mocked.spec.ts

import { test, expect } from '@playwright/test';
import { installSmokeMocks } from './_smokeMocks';
import { installAuthoringMocks, JOB_ID } from './_mockApi';

// Every top-level surface a ceo persona reaches, from the router's
// exact-match routes. Pure-action / form-submit routes (/login,
// /finance/new, /finance/journal-entries/new) are excluded — this asserts
// surfaces RENDER without throwing, not that forms submit. Two detail
// routes are included (a Workflow + a marketing asset) because the mock
// seeds them, and that is where the omitted-field crashes live.
const ROUTES: ReadonlyArray<string> = [
  // User Experiences perspective — bare / is the public home alias; the
  // operator surfaces are re-rooted under /ux/*.
  '/', '/ux/me', '/ux/inbox', '/ux/views', '/ux/jobs', '/ux/accounts', '/ux/vendors', '/ux/people', '/ux/parts',
  '/ux/products', '/ux/shipping', '/ux/assets', '/ux/catalog',
  '/ux/marketing-assets', '/ux/marketing-assets/ma-1', '/ux/calendar', '/ux/calendar/me',
  '/ux/support', '/ux/service', '/ux/refurb', '/ux/qa', '/ux/hr', '/ux/sales',
  '/ux/shop', '/ux/manual',
  // System Model perspective — the "read the running model" surfaces.
  //
  // CANONICAL PATHS ARE `/it/*`, which is what ROUTE_CATALOG registers
  // and what a browser's address bar holds. `/system/*` still answers:
  // router.ts rewrites the `/it` prefix to `/system` before matching,
  // so an old bookmark keeps working. This list crawled the `/system/*`
  // alias for three releases and therefore never rendered a single
  // canonical URL, while the drift test below reported all twenty as
  // uncovered. Both halves of that were true at once, which is why the
  // fix is to crawl what the catalog declares. The alias itself is
  // pinned in src/router.test.ts, where it costs one line instead of a
  // browser.
  '/it', '/it/subjects', '/it/dispatcher', '/it/dispatcher/rules',
  '/it/monitoring/perf', '/it/monitoring/events',
  '/it/monitoring/atlas', '/it/step-plugins', '/it/kb', '/it/design',
  '/it/experiments',
  // Modeling + admin surfaces (System Model).
  '/it/workflows', '/it/workflows/new',
  '/it/workflows/seasonal-release', '/it/policy', '/it/auth-admin',
  // IT surfaces added since the app split. They were absent for three
  // releases and the crawl reported success the whole time — see the
  // drift test at the bottom of this file for why that can no longer
  // happen quietly.
  '/it/feedback', '/it/flow', '/it/yard',
  // Incidents renders both panels' empty states under the mock's `[]`
  // catch-all (an array where the {data} envelope should be reads as
  // zero packets) — chrome + empty states, no crash.
  '/it/incidents',
  // The network map under the mock's `[]` catch-all: an array where
  // the {data} envelope should be reads as zero stations — chrome +
  // empty state, no crash.
  '/it/map',
  // Fleet renders its no-Workflows empty state under the mock's
  // empty /api/workflows — a real crawl of the page chrome + picker.
  '/it/fleet',
  // The risk watchlist. Its path is `/watchlist`, NOT `/ux/watchlist`
  // — it has no nav-catalog entry (it is URL-only), so the drift test
  // at the bottom of this file cannot notice it is missing: that test
  // only walks ROUTE_CATALOG. Adding it here is the whole of its
  // coverage.
  '/watchlist',
];

// DEFERRED, group 1 — aggregation dashboards that read OBJECT-shaped
// responses (statements, snapshots, summaries) the generic `[]` catch-all
// can't fake; they need faithful per-endpoint fixtures before they can be
// gated without false positives:
//   /ux/finance (statements .reduce) · /ux/warehouse (summary.below_reorder_count)
//   /ux/exec (.find/.length) · /system/monitoring (snapshot .length)
//
// The watchlist LEFT THIS GROUP on 2026-08-28: its `.length` crash was
// not a fixture problem but a CAST — the page read
// `(await r.json()) as { accounts: RiskScore[] }` and a payload without
// `accounts` made the value undefined. It now parses through
// RiskScoreListSchema and renders its error state on a wrong shape, so
// no faithful fixture is needed to gate it (feedback 2fe1c8c1).
//
// THAT COMMENT WAS TRUE AND THE PAGE WAS STILL NOT CRAWLED, for a full
// day, because it named `/ux/watchlist` and the route is `/watchlist`
// (router.ts:251). It was in neither ROUTES nor DEFERRED, and the drift
// test could not catch the omission because the page has no
// nav-catalog entry and that test only walks ROUTE_CATALOG. So the
// deferral was lifted in prose while the coverage it described never
// existed — a green suite reporting on a page it never opened. The
// route is now in ROUTES above, spelled the way the router spells it.
//
// Resolved: the marketing-assets no-shell this harness first caught was a
// real effect_update_depth_exceeded loop in loadClasses() called from a
// tracked $effect — fixed in session/classes.svelte.ts. The /calendar +
// /calendar/me failures seen alongside it were that loop bleeding across a
// shared page (pre-fix); both render cleanly in isolation and are gated
// above. All four routes are now in ROUTES.

type Issue = { route: string; kind: string; text: string };

test.describe('route smoke — every surface renders without a runtime crash', () => {
  test('crawl all top-level routes (adversarial mocked backend)', async ({ page }) => {
    test.setTimeout(240_000);
    // ONE shared page: the browser HTTP cache keeps the (on-the-fly
    // bundled) dev-server JS warm, so each route reloads fast and the
    // shell paints reliably. A full goto per route reloads the document,
    // wiping the previous route's JS state — so there's no effect/timer
    // bleed despite sharing the page. `page.route` handlers persist
    // across navigations, so the mocks are installed once.
    await installSmokeMocks(page);

    const issues: Issue[] = [];
    let route = '';
    page.on('pageerror', (e) => issues.push({ route, kind: 'pageerror', text: e.message }));
    page.on('console', (m) => {
      if (m.type() === 'error') issues.push({ route, kind: 'console.error', text: m.text() });
    });

    for (const r of ROUTES) {
      route = r;
      // Retry the shell check once: the on-the-fly bundling dev-server
      // occasionally serves a route slowly enough that the shell misses
      // the window; a reload settles it. A genuine render failure misses
      // BOTH attempts (and a real crash still fires its pageerror, which
      // is recorded regardless of the shell timing).
      let shellOk = false;
      for (let attempt = 1; attempt <= 2 && !shellOk; attempt++) {
        try {
          // 'commit' (not 'load'/'domcontentloaded'): for a client-routed
          // SPA we only need the navigation to commit; the real readiness
          // signal is the AppShell painting, asserted next.
          await page.goto(r, { waitUntil: 'commit', timeout: 20_000 });
          await expect(page.locator('.app-shell')).toBeVisible({ timeout: 20_000 });
          shellOk = true;
        } catch (e) {
          if (attempt === 2) {
            issues.push({ route: r, kind: 'no-shell', text: e instanceof Error ? e.message : String(e) });
          }
        }
      }
      // Let onMount effects + the (instant) mocked fetches settle so any
      // data-render crash fires while we're listening.
      if (shellOk) await page.waitForTimeout(500);
    }

    // Gate on crashes: uncaught exceptions + shells that never painted.
    // console.error is reported but not gated (the adversarial empty
    // fixtures provoke benign error logs on some pages).
    const crashes = issues.filter((i) => i.kind !== 'console.error');
    const noise = issues.filter((i) => i.kind === 'console.error');
    if (noise.length) {
      console.log(`\nconsole.error (non-gating, ${noise.length}):`);
      for (const i of noise) console.log(`  [${i.route}] ${i.text}`);
    }
    expect(
      crashes,
      `Runtime crashes across ${ROUTES.length} routes:\n` +
        crashes.map((i) => `  [${i.route}] ${i.kind}: ${i.text}`).join('\n'),
    ).toEqual([]);
  });

  test('Workflow authoring workspace renders a serde-omitted terminal (StepDagEditor)', async ({ page }) => {
    test.setTimeout(60_000);
    // _mockApi.seedSpec() now omits `terminal` on the non-terminal step —
    // the exact shape that crashed StepDagEditor before the fix.
    await installAuthoringMocks(page);

    const errors: string[] = [];
    page.on('pageerror', (e) => errors.push(e.message));

    await page.goto(`/system/workflows/authoring/${JOB_ID}`, { timeout: 20_000 });
    await expect(page.locator('.app-shell')).toBeVisible({ timeout: 10_000 });
    // Wait for the lazy graph + the step-authoring surface (which mounts
    // StepDagEditor) to render the seeded spec.
    await page.waitForTimeout(2_000);

    expect(errors, `pageerrors in the authoring workspace:\n${errors.join('\n')}`).toEqual([]);
  });
});

// A crawl that silently omits a surface reports success for a page it
// never loaded. That is not hypothetical: `/system/feedback`,
// `/system/os-map` and `/system/flow` were each added to the app, each
// wired into the sidebar, and none of them appeared here — the harness
// stayed green across all three because a hardcoded list cannot know
// what it is missing.
//
// So the list gets pinned against the route catalog, which is the one
// place a surface must be registered to exist at all. A new route is
// crawled by default; skipping one is a line in DEFERRED with a
// reason, not an omission.
test.describe('the crawl covers every registered surface', () => {
  /// Routes the crawl cannot cover yet, each with why. Shrinking this
  /// list is the work; adding to it is a decision.
  const DEFERRED: ReadonlyMap<string, string> = new Map([
    ['/it/monitoring', 'aggregation dashboard: snapshot .length needs a faithful fixture'],
    ['/ux/finance', 'statements .reduce needs object-shaped fixtures'],
    ['/ux/warehouse', 'summary.below_reorder_count needs a faithful fixture'],
    ['/ux/exec', '.find/.length over object-shaped summaries'],
    // '/system/os-map' deferral dropped: the page retired with the
    // pre-network framing and its catalog entry is gone.
  ]);

  test('no deferral names a route that does not exist', async () => {
    const { ROUTE_CATALOG } = await import('../../src/shell/nav-catalog');
    const registered = new Set(
      Object.values(ROUTE_CATALOG).map((r) => (r as { path: string }).path),
    );
    const ghosts = [...DEFERRED.keys()].filter((p) => !registered.has(p)).sort();
    expect(
      ghosts,
      'a deferral for an unregistered route reads as "covered elsewhere" while ' +
        'excusing nothing — drop it, or fix the path',
    ).toEqual([]);
  });

  test('every catalog route is crawled or deferred with a reason', async () => {
    const { ROUTE_CATALOG } = await import('../../src/shell/nav-catalog');
    const crawled = new Set(ROUTES);
    const missing = Object.values(ROUTE_CATALOG)
      .map((r) => (r as { path: string }).path)
      // Parameterised paths are covered by the detail routes above.
      .filter((p) => typeof p === 'string' && !p.includes(':'))
      .filter((p) => !crawled.has(p) && !DEFERRED.has(p))
      .sort();
    expect(
      missing,
      `these surfaces are registered but never rendered by any test — add them to ROUTES, ` +
        `or to DEFERRED with the reason they cannot be crawled yet`,
    ).toEqual([]);
  });
});
