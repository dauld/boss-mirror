// THE INTERACTION CRAWL — backlog f2b8a01c, decided on design 0e07ce64.
//
// route-smoke asks every surface "do you survive?"; outage-crawl asks
// "what do you SAY when your read fails?". This spec asks what a page
// audit asks by hand: does every control on the page DO something, and
// is what it does true? For EVERY crawled route (ROUTES, pinned to the
// catalog by route-smoke's drift test; DEFERRED is shared with it):
//
//   (a) every in-app a[href] is a path the router serves — an href
//       nothing serves is a red naming the page and the link;
//   (b) every button / [role=button] that is not destructive by label
//       (DESTRUCTIVE below) is clicked, and the page must not throw
//       and must answer observably — a navigation to a served route,
//       a dialog, an alert/status line, a request it issued, or a
//       change in what is painted;
//   (c) after each navigation a click caused, history.back() returns
//       to the route the click was made on;
//   (d) the empty-backend leg: with every read answering `[]` (the
//       shell's endpoints and the object-shaped fixtures kept, as
//       outage-crawl keeps them), the page paints an honest empty
//       state — no crash and no FAILURE_MARKER, because nothing failed;
//   (e) the refused-write leg: the same click sequence with every
//       write answered 403 and destructive controls included; a click
//       that issued a write must leave the refusal visible on the page
//       — step-write-failures.mocked.spec.ts, generalised.
//
// WHAT (a) CANNOT SEE — backlog d063c290. Leg (a) judges the hrefs the
// mocked backend RENDERS, on the routes in ROUTES (minus DEFERRED), and
// "served" means only that the router does not fall through to its
// catch-all. So three kinds of dead link pass it green:
//
//   - a link nobody generated: a kind whose data is absent under mocks
//     draws no row, so its href is never on any page (ticket and
//     opportunity had no record anywhere in the tree);
//   - a link on a page the crawl does not open: detail pages beyond
//     the two ROUTES seeds, and every DEFERRED route;
//   - a link a greedy wildcard eats: an agreement href parsed as the
//     ACCOUNT page with accountId 'agreements/<id>' — "served", by the
//     wrong page.
//
// All three shapes were live at once, on four entity kinds, and this
// crawl stayed green through every one of them until 38d4e458 retired
// them. What covers them is the static pin
// apps/web/src/entity-href-routes.test.ts: for EVERY kind in
// ENTITY_KINDS it asserts parseRoute(entityHref(kind, id)) is the route
// that kind belongs to — no data, no page, no crawl needed. A green here
// means every RENDERED link lands; a link generator the crawl never
// sees rendered is that pin's job. The pin also holds this paragraph's
// pointer and the title below to their claims, so neither can drift.
//
// Findings name route + control + what happened. A finding on main
// today is a real gap: it is either fixed in the page or listed in
// KNOWN_GAPS with its text, and KNOWN_GAPS is pinned in BOTH
// directions the way outage-crawl's SILENT is — a gap that closes must
// have its line deleted, so the list can only shrink.
//
// MEASURED 2026-09-18 on main at #452: 55 routes, 244 links, 145
// distinct page controls + 6 chrome controls clicked on the main leg
// and 151 on the refused leg (7 of them issuing a write), ~220 s of
// browser time for all three legs on one worker. So the catalog is
// split into SHARDS tests that Playwright runs in parallel workers,
// each carrying all three legs for its routes: the total is the same,
// the wall time is ~65 s. The first run found 10 gaps (listed in the
// commit that landed this file); three were fixed with it and the
// rest are KNOWN_GAPS below.
//
// Run: bunx playwright test -c playwright.mocked.config.ts tests/mocked/interaction-crawl.mocked.spec.ts
//   CRAWL_ROUTES=/ux/inbox,/it   crawl only these routes
//   CRAWL_VERBOSE=1              print every click's outcome

import { test, expect, type Page, type Request } from '@playwright/test';
import { DISPATCHER_RULES, OBJECT_ENDPOINTS, SHELL_ENDPOINTS, VIEW_RESULTS, installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER, LANDING_FALLBACK, ROUTES } from './_routes';
import { parseRoute } from '../../src/router';

// ---------------------------------------------------------------------------
// (a) served hrefs
// ---------------------------------------------------------------------------

/// Paths another layer answers, not the SPA router: the gateway's
/// /api/*, the simulator (its own SPA), plugin bundles, and static
/// files (the KB's diagrams are `<a href>`s to bundled svgs, served
/// under /_bun/asset/ by the dev-server and a hashed path by the
/// build). A link there cannot be judged by the router, so it is
/// counted and not judged.
const OFF_SPA: ReadonlyArray<RegExp> = [
  /^\/api\//,
  /^\/simulator(\/|$)/,
  /^\/plugins\//,
  /^\/_bun\//,
  /\.(svg|png|jpg|jpeg|gif|webp|pdf|md|json|csv|txt|js|css)$/,
];

/// Does the SPA router serve this path? parseRoute falls through to
/// `home` for an unknown path and to `systemYard` for an unknown
/// /it/* path, so a served path is one that parses to anything else.
/// The router is the definition of "served" — the nav catalog is the
/// sidebar, and a detail page (/ux/jobs/{id}) is served without a
/// catalog row — so the check reads the router rather than a copy of
/// its patterns (CLAUDE.md §9a).
function servedBySpa(href: string): boolean {
  const [path = '', query = ''] = href.split('#')[0]!.split('?');
  // parseRoute reads the query string off `window` for /jobs and
  // /search. This is Node, so give it the one field it reads.
  (globalThis as { window?: unknown }).window = { location: { search: query ? `?${query}` : '' } };
  const r = parseRoute(path);
  if (r.kind === 'home') return false;
  if (r.kind === 'systemYard') return path.replace(/\/$/, '') === '/it';
  return true;
}

// ---------------------------------------------------------------------------
// (b)/(e) controls
// ---------------------------------------------------------------------------

/// Controls the main leg does not click, by label. Each word names an
/// action that changes the record in a way a crawler has no business
/// doing, even against a mock: the click leg proves a control answers,
/// and a delete answering is not what anyone wants proven by default.
/// These controls get the refused-write leg only, where every write is
/// answered 403 and what is proven is that the refusal shows. A closed
/// list: adding a word is a decision.
const DESTRUCTIVE: ReadonlyArray<string> = ['delete', 'retire', 'abort', 'cancel'];

const isDestructive = (label: string): boolean =>
  DESTRUCTIVE.some((w) => label.toLowerCase().includes(w));

/// The text a refused write is answered with. The page must show it, or
/// the status — a refusal that changes nothing on screen is the silence
/// step-write-failures found on three step surfaces.
const REFUSAL = 'refused by policy: the interaction crawl declined this write';

/// Writes that are telemetry, not work: their failure is silent to the
/// operator BY DESIGN (surface-opens.ts: "a measurement must never get
/// in the way of the thing it measures"), so a 403 on them is not a
/// refusal the page owes anyone, and issuing one is not a response.
const SILENT_WRITES: ReadonlyArray<RegExp> = [/\/api\/surface-opens$/];

/// Class tokens and aria flags that say "this control is currently
/// selected". Stripped from a control's key so a filter button is one
/// control whether or not it is the active one — the first draft keyed
/// on the full class string, clicked every filter a second time in its
/// active state, and reported the idempotent click as a gap on 20
/// routes. A click on a selected control is idempotent by design, so
/// its silence is honest.
const STATE_TOKEN = /active|selected|current|pressed|open|checked|(^|-)on$/;

type Control = Readonly<{
  key: string;
  label: string;
  index: number;
  tag: string;
  submit: boolean;
  selected: boolean;
}>;

/// Where a route's own controls live: the shell's content slot, plus
/// any dialog (a modal opened from the page mounts wherever it likes).
/// Everything outside — the perspective bar, the sidebar, the feedback
/// trigger, the persona switcher — is CHROME, identical on every route,
/// and is crawled once by its own test rather than 55 times: the first
/// run clicked Sign out on every route and reported one gap 55 ways.
const DIALOGS = '[role="dialog"], [role="alertdialog"], dialog[open]';
const PAGE_SCOPE = `.shell-content, ${DIALOGS}`;
/// 'dialog' is the scope while a dialog is open: its controls are the
/// only ones a user can reach, and the ones behind the overlay would
/// each spend a click timeout proving it.
type Scope = 'page' | 'chrome' | 'dialog';

/// How long one control gets to become clickable. It was 3 000 ms until
/// 2026-09-18, and that redded train #461's gate at 22:00Z with a
/// finding that was the HOST, not the page: "[/ux/views] id: not
/// clickable: locator.click: Timeout 3000ms exceeded" and "[/ux/me]
/// MORE ▾: not clickable …" while cargo tests ran beside the web-suite
/// (the crawl took 2.0 min); the same flake fired once on the dev pod
/// under builder load the same day — the second web-suite flake at a
/// head in a day (backlog ac3270c7). A control that is genuinely not
/// clickable still fails, just later; a control that is merely waiting
/// on a starved renderer no longer reds a train.
const CLICK_TIMEOUT_MS = 15_000;

/// How long a click that navigates nowhere gets to answer before the
/// page is read. It is the TIMEOUT of a wait for the click's
/// navigation, never a sleep the navigation must fit inside (see the
/// race in clickLeg), and it starts only once every request the click
/// issued has been answered — that wait has its own bound, generous
/// because a mock answers in-process and only a starved host is slow.
const SETTLE_MS = 250;
const ANSWER_TIMEOUT_MS = 10_000;

/// Every visible, enabled button in scope, keyed so the same control
/// rendered twice (a row action repeated per row) is clicked once:
/// identical label + tag + stable classes is the same code path.
async function controls(page: Page, scope: Scope): Promise<ReadonlyArray<Control>> {
  return page.locator('button, [role="button"]').evaluateAll((els, [stateToken, pageScope, dialogs, scope]) => {
    const state = new RegExp(stateToken);
    return els.flatMap((el, index) => {
      const h = el as HTMLElement;
      const style = getComputedStyle(h);
      const rect = h.getBoundingClientRect();
      const visible = style.display !== 'none' && style.visibility !== 'hidden' && rect.width > 0 && rect.height > 0;
      const disabled = (h as HTMLButtonElement).disabled || h.getAttribute('aria-disabled') === 'true';
      if (!visible || disabled) return [];
      if (scope === 'page' && !h.closest(pageScope)) return [];
      if (scope === 'dialog' && !h.closest(dialogs)) return [];
      const label = (h.getAttribute('aria-label') ?? h.getAttribute('title') ?? h.innerText ?? '').trim().replace(/\s+/g, ' ').slice(0, 60);
      const tag = h.tagName.toLowerCase();
      const submit = tag === 'button' && (h as HTMLButtonElement).type === 'submit' && !!h.closest('form');
      const classes = (h.getAttribute('class') ?? '').split(/\s+/).filter((c) => c && !state.test(c));
      const selected =
        state.test(h.getAttribute('class') ?? '') ||
        ['aria-pressed', 'aria-selected', 'aria-current', 'aria-expanded'].some((a) => h.getAttribute(a) === 'true');
      return [{ key: `${tag}|${label}|${classes.join(' ')}`, label: label || `<${tag} #${index}>`, index, tag, submit, selected }];
    });
  }, [STATE_TOKEN.source, PAGE_SCOPE, DIALOGS, scope] as const);
}

/// A cheap fingerprint of what is painted — the markup plus every form
/// control's live value (a property, invisible to innerHTML: "Create Ad
/// Hoc Job" with the form already open changes only the kind field) —
/// so "the click changed the page" is measurable without knowing what
/// the page is.
async function paint(page: Page): Promise<string> {
  try {
    return await paintNow(page);
  } catch {
    // A document navigation began after the click settled (clickLeg
    // awaits the click's answers and races its navigation, so this is
    // the late edge, not the rule): the read landed on a destroyed
    // context. Read the document that replaced it, once, rather than
    // red the crawl on the host's timing.
    await page.waitForLoadState('domcontentloaded', { timeout: 10_000 }).catch(() => undefined);
    return paintNow(page);
  }
}

async function paintNow(page: Page): Promise<string> {
  return page.evaluate(() => {
    const values = [...document.querySelectorAll('input, select, textarea')]
      .map((f) => (f as HTMLInputElement).type === 'checkbox' || (f as HTMLInputElement).type === 'radio'
        ? String((f as HTMLInputElement).checked)
        : (f as HTMLInputElement).value)
      .join('\u0001');
    const s = document.body.innerHTML + '\u0002' + values;
    let h = 5381;
    for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) | 0;
    return `${s.length}:${h}`;
  });
}

const RESPONSE_MARKERS = `${DIALOGS}, [role="alert"], [role="status"]`;

async function open(page: Page, route: string): Promise<boolean> {
  for (let attempt = 1; attempt <= 2; attempt++) {
    try {
      await page.goto(route, { waitUntil: 'commit', timeout: 20_000 });
      await expect(page.locator('.app-shell')).toBeVisible({ timeout: 20_000 });
      await page.waitForTimeout(400);
      return true;
    } catch {
      // Retried once, as route-smoke does; a real failure misses both.
    }
  }
  return false;
}

const pathOf = (page: Page): string => new URL(page.url()).pathname;

type Finding = Readonly<{ route: string; control: string; what: string }>;
type Tally = { routes: number; links: number; clicks: number; refusedClicks: number; writes: number };

/// The legs' mock switches, flipped between legs on ONE page so the
/// smoke fixtures are installed once and layered, never copied.
type Mode = { refuse: boolean; empty: boolean };

/// Object-shaped reads the empty leg does not answer `[]`: the
/// OBJECT_ENDPOINTS fall through to their smoke fixture, which is
/// already the empty object (live counts, the yard, perf) or the seeded
/// detail the two detail routes exist to render; the two fixtures that
/// carry one row (the adversarial shape) are emptied here.
const EMPTIED: ReadonlyArray<readonly [RegExp, unknown]> = [
  [DISPATCHER_RULES, { rules: [], handler_emits: {}, system_edges: [] }],
  [VIEW_RESULTS, { view_id: 'view-1', source: 'jobs', layout: 'table', rows: [], matched: 0, truncated: false }],
];

async function installLegs(page: Page, mode: Mode): Promise<void> {
  await installSmokeMocks(page);
  await page.route('**/api/**', async (r) => {
    const url = r.request().url();
    const write = r.request().method() !== 'GET' && !SILENT_WRITES.some((re) => re.test(url));
    if (mode.refuse && write) {
      return r.fulfill({ status: 403, contentType: 'application/json', body: JSON.stringify({ error: REFUSAL }) });
    }
    if (mode.empty && !write && !SHELL_ENDPOINTS.some((re) => re.test(url))) {
      const emptied = EMPTIED.find(([re]) => re.test(url));
      if (emptied) return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(emptied[1]) });
      if (!OBJECT_ENDPOINTS.some((re) => re.test(url))) return r.fulfill({ status: 200, contentType: 'application/json', body: '[]' });
    }
    return r.fallback();
  });
}

/// The click legs over one route. `leg: 'main'` is (a)+(b)+(c) with
/// destructive controls skipped; `leg: 'refused'` is (e): the same
/// sequence, destructive controls included, every write 403'd, and the
/// refusal asserted after any click that issued one.
async function clickLeg(
  page: Page,
  route: string,
  leg: 'main' | 'refused',
  scope: Scope,
  tally: Tally,
  errors: string[],
): Promise<Finding[]> {
  const findings: Finding[] = [];
  // The route's OPEN is judged before any of its controls: the one
  // pageerror listener is on the page from before the first goto, so
  // a throw while the route loads lands in `errors` — and until
  // 2026-09-18 nothing read it there. The first click cleared it, and
  // a route with no controls never read it at all, so the
  // /it/registry/dispatcher crash the nightly playground crawl found
  // (backlog ee86a789) was invisible here even with the reproducing
  // fixture installed. A load-time throw is a finding of its own kind,
  // named `page (load)`, reported once (on the main leg; the refused
  // leg opens the same page again).
  errors.length = 0;
  if (!(await open(page, route))) {
    findings.push({ route, control: `page (${leg} leg)`, what: 'shell never painted' });
    return findings;
  }
  if (leg === 'main') {
    tally.routes += 1;
    if (errors.length) findings.push({ route, control: 'page (load)', what: `pageerror: ${errors.join(' | ')}` });
  }

  // (a) every in-app href is served.
  if (leg === 'main' && scope === 'page') {
    const hrefs = await page.locator('a[href]').evaluateAll((els) => els.map((a) => a.getAttribute('href') ?? ''));
    for (const href of new Set(hrefs)) {
      if (!href.startsWith('/') || href.startsWith('//')) continue;
      tally.links += 1;
      if (OFF_SPA.some((re) => re.test(href.split(/[?#]/)[0]!))) continue;
      if (!servedBySpa(href)) findings.push({ route, control: `link ${href}`, what: 'no route serves it (the router falls through to its landing)' });
    }
  }

  // (b)+(c), or (e): click every control once.
  const clicked = new Set<string>();
  let nativeDialog: string | null = null;
  const onDialog = (d: { type(): string; accept(): Promise<void>; dismiss(): Promise<void> }): void => {
    nativeDialog = d.type();
    // The refused leg accepts a confirm so the write it guards is
    // issued and refused; the main leg dismisses it.
    void (leg === 'refused' ? d.accept() : d.dismiss());
  };
  page.on('dialog', onDialog);
  let requests: string[] = [];
  let writes: string[] = [];
  // Every API request the page has issued and not yet been answered
  // on. A click is judged only once this is empty (bounded): what the
  // page does with an answer — paint a refusal, navigate after a
  // confirmed sign-out — cannot be read before the answer arrives.
  const inFlight = new Set<Request>();
  const onRequest = (r: Request): void => {
    const url = r.url();
    if (!url.includes('/api/')) return;
    inFlight.add(r);
    if (SILENT_WRITES.some((re) => re.test(url))) return;
    const line = `${r.method()} ${new URL(url).pathname}`;
    requests.push(line);
    if (r.method() !== 'GET') writes.push(line);
  };
  const onAnswered = (r: Request): void => {
    inFlight.delete(r);
  };
  page.on('request', onRequest);
  page.on('requestfinished', onAnswered);
  page.on('requestfailed', onAnswered);

  for (let guard = 0; guard < 120; guard++) {
    // While a dialog is open, its controls are the page; once they are
    // all clicked, close it (Escape; a dialog that stays gets the page
    // reopened) and carry on with the page beneath.
    const dialogOpen = (await page.locator(DIALOGS).count()) > 0;
    const next = (await controls(page, dialogOpen ? 'dialog' : scope)).find((c) => !clicked.has(c.key) && (leg === 'refused' || !isDestructive(c.label)));
    if (!next && dialogOpen) {
      await page.keyboard.press('Escape');
      await page.waitForTimeout(150);
      if ((await page.locator(DIALOGS).count()) > 0 && !(await open(page, route))) break;
      continue;
    }
    if (!next) break;
    clicked.add(next.key);
    if (leg === 'main') tally.clicks += 1;
    else tally.refusedClicks += 1;
    const before = await paint(page);
    const markersBefore = await page.locator(RESPONSE_MARKERS).count();
    errors.length = 0;
    requests = [];
    writes = [];
    nativeDialog = null;
    const urlBefore = page.url();
    const historyBefore = await page.evaluate(() => history.length).catch(() => 0);
    const target = page.locator('button, [role="button"]').nth(next.index);
    try {
      // A non-<button> control falls through to dispatchEvent below when
      // the real click misses, so it keeps the short wait: fifteen
      // seconds per SVG track before a click that lands anyway is the
      // crawl's own time, not a finding.
      await target.click({ timeout: next.tag === 'button' ? CLICK_TIMEOUT_MS : 3_000 });
    } catch (e) {
      // A `<g role="button">` in an SVG (the yard map's track) has no
      // fill, so the centre of its box hits the canvas beneath and the
      // real click never lands; the handler is still the thing under
      // test, so it is reached the way the keyboard reaches it.
      if (next.tag !== 'button') {
        await target.dispatchEvent('click').catch(() => undefined);
      } else {
        findings.push({ route, control: next.label, what: `not clickable: ${(e instanceof Error ? e.message : String(e)).split('\n')[0]}` });
        continue;
      }
    }
    // The click's answers and its navigation, if it caused one, are
    // AWAITED — not given a fixed 250 ms to happen in. The chrome leg
    // flaked once under parallel workers (2026-09-18) with
    // `page.evaluate: Execution context was destroyed` in paint():
    // Sign out's logout round-trip outlasted the wait, the URL still
    // read as the route, and paint() ran into the document navigation
    // that followed. So first every request the click issued is
    // answered (bounded), then the settle is the TIMEOUT of a race
    // against a URL change, and a change that wins is followed to a
    // committed, parsed document before anything reads the page.
    const answeredBy = Date.now() + ANSWER_TIMEOUT_MS;
    while (inFlight.size && Date.now() < answeredBy) await page.waitForTimeout(25);
    const navigated = await page
      .waitForURL((u) => u.href !== urlBefore, { waitUntil: 'commit', timeout: SETTLE_MS })
      .then(() => true, () => false);
    if (navigated) await page.waitForLoadState('domcontentloaded', { timeout: 10_000 }).catch(() => undefined);
    if (writes.length) tally.writes += writes.length;
    if (errors.length) findings.push({ route, control: next.label, what: `pageerror: ${errors.join(' | ')}` });

    const now = pathOf(page);
    // A navigation leaves the route or pushes an entry. A control that
    // only REPLACES its own route's query — a filter the page keeps in
    // the URL so a reload and a shared link keep it (/ux/jobs, backlog
    // f8027805) — changed state, not place: there is no entry for back
    // to step through, so reading it as a navigation reported "back
    // landed on /" for every filter. It is judged as a response below.
    // An unreadable history length counts as pushed, the old reading.
    const pushed =
      navigated && (await page.evaluate(() => history.length).catch(() => historyBefore + 1)) > historyBefore;
    if (now !== route || pushed) {
      // Navigated. It must be served, and back must return.
      const landed = `${now}${new URL(page.url()).search}`;
      if (process.env['CRAWL_VERBOSE']) console.log(`[click:${leg}] ${route} ${next.key} -> navigated ${landed}`);
      if (leg === 'main' && !OFF_SPA.some((re) => re.test(now)) && !servedBySpa(landed)) {
        findings.push({ route, control: next.label, what: `navigated to ${landed}, which no route serves` });
      }
      if (leg === 'refused' && writes.length) {
        findings.push({ route, control: next.label, what: `write refused (${writes.join(', ')}) and the page navigated to ${landed} as if it had succeeded` });
      }
      await page.goBack({ waitUntil: 'commit', timeout: 10_000 }).catch(() => undefined);
      await page.waitForTimeout(250);
      if (pathOf(page) !== route) {
        if (leg === 'main') findings.push({ route, control: next.label, what: `navigated to ${landed}; back landed on ${pathOf(page)}` });
        if (!(await open(page, route))) break;
      } else {
        await expect(page.locator('.app-shell')).toBeVisible({ timeout: 10_000 }).catch(() => undefined);
      }
      continue;
    }

    const markersAfter = await page.locator(RESPONSE_MARKERS).count();
    const invalidForm = next.submit
      ? await target.evaluate((el) => !(el.closest('form') as HTMLFormElement | null)?.checkValidity()).catch(() => false)
      : false;
    const after = await paint(page);
    const responded = navigated || nativeDialog !== null || markersAfter > markersBefore || after !== before || invalidForm || requests.length > 0 || next.selected;
    if (process.env['CRAWL_VERBOSE']) console.log(`[click:${leg}] ${route} ${next.key} -> ${responded ? 'responded' : 'silent'} requests=${requests.length} markers ${markersBefore}->${markersAfter}`);
    if (leg === 'main' && !responded) {
      findings.push({ route, control: next.label, what: 'no observable response (no navigation, dialog, alert, request, or change in what is painted)' });
    }
    if (leg === 'refused') {
      if (isDestructive(next.label) && !responded) {
        findings.push({ route, control: next.label, what: 'destructive control: no observable response' });
      }
      if (writes.length) {
        const text = await page.locator('body').innerText().catch(() => '');
        if (!text.includes(REFUSAL) && !/\b403\b/.test(text)) {
          findings.push({ route, control: next.label, what: `write refused (${writes.join(', ')}) and the page shows neither the refusal nor its status` });
        }
      }
    }
  }
  page.off('dialog', onDialog);
  page.off('request', onRequest);
  page.off('requestfinished', onAnswered);
  page.off('requestfailed', onAnswered);
  return findings;
}

/// (d) the empty leg over one route.
async function emptyLeg(page: Page, route: string, errors: string[]): Promise<Finding[]> {
  const findings: Finding[] = [];
  errors.length = 0;
  if (!(await open(page, route))) {
    findings.push({ route, control: 'empty backend', what: 'shell never painted' });
    return findings;
  }
  await page.waitForTimeout(300);
  if (errors.length) findings.push({ route, control: 'empty backend', what: `pageerror: ${errors.join(' | ')}` });
  const markers = await page.locator(FAILURE_MARKER).allInnerTexts();
  if (markers.length) {
    findings.push({
      route,
      control: 'empty backend',
      what: `every read answered 200 and empty, yet ${markers.length} ${FAILURE_MARKER} painted: ${markers.map((m) => JSON.stringify(m.trim().slice(0, 80))).join(', ')}`,
    });
  }
  return findings;
}

// ---------------------------------------------------------------------------
// Known gaps — what the crawl found on main, listed so the crawl is
// green and the gap stays visible. Pinned both ways: a finding here is
// expected, and an entry with no finding is a red that says "delete
// the line" — the mechanism outage-crawl's SILENT list uses.
// ---------------------------------------------------------------------------

type Gap = Readonly<{ route: string; control: string; chrome?: true; why: string }>;

/// Where the chrome is crawled. Any route would do — the chrome is the
/// same everywhere — and this is the one sign-in lands on.
const CHROME_ROUTE = '/ux/me';

/// The route the load-time-throw pin opens, and what it throws. Any
/// crawled route would do — the pin is about the crawl, not the page —
/// and this one carries few controls, so the pin is short.
const LOAD_THROW_ROUTE = '/ux/manual';
const LOAD_THROW_TEXT = 'crawl pin: thrown while the route loaded';

const KNOWN_GAPS: ReadonlyArray<Gap> = [
];

const isKnown = (f: Finding): boolean => KNOWN_GAPS.some((g) => g.route === f.route && g.control === f.control);

// ---------------------------------------------------------------------------
// The crawl, sharded.
// ---------------------------------------------------------------------------

const SHARDS = 4;

test.describe('the interaction crawl — every rendered link lands, every control answers, back returns', () => {
  test.describe.configure({ mode: 'parallel' });

  for (let shard = 0; shard < SHARDS; shard++) {
    test(`shard ${shard + 1}/${SHARDS}: links, clicks, back, empty backend, refused writes`, async ({ page }) => {
      test.setTimeout(600_000);
      const only = process.env['CRAWL_ROUTES']?.split(',');
      const routes = ROUTES.filter((r, i) => i % SHARDS === shard && (!only || only.includes(r)));

      const mode: Mode = { refuse: false, empty: false };
      await installLegs(page, mode);
      const errors: string[] = [];
      page.on('pageerror', (e) => errors.push(e.message));
      const tally: Tally = { routes: 0, links: 0, clicks: 0, refusedClicks: 0, writes: 0 };
      const found: Finding[] = [];
      const t0 = Date.now();

      for (const route of routes) {
        const t = Date.now();
        mode.refuse = false;
        mode.empty = false;
        found.push(...(await clickLeg(page, route, 'main', 'page', tally, errors)));
        mode.refuse = true;
        found.push(...(await clickLeg(page, route, 'refused', 'page', tally, errors)));
        mode.refuse = false;
        mode.empty = true;
        found.push(...(await emptyLeg(page, route, errors)));
        mode.empty = false;
        console.log(`[crawl ${shard + 1}] ${route} ${Date.now() - t}ms`);
      }
      console.log(`[crawl ${shard + 1}] ${routes.length} routes in ${Date.now() - t0}ms`, tally);

      const unexpected = found.filter((f) => !isKnown(f));
      const closed = KNOWN_GAPS.filter((g) => !g.chrome && routes.includes(g.route) && !found.some((f) => f.route === g.route && f.control === g.control));

      expect(
        unexpected.map((f) => `[${f.route}] ${f.control}: ${f.what}`),
        `Gaps the crawl found (route, control, what happened). Fix the page, or list ` +
          `the gap in KNOWN_GAPS with its text so it stays visible while it waits.`,
      ).toEqual([]);
      expect(
        closed.map((g) => `[${g.route}] ${g.control}`),
        `These KNOWN_GAPS entries produced no finding — the gap has closed. Delete ` +
          `their lines, so the list cannot go on excusing pages that no longer need it.`,
      ).toEqual([]);
    });
  }

  test('the chrome: every control in the perspective bar, sidebar and feedback trigger answers', async ({ page }) => {
    test.setTimeout(300_000);
    const mode: Mode = { refuse: false, empty: false };
    await installLegs(page, mode);
    const errors: string[] = [];
    page.on('pageerror', (e) => errors.push(e.message));
    const tally: Tally = { routes: 0, links: 0, clicks: 0, refusedClicks: 0, writes: 0 };
    const found = await clickLeg(page, CHROME_ROUTE, 'main', 'chrome', tally, errors);
    mode.refuse = true;
    found.push(...(await clickLeg(page, CHROME_ROUTE, 'refused', 'chrome', tally, errors)));
    console.log(`[crawl chrome]`, tally);
    // Scope 'chrome' is everything on the page, so a page gap listed
    // for CHROME_ROUTE is expected here too.
    const unexpected = found.filter((f) => !isKnown(f));
    const closed = KNOWN_GAPS.filter((g) => g.route === CHROME_ROUTE && !found.some((f) => f.control === g.control));
    expect(unexpected.map((f) => `[${f.route}] ${f.control}: ${f.what}`)).toEqual([]);
    expect(closed.map((g) => `[${g.route}] ${g.control}`), 'these KNOWN_GAPS entries produced no finding — delete their lines').toEqual([]);
  });

  test('a route that throws while it loads is a finding of its own, before any control is clicked', async ({ page }) => {
    // The /it/registry/dispatcher crash the first nightly playground
    // crawl found (2026-09-18, backlog ee86a789) threw while the page
    // LOADED, and this crawl could not see it even with the reproducing
    // fixture installed: it read pageerrors only after a click, and
    // cleared them before each one. The reproducing shape is fixed on
    // main, so this pin manufactures the class instead of the instance:
    // the route's open throws, and the crawl must name it as the
    // route's own finding — `page (load)` — not silence, and not a
    // control's.
    test.setTimeout(120_000);
    const mode: Mode = { refuse: false, empty: false };
    await installLegs(page, mode);
    await page.addInitScript(([route, text]) => {
      if (location.pathname === route) {
        setTimeout(() => {
          throw new Error(text);
        }, 0);
      }
    }, [LOAD_THROW_ROUTE, LOAD_THROW_TEXT] as const);
    const errors: string[] = [];
    page.on('pageerror', (e) => errors.push(e.message));
    const tally: Tally = { routes: 0, links: 0, clicks: 0, refusedClicks: 0, writes: 0 };
    const found = await clickLeg(page, LOAD_THROW_ROUTE, 'main', 'page', tally, errors);
    expect(found.map((f) => `[${f.route}] ${f.control}: ${f.what}`)).toContain(
      `[${LOAD_THROW_ROUTE}] page (load): pageerror: ${LOAD_THROW_TEXT}`,
    );
  });

  test('every crawled route is one the router serves', () => {
    // A roster row nothing serves crawls the landing page under a false
    // name and reports success for a page that does not exist: that
    // was '/ux/refurb', for months (see LANDING_FALLBACK in _routes.ts).
    const ghosts = ROUTES.filter((r) => r !== LANDING_FALLBACK && !servedBySpa(r));
    expect(ghosts, 'these ROUTES rows parse to the router catch-all — there is no such page').toEqual([]);
    expect(servedBySpa(LANDING_FALLBACK), 'LANDING_FALLBACK must stay a path the router does not serve').toBe(false);
  });

  test('no KNOWN_GAPS entry names a route that is not crawled', () => {
    const crawled = new Set(ROUTES);
    const ghosts = KNOWN_GAPS.filter((g) => !crawled.has(g.route)).map((g) => `[${g.route}] ${g.control}`);
    expect(ghosts, 'a gap on a route no crawl visits reads as "known" while covering nothing — drop it, or fix the path').toEqual([]);
  });

  // The DEFERRED roster is one definition in _routes.ts, read by this
  // spec and by route-smoke, and route-smoke's drift tests pin it to
  // the catalog — so a route deferred for one crawl and not the other
  // cannot be written, and needs no test to catch it.
});
