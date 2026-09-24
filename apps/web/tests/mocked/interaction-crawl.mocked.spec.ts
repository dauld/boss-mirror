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
//       change in what is painted (THE JUDGING RULE below);
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
// THE JUDGING RULE — backlog 840c5a76. A click is judged ONCE, at a
// moment its own events fix:
//
//   1. just before the click, what is painted is snapshotted and the
//      page's own record of the API requests it opens is marked
//      (recordPageRequests in _helpers.ts: every fetch, XHR, EventSource);
//   2. after it, the crawl waits, under the suite's stated expect
//      budget, for the requests past that mark that are the CLICK'S —
//      each to arrive as a request event, then each to be answered —
//      re-reading the record as it waits, so a request an answer leads
//      to joins the wait when the page opens it;
//   3. a 250 ms race for a navigation (SETTLE_MS), the record read once
//      more for a request opened during it, and the page is read.
//
// It RESPONDED if it navigated, raised a native dialog or a new
// alert/status/dialog, left its form invalid, is the selected control
// (an idempotent click), ISSUED a request, or changed what is painted
// between the snapshot in 1 and the read in 3. A request is the click's
// unless the page's own clock opened it: a timer callback the click did
// not schedule (a mount-time poll, a ticking refresh) is what a no-op
// click would set off too, so it answers for nothing. A timer the click
// DID schedule — a debounce, a deferred reload — is the click's.
// Nothing extends the wait but requests the click owns, so a silent
// control, which owns none, is judged after exactly the 250 ms it
// always was, and its verdict is read once, never again in the hope
// that something changes.
//
// Two ways to get this wrong, each measured. The judge used to wait on
// "every request in flight", heard only through Playwright's request
// event; a starved host delivers that event late, the set was empty when
// the click returned, and "[/it/operate/perf] RESET — no observable
// response" was read with its POST already on the wire (once in ~26
// shard-1 runs at 5x parallel load). And the naive repair — keep
// re-reading until anything responds — lets a page that polls answer for
// a control that does nothing; the old judge already counted any request
// heard in its window, a poll's included. Both are pinned below by
// forcing them: the race (the POST's answer held back and its event
// handed on late) must not read silent, and an inert control on a page
// polling every 40 ms must.
//
// What the rule still cannot tell apart: a page that REPAINTS on its
// own clock inside the 250 ms race credits that paint change to
// whatever was clicked. That window is the one the judge always had —
// the rule adds no time to it — and a paint change has no author the
// way a request has, so it is named here rather than guessed at.
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
import { pageRequests, readsSettled, recordPageRequests } from './_helpers';

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
/// issued has been answered — that wait is bounded by the suite's stated
/// expect budget, generous because a mock answers in-process and only a
/// starved host is slow.
const SETTLE_MS = 250;

/// How long a dialog gets to leave after Escape before the crawl calls
/// it one Escape does not close and reopens the route. A wait for the
/// dialog to be GONE, so a quick close costs its outro and no more; the
/// bound is only reached by a dialog that stays.
const ESCAPE_CLOSES_MS = 1_000;

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
    await page.waitForLoadState('domcontentloaded').catch(() => undefined);
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
      await page.goto(route, { waitUntil: 'commit' });
      await expect(page.locator('.app-shell')).toBeVisible({ timeout: 20_000 });
      // The route's own reads answered and painted — not 400 ms, which
      // was a bet that they would be (backlog 840c5a76). A route whose
      // reads outlast the budget is crawled as it stands, as before.
      await readsSettled(page).catch(() => undefined);
      return true;
    } catch {
      // Retried once, as route-smoke does; a real failure misses both.
    }
  }
  return false;
}

const pathOf = (page: Page): string => new URL(page.url()).pathname;

/// A request as the page's record spells it: method and path.
const lineOf = (r: Request): string => `${r.method()} ${new URL(r.url()).pathname}`;

/// The heard request events that account for every line the page says
/// the click issued — one event per line — or null while any line is
/// still unheard.
function claim(issued: ReadonlyArray<string>, heard: ReadonlyArray<Request>): Request[] | null {
  const pool = [...heard];
  const own: Request[] = [];
  for (const line of issued) {
    const at = pool.findIndex((r) => lineOf(r) === line);
    if (at < 0) return null;
    own.push(...pool.splice(at, 1));
  }
  return own;
}

type Finding = Readonly<{ route: string; control: string; what: string }>;
type Tally = { routes: number; links: number; clicks: number; refusedClicks: number; writes: number };

/// The legs' mock switches, flipped between legs on ONE page so the
/// smoke fixtures are installed once and layered, never copied.
type Mode = { refuse: boolean; empty: boolean };

/// What the judging-rule pins force, and nothing else sets: a request
/// event handed to the crawl this many ms after the browser issued it.
type Force = Readonly<{ requestEventLateMs?: number }>;

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
  // The page's own record of the requests it opens, and which of them
  // its own clock opened (_helpers.ts) — THE JUDGING RULE reads it.
  await recordPageRequests(page);
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
  force: Force = {},
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
  // Every API request event the crawl has heard since the click, and
  // every request answered. The click's OWN requests are the page's
  // record (recordPageRequests); these events are only how the crawl
  // learns each of them has been answered — what the page does with an
  // answer (paint a refusal, navigate after a confirmed sign-out) cannot
  // be read before it arrives.
  let heard: Request[] = [];
  const answered = new WeakSet<Request>();
  const onRequest = (r: Request): void => {
    if (r.url().includes('/api/')) heard.push(r);
  };
  const onAnswered = (r: Request): void => {
    answered.add(r);
  };
  // The pins below hand a request event to the crawl late, the way a
  // starved host's protocol does; every real leg hands it on at once.
  const onRequestEvent = (r: Request): void => {
    if (force.requestEventLateMs) setTimeout(() => onRequest(r), force.requestEventLateMs);
    else onRequest(r);
  };
  page.on('request', onRequestEvent);
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
      // Gone, outro and all — not 150 ms, which a starved host's outro
      // outlasts (backlog 840c5a76). A dialog still up when this gives up
      // is one Escape does not close, and the route is reopened.
      // short on purpose: timing out IS the answer — Escape did not close this dialog
      await expect(page.locator(DIALOGS)).toHaveCount(0, { timeout: ESCAPE_CLOSES_MS }).catch(() => undefined);
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
    nativeDialog = null;
    // Where the page's record stands before the click: everything after
    // this mark, on this document, the click may own.
    const mark = await pageRequests(page);
    heard = [];
    const urlBefore = page.url();
    const historyBefore = await page.evaluate(() => history.length).catch(() => 0);
    const target = page.locator('button, [role="button"]').nth(next.index);
    try {
      // A non-<button> control falls through to dispatchEvent below when
      // the real click misses, so it keeps the short wait: fifteen
      // seconds per SVG track before a click that lands anyway is the
      // crawl's own time, not a finding.
      // short on purpose: a missed non-button click is dispatched below, so its timing out is not a finding
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
    //
    // WHICH requests are the click's is the page's own record, and the
    // wait is for exactly those: each to reach the crawl as a request
    // event, then each to be answered — THE JUDGING RULE in the header.
    // The record is re-read on every poll, so a request the click's
    // answers lead to (the seasonal-release retire POST is issued only
    // once its GET /api/jobs has answered) joins the wait when the page
    // opens it; it is read once more after the navigation race for one
    // opened during it. The wait ran on "every request in flight" until
    // backlog 840c5a76, heard through Playwright's request event, and
    // that set was EMPTY when a starved host had not yet delivered the
    // Reset POST's event: the wait ended at once and the click was judged
    // silent with its write already on the wire.
    //
    // The wait is under the suite's stated expect budget (a poll every
    // 25 ms), and so are the best-effort waits below: each carried
    // 10 000 ms of its own until backlog de205627, a tighter cap than the
    // suite states, and one that gives up quietly — a click still waiting
    // on a starved host was then read as "no observable response" or
    // "back landed on", a finding about the host rather than the page.
    // A document the click replaced (Sign out's /login) is not the one
    // marked, so its record is not this click's (null); then what the
    // crawl heard is the only record there is, as it was before.
    const settleOwn = async (): Promise<string[] | null> => {
      let own: string[] | null = [];
      await expect
        .poll(async () => {
          const now = await pageRequests(page);
          own = now && mark && now.doc === mark.doc
            ? now.opened.slice(mark.opened.length).filter((e) => !e.timer).map((e) => `${e.method} ${e.path}`)
            : null;
          if (own === null) return true;
          const matched = claim(own, heard);
          return matched !== null && matched.every((r) => answered.has(r));
        }, { intervals: [25] })
        .toBe(true)
        .catch(() => undefined);
      return own;
    };
    let own = await settleOwn();
    const navigated = await page
      // short on purpose: SETTLE_MS is a race the click may lose — timing out means it navigated nowhere
      .waitForURL((u) => u.href !== urlBefore, { waitUntil: 'commit', timeout: SETTLE_MS })
      .then(() => true, () => false);
    if (navigated) await page.waitForLoadState('domcontentloaded').catch(() => undefined);
    else own = await settleOwn();
    const issued = (own ?? heard.map(lineOf)).filter((line) => !SILENT_WRITES.some((re) => re.test(line.split(' ')[1] ?? '')));
    const writes = issued.filter((line) => !line.startsWith('GET '));
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
      await page.goBack({ waitUntil: 'commit' }).catch(() => undefined);
      // Back on the route — the event (c) is about — under the suite's
      // navigation budget; it waited a flat 250 ms until backlog 840c5a76.
      await page.waitForURL((u) => u.pathname === route, { waitUntil: 'commit' }).catch(() => undefined);
      if (pathOf(page) !== route) {
        if (leg === 'main') findings.push({ route, control: next.label, what: `navigated to ${landed}; back landed on ${pathOf(page)}` });
        if (!(await open(page, route))) break;
      } else {
        await expect(page.locator('.app-shell')).toBeVisible().catch(() => undefined);
      }
      continue;
    }

    const markersAfter = await page.locator(RESPONSE_MARKERS).count();
    const invalidForm = next.submit
      ? await target.evaluate((el) => !(el.closest('form') as HTMLFormElement | null)?.checkValidity()).catch(() => false)
      : false;
    const after = await paint(page);
    const responded = navigated || nativeDialog !== null || markersAfter > markersBefore || after !== before || invalidForm || issued.length > 0 || next.selected;
    if (process.env['CRAWL_VERBOSE']) console.log(`[click:${leg}] ${route} ${next.key} -> ${responded ? 'responded' : 'silent'} issued=${issued.length} [${issued.join(', ')}] heard=${heard.length} [${heard.map(lineOf).join(', ')}] markers ${markersBefore}->${markersAfter}`);
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
  page.off('request', onRequestEvent);
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
  // open() has waited for the route's reads to answer and a frame to
  // paint what they meant — the empty state this leg reads. It slept a
  // further 300 ms here until backlog 840c5a76.
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

/// The judging-rule pins (backlog 840c5a76). The race is the route the
/// flake named, forced: its Reset POST answered RACE_ANSWER_HELD_MS late
/// and its request event handed on RACE_EVENT_LATE_MS late — both far
/// past the 250 ms settle, so the old judge read them as nothing.
const RACE_ROUTE = '/it/operate/perf';
const RACE_WRITE = '**/api/gateway/perf/reset';
const RACE_ANSWER_HELD_MS = 3_000;
const RACE_EVENT_LATE_MS = 1_500;
/// The silent-control pin plants an inert button on a quiet route and a
/// poll fast enough that a request lands in every judging window.
const SILENT_ROUTE = '/ux/manual';
const SILENT_LABEL = 'crawl pin: an inert control';
const SILENT_POLL_MS = 40;

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

  test('a click whose request reaches the crawl late is judged by that request, not called silent', async ({ page }) => {
    // Backlog 840c5a76: "[/it/operate/perf] RESET — no observable
    // response" with requests=0, once in ~26 shard-1 runs under 5x
    // parallel load, where every quiet run read requests=1. The page
    // issues the POST inside the click; the crawl hears of it through
    // Playwright's request EVENT, which a starved host delivers late —
    // and the answer that would repaint the page was later still. So
    // the pin forces both halves: the POST's answer is held back, and
    // its request event reaches the crawl after the click has returned.
    test.setTimeout(120_000);
    const mode: Mode = { refuse: false, empty: false };
    await installLegs(page, mode);
    await page.route(RACE_WRITE, async (r) => {
      await new Promise((settle) => setTimeout(settle, RACE_ANSWER_HELD_MS));
      return r.fallback();
    });
    const errors: string[] = [];
    page.on('pageerror', (e) => errors.push(e.message));
    const tally: Tally = { routes: 0, links: 0, clicks: 0, refusedClicks: 0, writes: 0 };
    const found = await clickLeg(page, RACE_ROUTE, 'main', 'page', tally, errors, { requestEventLateMs: RACE_EVENT_LATE_MS });
    expect(found.map((f) => `[${f.route}] ${f.control}: ${f.what}`)).toEqual([]);
    expect(tally.writes, 'the held POST is still counted as the write it is').toBe(1);
  });

  test('a request the page issues on its own timer does not answer for a silent control', async ({ page }) => {
    // The other half of the rule: the crawl must not buy its patience by
    // crediting whatever happens next. A page that polls fast issues a
    // request inside every judging window, so a control that does
    // nothing would read as "issued a request" if any request counted —
    // and the old judge counted any. Planted: one inert button in the
    // page's content slot, on a page that polls every SILENT_POLL_MS from
    // a timer of its own, scheduled at load and not by any click.
    test.setTimeout(120_000);
    const mode: Mode = { refuse: false, empty: false };
    await installLegs(page, mode);
    await page.addInitScript(([route, label, pollMs]) => {
      if (location.pathname !== route) return;
      window.setInterval(() => void fetch('/api/gateway/perf').catch(() => undefined), pollMs);
      const plant = window.setInterval(() => {
        const slot = document.querySelector('.shell-content');
        if (!slot || document.getElementById('crawl-pin-silent')) return;
        const b = document.createElement('button');
        b.id = 'crawl-pin-silent';
        b.type = 'button';
        b.textContent = label;
        slot.appendChild(b);
        window.clearInterval(plant);
      }, 20);
    }, [SILENT_ROUTE, SILENT_LABEL, SILENT_POLL_MS] as const);
    const errors: string[] = [];
    page.on('pageerror', (e) => errors.push(e.message));
    const tally: Tally = { routes: 0, links: 0, clicks: 0, refusedClicks: 0, writes: 0 };
    const found = await clickLeg(page, SILENT_ROUTE, 'main', 'page', tally, errors);
    expect(found.map((f) => `[${f.route}] ${f.control}: ${f.what}`)).toContain(
      `[${SILENT_ROUTE}] ${SILENT_LABEL}: no observable response (no navigation, dialog, alert, request, or change in what is painted)`,
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
