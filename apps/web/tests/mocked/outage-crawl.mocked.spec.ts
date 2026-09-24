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

import { test, expect, type Page, type Request } from '@playwright/test';
import { SHELL_ENDPOINTS, installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER, ROUTES } from './_routes';

/// The endpoints that stay up — SHELL_ENDPOINTS, defined beside the
/// fixtures in _smokeMocks.ts since the interaction crawl's empty leg
/// (f2b8a01c) needs the same set. The app shell cannot paint without
/// identity and the nav cannot resolve visibility without the taxonomy
/// registries, and a crawl where nothing renders measures nothing.
/// Every other endpoint — including the chrome's own /api/jobs/live and
/// /api/jobs/step-types — is broken, which is what makes the assertion
/// below meaningful: those two break on all 51 routes, and the 34 silent
/// ones still show no marker, so a marker is never chrome's.
const HEALTHY = SHELL_ENDPOINTS;

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
///
/// 2026-09-24, sweep c3e4edcc: 28 entries → 12. Sixteen routes painted
/// their failure in words without the marker (or, on /it/operate/audit,
/// four such lines); each line now wears it, and the header above it
/// states no count it does not have. What is left is either a read that
/// never fires under the crawl's empty session or behind a click, or a
/// page with no failure line at all (/ux/shop, /it/registry/new) — a
/// different defect from a line without the marker.
const SILENT: ReadonlyMap<string, string> = new Map([
  ['/', 'My Day (the bare alias): same as /ux/me'],
  ['/ux/me', 'My Day: identity-keyed reads never fire under the empty mocked session (its two failure lines wear the marker)'],
  ['/ux/inbox', 'inbox: /api/messages/inbox/{id} never fires under the empty mocked session'],
  ['/ux/views', 'views composer: /api/views never fires under the empty mocked session — load() waits for a viewer id'],
  ['/ux/calendar/me', 'my calendar: identity-keyed reads never fire under the empty mocked session'],
  ['/ux/hr', 'HR: the workflow + step reads fire on the Workflows tab, not on load (see false-empty.mocked.spec.ts, which pins them)'],
  ['/ux/shop', 'shop: a failed /api/inventory/items falls back to "check availability" and paints no failure line at all'],
  ['/it/registry/subjects', 'subjects+classes: reads /api/subject-kinds + /api/classes, which HEALTHY keeps up'],
  ['/it/kb', 'KB: its search reads fire on a query, not on load'],
  ['/it/registry/new', 'new-workflow form: /api/workflows + /api/subject-kinds only fill suggestion lists; a failure keeps the defaults and paints no line'],
  ['/it/auth-admin', 'auth admin: its reads fire behind a tab'],
  ['/hr', 'HR (bare alias): same as /ux/hr'],
]);

/// PER-READ OUTAGES: a shell read that is also a route's OWN data, broken
/// on that route alone (backlog 25ad5042). HEALTHY keeps /api/people up
/// on every route, so /ux/people, whose one read IS /api/people, was
/// crawled with its roster loaded and sat on SILENT, and nothing failed
/// the read and asserted the page's roster-failure line. A regression
/// to "No employees match those filters." would have passed every spec.
/// Breaking it on the one route that owns it keeps the other 50 crawled
/// with the shell they need. The session read beside it tolerates the
/// failure (loadSession falls through on an empty roster), and the
/// crawl's mocked session is unauthenticated anyway, so the shell still
/// paints.
const ALSO_BROKEN: ReadonlyMap<string, ReadonlyArray<RegExp>> = new Map([
  ['/ux/people', [/\/api\/people$/]],
]);

/// How late a route's OWN read is issued — the manual-page fix's figure
/// (0eda772f), three times the quiet window the crawl used to settle on.
const LATE_READ_MS = 750;

/// A ROUTE'S OWN READ, ISSUED LATE on purpose (backlog 6592caf7). The
/// crawl redded gate 59ed3414 on /ux/people with no marker, and passed on
/// a re-gate of the same sha: the page asks for /api/people from an
/// $effect after mount, and under load the shell's reads can all answer
/// and sit quiet for longer than the old 250 ms window before that
/// request is even made — so the crawl counted a page whose read had not
/// been asked for yet. Holding each ALSO_BROKEN read back before it
/// leaves the page makes that the ordinary case, so a crawl that counts
/// before the route's own read fails every run, not one in a busy hour.
async function issueOwnReadsLate(page: Page): Promise<void> {
  const late = [...ALSO_BROKEN].map(([path, reads]) => ({
    path,
    reads: reads.map((re) => re.source),
  }));
  await page.addInitScript(
    ({ late, ms }) => {
      const mine = late.find((l) => l.path === location.pathname);
      if (!mine) return;
      const reads = mine.reads.map((s) => new RegExp(s));
      const real = window.fetch.bind(window);
      window.fetch = async (input, init) => {
        const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
        const path = new URL(url, location.href).pathname;
        if (reads.some((re) => re.test(path))) await new Promise((ok) => setTimeout(ok, ms));
        return real(input, init);
      };
    },
    { late, ms: LATE_READ_MS },
  );
}

/// Force the outage. Runs AFTER installSmokeMocks, so it takes
/// precedence, and falls back to the healthy fixtures for HEALTHY —
/// except the reads ALSO_BROKEN names for the route being crawled, which
/// `current` answers at the moment each request is made.
async function installOutage(page: Page, current: () => string): Promise<void> {
  await installSmokeMocks(page);
  await issueOwnReadsLate(page);
  await page.route('**/api/**', async (route) => {
    const url = route.request().url();
    const broken = ALSO_BROKEN.get(current()) ?? [];
    const healthy = HEALTHY.some((re) => re.test(url)) && !broken.some((re) => re.test(url));
    if (healthy) return route.fallback();
    return route.fulfill({
      status: 500,
      contentType: 'application/json',
      body: JSON.stringify('the backend is down'),
    });
  });
}

/// `bare` is every marker on the route NOT drawn as the failed read's red
/// rail (backlog 6f471ff6, car 3), as its computed left edge — so a page
/// whose own class takes the rail back is named here, by route.
type Seen = { route: string; markers: number; shell: boolean; bare: string[] };

/// The rail as a browser computes it: 8px of the troubled plate's red
/// (#C8283D, --troubled).
const RAIL = '8px solid rgb(200, 40, 61)';

/// THE READS A ROUTE FIRES, OBSERVED (backlog e6bc776b). The crawl used
/// to give a painted shell a flat 700 ms for "onMount effects and the
/// (instant) mocked rejections" to settle, then count markers. 700 ms is
/// a guess about a quiet pod, and under load this suite runs ten times
/// slower with nothing about the page changed (measured 2026-09-22: the
/// same four specs go from 0.45-0.73 s each quiet to 4.8-7.8 s at 10x
/// CPU oversubscription). A route whose rejection had not rendered yet
/// counted zero markers and was reported BY NAME as a surface telling
/// the operator something it does not know — a false finding shaped
/// exactly like a real one, which is the worst failure mode this file
/// has. So the wait is on the reads themselves: they are answered, not
/// assumed to have been.
type Reads = { inFlight: Set<Request>; issued: number };

/// Reads are counted per ROUTE, so `issued` is zeroed at each goto.
function watchReads(page: Page): Reads {
  const reads: Reads = { inFlight: new Set(), issued: 0 };
  page.on('request', (r) => {
    if (!r.url().includes('/api/')) return;
    reads.inFlight.add(r);
    reads.issued += 1;
  });
  page.on('requestfinished', (r) => reads.inFlight.delete(r));
  page.on('requestfailed', (r) => reads.inFlight.delete(r));
  return reads;
}

/// Total budget for one route to answer its reads and paint what they
/// mean. Reached only by a route still fetching after this long; the
/// normal path leaves in a few hundred ms, FASTER than the sleep it
/// replaces, because it leaves when the work is done rather than when
/// the clock says it should be.
const SETTLE_BUDGET_MS = 15_000;
/// A mount can fire a second read once the first answers, so an empty
/// in-flight set is only quiescence if it STAYS empty this long.
const QUIET_MS = 250;
/// A shell painted but no read issued yet is EARLY, not quiet — onMount
/// has not run. Bounded so a genuinely read-free route costs this much
/// and not the whole budget.
const FIRST_READ_MS = 2_000;
/// The last answer lands in JS; the render it causes is the next frame.
const PAINT_MS = 100;

async function settle(page: Page, reads: Reads): Promise<void> {
  const start = Date.now();
  const deadline = start + SETTLE_BUDGET_MS;
  let quietSince = 0;
  while (Date.now() < deadline) {
    const started = reads.issued > 0 || Date.now() - start >= FIRST_READ_MS;
    if (reads.inFlight.size > 0 || !started) quietSince = 0;
    else if (quietSince === 0) quietSince = Date.now();
    else if (Date.now() - quietSince >= QUIET_MS) break;
    await page.waitForTimeout(25);
  }
  await page.waitForTimeout(PAINT_MS);
}

/// WHAT A QUIET WINDOW CANNOT SEE (backlog 6592caf7). settle() leaves
/// once the reads ISSUED so far have answered and nothing new has left
/// for QUIET_MS. A read the page has not asked for yet is invisible to
/// it: /ux/people asks for its roster from an $effect after mount, and on
/// gate 59ed3414 the shell's own reads answered and sat quiet for longer
/// than the window before that request was made, so the crawl counted a
/// page mid-mount and named it mute. So the crawl that ASSERTS a marker
/// waits for one first — the page's rendered answer, under the same
/// budget — and only then settles for the rest. A page that never draws
/// one still counts zero and is still named, one budget later; only the
/// page that had not drawn it YET stops being reported as a falsehood.
/// The SILENT crawl cannot wait on a marker it expects not to see, and
/// its miss is the opposite direction — a fixed page counted before its
/// marker renders stays on the list one more run, which reds nothing.
type Await = 'marker' | 'quiet';

/// One shared page, one navigation per route — same rationale as
/// route-smoke: the browser keeps the on-the-fly bundle warm, and a full
/// goto wipes the previous route's JS state, so there is no effect bleed.
async function crawl(page: Page, routes: ReadonlyArray<string>, until: Await): Promise<Seen[]> {
  let current = '';
  await installOutage(page, () => current);
  const reads = watchReads(page);
  const seen: Seen[] = [];
  for (const route of routes) {
    current = route;
    let shell = false;
    for (let attempt = 1; attempt <= 2 && !shell; attempt++) {
      try {
        reads.inFlight.clear();
        reads.issued = 0;
        await page.goto(route, { waitUntil: 'commit', timeout: 20_000 });
        await expect(page.locator('.app-shell')).toBeVisible({ timeout: 20_000 });
        shell = true;
      } catch {
        // Recorded as shell:false below if the retry also misses.
      }
    }
    // The asserted crawl waits for the page's own answer first; a miss is
    // not thrown here but counted below, so the failure names the route.
    if (shell && until === 'marker') {
      await page
        .locator(FAILURE_MARKER)
        .first()
        .waitFor({ state: 'attached', timeout: SETTLE_BUDGET_MS })
        .catch(() => undefined);
    }
    // Let onMount's reads be ANSWERED and the failure branch render.
    if (shell) await settle(page, reads);
    const markers = shell ? await page.locator(FAILURE_MARKER).count() : 0;
    const edges = shell
      ? await page.locator(FAILURE_MARKER).evaluateAll((els) =>
          els.map((e) => {
            const s = getComputedStyle(e);
            return `${s.borderLeftWidth} ${s.borderLeftStyle} ${s.borderLeftColor}`;
          }),
        )
      : [];
    seen.push({ route, markers, shell, bare: edges.filter((e) => e !== RAIL) });
  }
  return seen;
}

test.describe('the outage crawl — a surface cannot render a falsehood', () => {
  test('every asserted surface says a read failed when every read fails', async ({ page }) => {
    test.setTimeout(600_000);
    const asserted = ROUTES.filter((r) => !SILENT.has(r));
    const seen = await crawl(page, asserted, 'marker');

    const mute = seen.filter((s) => s.markers === 0);
    expect(
      mute.map((s) => `${s.route}${s.shell ? '' : ' (shell never painted)'}`).sort(),
      `With every /api/** read returning 500, these surfaces showed no ` +
        `${FAILURE_MARKER} — so they are telling the operator something ` +
        `they do not know. Render the failure (the branch, not a lint: see ` +
        `src/data/remote.ts), or move the route to SILENT with what it reads.`,
    ).toEqual([]);

    // What it SAYS it in: the failed read is a red rail on every page that
    // names the marker, because the marker's own rule draws it — a failure
    // set as red words on an empty's line reads as an empty.
    const bare = seen.filter((s) => s.bare.length > 0);
    expect(
      bare.map((s) => `${s.route}: ${s.bare.join(', ')}`).sort(),
      `These surfaces show ${FAILURE_MARKER} without the ${RAIL} rail — ` +
        `a page rule outranks the marker's (styles.css, the failed read).`,
    ).toEqual([]);
  });

  test('no surface on the SILENT list has quietly started reporting its failures', async ({ page }) => {
    test.setTimeout(600_000);
    const silent = ROUTES.filter((r) => SILENT.has(r));
    const seen = await crawl(page, silent, 'quiet');

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

  test('every per-read outage breaks a crawled, asserted route', async () => {
    const crawled = new Set(ROUTES);
    const idle = [...ALSO_BROKEN.keys()].filter((r) => !crawled.has(r) || SILENT.has(r)).sort();
    expect(
      idle,
      'a per-read outage on a route no crawl visits, or on a SILENT one the ' +
        'first test never asserts, breaks a read and checks nothing — drop ' +
        'it, fix the path, or take the route off SILENT',
    ).toEqual([]);
  });
});
