// THE OUTAGE CRAWL — backlog 89011641.
//
// route-smoke.mocked.spec.ts asks every surface "do you survive?". This
// spec asks the question that was actually going wrong: "what do you SAY
// when your read fails?"
//
// THE CLASS. A failed read rendered as an empty result. It has now been
// found four times by hand and fixed four times by hand: TriageBoard
// printed already-routed cards under "Nobody has routed these yet" when
// /api/workflows was down; the Inbox announced "Nothing is waiting on
// you" during a message-store outage; HrPage said an employee had no
// onboarding tasks when the step read 503'd; and before those, the two
// instances Remote<T> was introduced for. Every one of them rendered
// perfectly. /ux/hr passed the route crawl for as long as its workflow
// table was structurally incapable of showing a row.
//
// WHY THE EXISTING CRAWL STRUCTURALLY CANNOT CATCH IT. It gates on
// `pageerror` and a shell that never paints. A page that renders a
// falsehood does neither. And a mocked unit test cannot catch it either:
// it asserts what the client does with a fixture the test author wrote,
// so it agrees with a wrong reader by construction.
//
// WHAT THIS DOES. Installs the same adversarial mocks, then forces every
// /api/** call to 500 EXCEPT the handful the app shell needs to paint at
// all (identity + taxonomy — see HEALTHY). Then for each route: the page
// must show at least one FAILURE_MARKER. An outage is the one condition
// where a data-bearing surface has nothing true to say, so saying
// nothing is the bug.
//
// THE NUMBER THIS STARTED FROM (measured 2026-09-11, twice, identical):
// of the 51 crawled routes, 17 showed SOME failure marker and 34 did not. None
// failed to paint, and none of the 34 was silent for want of a broken
// read — every route issues at least three failing requests. So the 34
// are debt, they are listed in SILENT below with what each reads, and
// the list is the work. It is pinned in BOTH directions (see the two
// tests at the bottom) so it can only shrink.

import { test, expect, type Page } from '@playwright/test';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER, ROUTES } from './_routes';

/// The endpoints that stay up. The app shell cannot paint without
/// identity and the nav cannot resolve visibility without the taxonomy
/// registries, and a crawl where nothing renders measures nothing. Every
/// other endpoint — including the chrome's own /api/jobs/live and
/// /api/jobs/step-types — is broken, which is what makes the assertion
/// below meaningful: those two break on all 51 routes, and the 34 silent
/// ones still show no marker, so a marker is never chrome's.
const HEALTHY: ReadonlyArray<RegExp> = [
  /\/api\/session$/,
  /\/api\/auth\/me$/,
  /\/api\/people$/,
  /\/api\/tenant\/manifest$/,
  /\/api\/classes(\?|$)/,
  /\/api\/subject-kinds$/,
];

/// Surfaces that render NOTHING when every read behind them fails, with
/// what each one reads. Shrinking this list is the work; adding to it is
/// a decision, and a new surface is asserted by default because the
/// roster is ROUTES minus this map.
///
/// THE ENTRY IS A CLAIM ABOUT THE PAGE, NOT AN EXEMPTION. The second
/// test below re-runs the crawl over these routes and fails if one of
/// them HAS started showing a marker — so fixing a page forces its line
/// here to be deleted, and the list cannot quietly go on excusing
/// surfaces that no longer need it. That is the half the packet's
/// "ROUTES-style drift pin" was asking for: a roster that rots in either
/// direction reds the gate.
const SILENT: ReadonlyMap<string, string> = new Map([
  ['/', 'home: /api/jobs/live + the sim-clock stream'],
  ['/ux/me', 'My Day: /api/jobs/live; identity-keyed reads never fire under the empty mocked session'],
  ['/ux/inbox', 'inbox: /api/messages/inbox/{id} never fires under the empty mocked session'],
  ['/ux/views', 'views composer: /api/views'],
  ['/ux/jobs', 'jobs list: /api/jobs + /api/workflows'],
  ['/ux/accounts', 'accounts: /api/people/accounts, /api/assets, /api/commerce/invoices'],
  ['/ux/vendors', 'vendors: /api/inventory/vendors, /orders, /vendor-invoices'],
  ['/ux/people', 'people: roster reads /api/people, which HEALTHY keeps up — needs a per-read outage'],
  ['/ux/assets', 'assets: /api/assets + /api/assets/summary'],
  ['/ux/calendar/me', 'my calendar: identity-keyed reads never fire under the empty mocked session'],
  ['/ux/service', 'service: /api/jobs + /api/workflows'],
  ['/ux/refurb', 'refurb: /api/jobs + /api/workflows/{kind}'],
  ['/ux/hr', 'HR: the workflow + step reads fire on the Workflows tab, not on load (see false-empty.mocked.spec.ts, which pins them)'],
  ['/ux/sales', 'sales: /api/jobs + /api/workflows'],
  ['/ux/shop', 'shop: /api/inventory/items + /api/workflows'],
  ['/it', 'IT hub: /api/jobs, the two station queues, /api/yard/status, /api/jobs/health'],
  ['/it/registry/subjects', 'subjects+classes: reads /api/subject-kinds + /api/classes, which HEALTHY keeps up'],
  ['/it/registry/dispatcher', 'dispatcher cascade: /api/dispatcher/rules'],
  ['/it/registry/rules', 'rules: /api/dispatcher/rules'],
  ['/it/operate/perf', 'gateway perf: /api/gateway/perf'],
  ['/it/operate/atlas', 'atlas: /api/views/stage-runs, /stage-durations, /api/stations'],
  ['/it/registry/step-plugins', 'step plugins: /api/jobs/step-plugins'],
  ['/it/kb', 'KB: its search reads fire on a query, not on load'],
  ['/it/design', 'design queue: /api/stations/design-review/queue'],
  ['/it/registry', 'workflow registry: /api/workflows'],
  ['/it/registry/new', 'new-workflow form: /api/workflows (a form, but it reads the registry to validate)'],
  ['/it/registry/seasonal-release', 'workflow detail: /api/workflows/{kind} + /versions'],
  ['/it/registry/policy', 'policy rules: /api/policy/rules'],
  ['/it/auth-admin', 'auth admin: its reads fire behind a tab'],
  ['/it/operate/bottlenecks', 'bottlenecks: /api/workflows'],
  ['/watchlist', 'watchlist: /api/people/accounts/risk-scores'],
  ['/hr', 'HR (bare alias): same as /ux/hr'],
]);

/// Force the outage. Runs AFTER installSmokeMocks, so it takes
/// precedence, and falls back to the healthy fixtures for HEALTHY.
async function installOutage(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route('**/api/**', async (route) => {
    const url = route.request().url();
    if (HEALTHY.some((re) => re.test(url))) return route.fallback();
    return route.fulfill({
      status: 500,
      contentType: 'application/json',
      body: JSON.stringify('the backend is down'),
    });
  });
}

type Seen = { route: string; markers: number; shell: boolean };

/// One shared page, one navigation per route — same rationale as
/// route-smoke: the browser keeps the on-the-fly bundle warm, and a full
/// goto wipes the previous route's JS state, so there is no effect bleed.
async function crawl(page: Page, routes: ReadonlyArray<string>): Promise<Seen[]> {
  await installOutage(page);
  const seen: Seen[] = [];
  for (const route of routes) {
    let shell = false;
    for (let attempt = 1; attempt <= 2 && !shell; attempt++) {
      try {
        await page.goto(route, { waitUntil: 'commit', timeout: 20_000 });
        await expect(page.locator('.app-shell')).toBeVisible({ timeout: 20_000 });
        shell = true;
      } catch {
        // Recorded as shell:false below if the retry also misses.
      }
    }
    // Let onMount effects and the (instant) mocked rejections settle so
    // the failure branch has actually rendered.
    if (shell) await page.waitForTimeout(700);
    const markers = shell ? await page.locator(FAILURE_MARKER).count() : 0;
    seen.push({ route, markers, shell });
  }
  return seen;
}

test.describe('the outage crawl — a surface cannot render a falsehood', () => {
  test('every asserted surface says a read failed when every read fails', async ({ page }) => {
    test.setTimeout(600_000);
    const asserted = ROUTES.filter((r) => !SILENT.has(r));
    const seen = await crawl(page, asserted);

    const mute = seen.filter((s) => s.markers === 0);
    expect(
      mute.map((s) => `${s.route}${s.shell ? '' : ' (shell never painted)'}`).sort(),
      `With every /api/** read returning 500, these surfaces showed no ` +
        `${FAILURE_MARKER} — so they are telling the operator something ` +
        `they do not know. Render the failure (the branch, not a lint: see ` +
        `src/data/remote.ts), or move the route to SILENT with what it reads.`,
    ).toEqual([]);
  });

  test('no surface on the SILENT list has quietly started reporting its failures', async ({ page }) => {
    test.setTimeout(600_000);
    const silent = ROUTES.filter((r) => SILENT.has(r));
    const seen = await crawl(page, silent);

    const fixed = seen.filter((s) => s.markers > 0);
    expect(
      fixed.map((s) => s.route).sort(),
      `These routes are listed in SILENT but now show ${FAILURE_MARKER} ` +
        `under a total outage. That is the fix landing — delete their ` +
        `lines from SILENT so the first test starts holding them. A ` +
        `roster that keeps excusing surfaces it no longer needs to is how ` +
        `this class survived four hand-fixes.`,
    ).toEqual([]);
  });

  test('no SILENT entry names a route that is not crawled', async () => {
    const crawled = new Set(ROUTES);
    const ghosts = [...SILENT.keys()].filter((r) => !crawled.has(r)).sort();
    expect(
      ghosts,
      'an excuse for a route no crawl visits reads as "known debt" while ' +
        'covering nothing — drop it, or fix the path',
    ).toEqual([]);
  });
});
