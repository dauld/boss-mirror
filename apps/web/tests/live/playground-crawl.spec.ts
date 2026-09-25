// The playground crawl — every catalogued surface, rendered from a REAL
// instance's data as a guest sees it, nightly (design 0e07ce64,
// 2026-09-18).
//
// WHAT IT PROVES that the mocked suite cannot. tests/mocked/route-smoke
// crawls the same roster under an adversarial fake backend and catches
// "the page throws on a shape the mock made up". This crawl opens the
// same roster against the playground — the brewery tenant, the sim
// ticking, real projections — and catches the other half: a page that
// throws on a shape the BACKEND actually produces (the 2026-05-22 pair:
// `extras = null` on a real catalog model, a real part whose detail
// route 404'd while the list linked to it). The 35-spec live suite this
// replaces asserted that class by hand-picking ids per page; here the
// roster is the ONE list both crawls read (tests/mocked/_routes.ts), so
// a surface added to the catalog is crawled live the next night without
// anyone remembering to.
//
// THE SESSION. The playground offers guest access (instances.toml
// `guest = "audit"`): `POST /api/auth/guest` mints a read-only session
// with no employee behind it — the same session a stranger clicking
// "Browse as a guest" gets. That is the crawl's identity on purpose:
// public data, a read-only role, nothing it can write.
//
// The gateway sets that cookie `Secure` (right for the edge, where the
// hostname is TLS-terminated by Cloudflare), and the crawl reaches the
// instance over plain HTTP by its in-cluster Service name — the same
// origin the tunnel proxies to. Measured 2026-09-18: the public
// hostname answers 302 to Cloudflare Access for an anonymous request,
// so the crawl cannot use it; and Chromium will not send a Secure
// cookie to an http: origin. So the minted cookie is re-planted on the
// context for the crawled origin, minus the flag. The value is the
// gateway's own, copied not retyped; only the transport attribute
// changes, and only for a URL that is already http.
//
// THE VERDICT. Same bar as route-smoke: a route is RED when the shell
// never paints or an uncaught exception fires while it is open;
// console.error is reported, not gated. Every red route is printed on
// its own line as `RED <route> <kind>: <text>` BEFORE the assertion
// fails, so the chore's packet (boss-chore.sh carries the check's
// output onto the `run` step) holds the list the daily judge opens one
// backlog-item per route from (infra/dispatcher/rules/
// file-backlog-items-on-playground-crawl-red.toml, backlog ac3270c7).
//
// EXPECTED console.error. A guest hitting an operator route is the
// product refusing correctly: the gateway answers 404 or 403, Chromium
// logs "Failed to load resource" as a console.error, and the page
// renders its refusal. Those are listed in EXPECTED_CONSOLE_ERRORS
// with that reason and printed apart from the rest, so the noise a
// reader scans is the noise nobody has explained. Not gated either
// way (decided on ac3270c7, 2026-09-18).

import { test, expect } from '@playwright/test';
import { ROUTES } from '../mocked/_routes';

const SESSION_COOKIE = 'boss_session';

type Issue = { route: string; kind: string; text: string };

/// The one reason every entry below carries: the crawl is a guest, and
/// these routes are an operator's. The refusal IS the product working.
const GUEST_REFUSED = 'a guest hitting an operator route is the product refusing correctly';

/// console.error lines the crawl expects, by route and HTTP status —
/// measured on the chore's first nights (2026-09-18). An entry matches
/// a resource-load error on that route carrying that status; anything
/// else on the route is still reported as unexplained noise.
const EXPECTED_CONSOLE_ERRORS: ReadonlyArray<{ route: string; status: number; reason: string }> = [
  { route: '/ux/marketing-assets/ma-1', status: 404, reason: GUEST_REFUSED },
  { route: '/it', status: 404, reason: GUEST_REFUSED },
  { route: '/it/codebase', status: 403, reason: GUEST_REFUSED },
  // Measured on the crawl's first scheduled run (packet 304960d9,
  // 2026-09-19 04:45Z): the one unexplained line, the same class as
  // its sibling /it/codebase.
  { route: '/it/design/codebase', status: 403, reason: GUEST_REFUSED },
];

/// PURE: the expected entry a console.error matches, if any.
function expectedFor(issue: Issue): { reason: string } | undefined {
  return EXPECTED_CONSOLE_ERRORS.find(
    (e) => e.route === issue.route && issue.text.includes(`status of ${e.status}`),
  );
}

/// Mint the guest session and plant it for the crawled origin.
async function guestSession(page: import('@playwright/test').Page, origin: string): Promise<void> {
  const minted = await page.request.post('/api/auth/guest');
  expect(
    minted.ok(),
    `POST /api/auth/guest answered ${minted.status()} — the instance at ${origin} does not offer ` +
      'guest access (instances.toml guest = "audit"), so there is no session to crawl as',
  ).toBe(true);
  const setCookie = minted
    .headersArray()
    .find((h) => h.name.toLowerCase() === 'set-cookie' && h.value.startsWith(`${SESSION_COOKIE}=`));
  expect(setCookie, `the guest mint set no ${SESSION_COOKIE} cookie`).toBeDefined();
  const value = setCookie!.value.slice(SESSION_COOKIE.length + 1).split(';')[0]!;
  await page.context().addCookies([
    {
      name: SESSION_COOKIE,
      value,
      url: origin,
      httpOnly: true,
      sameSite: 'Lax',
      secure: origin.startsWith('https:'),
    },
  ]);
}

test('every catalogued route renders the instance as a guest', async ({ page, baseURL }) => {
  const origin = new URL(baseURL!).origin;
  await guestSession(page, origin);

  const issues: Issue[] = [];
  let route = '';
  page.on('pageerror', (e) => issues.push({ route, kind: 'pageerror', text: e.message }));
  page.on('console', (m) => {
    if (m.type() === 'error') issues.push({ route, kind: 'console.error', text: m.text() });
  });

  // The session must actually hold, or every route below renders the
  // sign-in page and the crawl reports on a surface it never opened.
  await page.goto('/', { waitUntil: 'commit', timeout: 20_000 });
  await expect(page.locator('.app-shell'), 'the guest session did not take: / has no shell')
    .toBeVisible({ timeout: 20_000 });
  const session = await page.request.get('/api/session');
  expect(session.status(), '/api/session answers the planted guest cookie').toBe(200);

  for (const r of ROUTES) {
    route = r;
    let shellOk = false;
    for (let attempt = 1; attempt <= 2 && !shellOk; attempt++) {
      try {
        await page.goto(r, { waitUntil: 'commit', timeout: 20_000 });
        await expect(page.locator('.app-shell')).toBeVisible({ timeout: 20_000 });
        shellOk = true;
      } catch (e) {
        if (attempt === 2) {
          issues.push({ route: r, kind: 'no-shell', text: e instanceof Error ? e.message : String(e) });
        }
      }
    }
    // Real fetches are not instant: give the page's reads time to land
    // and render, so a crash in a data path fires while we listen.
    if (shellOk) await page.waitForTimeout(1_500);
  }

  const crashes = issues.filter((i) => i.kind !== 'console.error');
  const noise = issues.filter((i) => i.kind === 'console.error' && !expectedFor(i));
  const expected = issues.filter((i) => i.kind === 'console.error' && expectedFor(i));
  console.log(
    `crawled ${ROUTES.length} routes at ${origin}: ${crashes.length} red, ${noise.length} console.error, ${expected.length} expected console.error`,
  );
  // One line per finding: a multi-line stack would break the one-item-
  // per-line contract the packet's reader relies on.
  const oneLine = (t: string) => t.replace(/\s*\n\s*/g, ' | ').trim();
  for (const i of noise) console.log(`  console.error [${i.route}] ${oneLine(i.text)}`);
  for (const i of expected) {
    console.log(`  expected console.error [${i.route}] ${oneLine(i.text)} — ${expectedFor(i)!.reason}`);
  }
  for (const i of crashes) console.log(`RED ${i.route} ${i.kind}: ${oneLine(i.text)}`);
  expect(
    crashes,
    `${crashes.length} of ${ROUTES.length} routes red at ${origin} — the RED lines above name them`,
  ).toEqual([]);
});
