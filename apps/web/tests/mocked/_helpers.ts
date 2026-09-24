// The mocked suite's shared mount helper.
//
// Lived under tests/smoke until 2026-09-18, beside the live-backend
// suite that directory held — 35 specs no CI job, gate check or chore
// ran since 2026-06-18, deleted with design 0e07ce64 (the nightly
// playground crawl under tests/live is what replaced them). This
// helper was the one file that stayed, because twelve mocked specs
// imported `mountPage` from `../smoke/_helpers`; it moved here with
// them (backlog ac3270c7), and tests/smoke is gone —
// crates/core/boss-testing/tests/the_playground_is_crawled_nightly.rs
// pins that it stays gone.
//
// The four helpers only the deleted specs used (pinPersona,
// clickButton, clickAndExpectNavigation, expectTableRow) went with them.

import type { Locator, Page } from '@playwright/test';
import { expect } from '@playwright/test';

// One second of settling: the unbounded JobsListPage loop managed 554
// reads in it, so a bounded page's count is unambiguous.
const SETTLE_MS = 1_000;

/**
 * Wait until `expected` reads have ARRIVED, then give a loop the
 * settling window to show itself, then take the exact count.
 *
 * A window that starts at mount bounds two things at once: how long a
 * loop gets to climb, and how long the FIRST read may take to reach
 * the route handler. Only the first is a spec's claim. On a loaded gate
 * runner the second lost — Expected 1, Received 0 on a correct page, in
 * mount-refetch-audit (backlog 28a60028, 2026-09-20) and then in
 * jobs-kinds-failed-read, which carried the same fixed-wait shape
 * (backlog 3571be7f). A count BELOW the claim is a late read, not a
 * fixed page, and asserting a bound instead (at most one) would pass a
 * page that stopped reading at all — so the count stays exact and the
 * wait is for the condition, under the suite's stated expect budget
 * (playwright.mocked.config.ts), before the window opens. One
 * definition, so the next spec that counts reads takes this rather
 * than a copy of the old shape.
 */
export async function settledReads(
  page: Page,
  reads: () => number,
  expected: number,
): Promise<number> {
  await expect
    .poll(reads, { message: `waiting for the ${expected} mount-time read(s) to arrive` })
    .toBeGreaterThanOrEqual(expected);
  // short on purpose: a runaway loop has no event to wait on — it is only ever seen by counting over a window
  await page.waitForTimeout(SETTLE_MS);
  return reads();
}

// ---------------------------------------------------------------------------
// THE PAGE'S OWN RECORD OF THE REQUESTS IT OPENS (backlog 840c5a76).
//
// A spec that asserts "nothing was sent" or "the page is done reading"
// used to read Playwright's request event after a fixed sleep — 300 ms,
// 500 ms, 6 000 ms — which bets on two things at once: that the page
// has done whatever it was going to do, and that the event has made it
// across the protocol. Under load the second bet loses on its own: the
// interaction crawl judged a Reset click silent with its POST already on
// the wire, because the request event arrived after the judge. The page
// knows better, and knows it synchronously. This records, in the page,
// every API request it opens — fetch, XHR, EventSource — at the moment
// it is opened, and marks when each is answered and when its body has
// been read; so once the action that would send has returned, the
// record already holds anything it sent, and a wait for an answer is a
// wait for that answer rather than for the clock.
//
// It also records WHO opened each: setTimeout and setInterval are
// wrapped so a callback knows which click, if any, scheduled it (clicks
// are numbered from 1 by a capture-phase listener, and a click's
// dispatch runs to a microtask queued by a bubble-phase listener on
// window, or a timeout of 0 if a handler stops propagation). A timer no
// click scheduled — a mount-time poll, a ticking refresh — is the page's
// own clock, and `timer` says so. Install before the first goto.
// ---------------------------------------------------------------------------

/// One API request the page opened, from its own record.
export type Opened = Readonly<{
  method: string;
  /// Absolute, with its query.
  url: string;
  path: string;
  /// Opened from a timer the click being judged (the latest) did not
  /// schedule: the page's own clock, which a no-op click sets off too.
  timer: boolean;
  /// An EventSource: it is never answered in full, so it never settles.
  stream: boolean;
  /// Its answer (or its failure) has arrived.
  settled: boolean;
  /// Its body has been read by the page (json or text), and a task has
  /// run since — so what the page did with it has been done.
  read: boolean;
}>;

/// What `pageRequests` reads: the document's identity and its record. A
/// different `doc` means the document was replaced (a full navigation)
/// and the record read is the new one's, not a continuation.
export type PageRecord = Readonly<{ doc: string; opened: ReadonlyArray<Opened> }>;

function pageRequestRecorder(): void {
  type Entry = { method: string; url: string; path: string; timer: boolean; stream: boolean; settled: boolean; read: boolean };
  const w = window as unknown as { __pageRequests?: Entry[]; __pageDoc?: string };
  if (w.__pageRequests) return;
  const record: Entry[] = [];
  w.__pageRequests = record;
  w.__pageDoc = `${Date.now()}-${Math.random()}`;
  const later = window.setTimeout.bind(window);
  const every = window.setInterval.bind(window);
  let click = 0;
  let dispatching = false;
  // The click that scheduled the timer callback now running (0: none),
  // or -1 outside any timer callback. Reset in a microtask so the
  // effects a callback's state change flushes are still its own.
  let timerOf = -1;
  const lineage = (fn: unknown): unknown => {
    if (typeof fn !== 'function') return fn;
    const from = dispatching ? click : 0;
    return function (this: unknown, ...a: unknown[]) {
      timerOf = from;
      try {
        return (fn as (...x: unknown[]) => unknown).apply(this, a);
      } finally {
        queueMicrotask(() => {
          timerOf = -1;
        });
      }
    };
  };
  window.setTimeout = ((fn: unknown, ms?: number, ...rest: unknown[]) => later(lineage(fn) as TimerHandler, ms, ...rest)) as typeof setTimeout;
  window.setInterval = ((fn: unknown, ms?: number, ...rest: unknown[]) => every(lineage(fn) as TimerHandler, ms, ...rest)) as typeof setInterval;
  const done = (): void => {
    dispatching = false;
  };
  addEventListener('click', () => {
    click += 1;
    dispatching = true;
    later(done, 0);
  }, true);
  addEventListener('click', () => queueMicrotask(done));
  const open = (method: string | undefined, url: string | URL, stream: boolean): Entry | null => {
    try {
      const u = new URL(String(url), location.href);
      if (u.origin !== location.origin || !u.pathname.startsWith('/api/')) return null;
      const e: Entry = {
        method: (method ?? 'GET').toUpperCase(),
        url: u.href,
        path: u.pathname,
        timer: timerOf === 0 || (timerOf > 0 && timerOf !== click),
        stream,
        settled: false,
        read: false,
      };
      record.push(e);
      return e;
    } catch {
      return null; // Not a URL the browser would open either.
    }
  };
  const answerOf = new WeakMap<Response, Entry>();
  const fetchIt = window.fetch;
  window.fetch = function (input: RequestInfo | URL, init?: RequestInit) {
    const req = input instanceof Request ? input : null;
    const e = open(init?.method ?? req?.method, req ? req.url : (input as string | URL), false);
    const p = fetchIt.call(this, input, init);
    if (e) {
      p.then(
        (r) => {
          e.settled = true;
          answerOf.set(r, e);
        },
        () => {
          e.settled = true;
          e.read = true;
        },
      );
    }
    return p;
  } as typeof fetch;
  for (const m of ['json', 'text'] as const) {
    const body = Response.prototype[m] as (this: Response) => Promise<unknown>;
    (Response.prototype as unknown as Record<string, unknown>)[m] = function (this: Response) {
      const p = body.call(this);
      const e = answerOf.get(this);
      if (e) {
        const mark = (): void => {
          later(() => {
            e.read = true;
          }, 0);
        };
        p.then(mark, mark);
      }
      return p;
    };
  }
  const xhrOpen = XMLHttpRequest.prototype.open as (this: XMLHttpRequest, ...a: unknown[]) => void;
  XMLHttpRequest.prototype.open = function (this: XMLHttpRequest, ...a: unknown[]) {
    const e = open(String(a[0]), a[1] as string | URL, false);
    if (e) {
      this.addEventListener('loadend', () => {
        e.settled = true;
        later(() => {
          e.read = true;
        }, 0);
      });
    }
    return xhrOpen.apply(this, a);
  } as typeof XMLHttpRequest.prototype.open;
  const Stream = window.EventSource;
  if (Stream) {
    window.EventSource = class extends Stream {
      constructor(url: string | URL, init?: EventSourceInit) {
        open('GET', url, true);
        super(url, init);
      }
    };
  }
}

/// Install the record. Before the first goto; installing twice is one.
export async function recordPageRequests(page: Page): Promise<void> {
  await page.addInitScript(pageRequestRecorder);
}

/// The page's record now, or null while no document can be read (one is
/// being replaced).
export async function pageRequests(page: Page): Promise<PageRecord | null> {
  return page
    .evaluate(() => {
      const w = window as unknown as { __pageRequests?: Opened[]; __pageDoc?: string };
      return { doc: w.__pageDoc ?? '', opened: (w.__pageRequests ?? []).map((e) => ({ ...e })) };
    })
    .catch(() => null);
}

/// The page's record, refusing one it cannot read (a document being
/// replaced) or one never installed — either would read as "nothing
/// sent", which is the assertion a spec reads this for.
export async function openedRequests(page: Page): Promise<ReadonlyArray<Opened>> {
  const r = await pageRequests(page);
  if (!r || !r.doc) {
    throw new Error('the page has no request record to read: install recordPageRequests before the goto, and read it on a settled document');
  }
  return r.opened;
}

/// Every request the page opened on its own account — not from its own
/// clock, not a stream — has been answered, and a frame has painted
/// since. The wait for "the page has done its reading" that a mount
/// settle and a crash crawl need; under the suite's expect budget.
export async function readsSettled(page: Page): Promise<void> {
  await expect
    .poll(async () => {
      const r = await pageRequests(page);
      return r !== null && r.opened.every((e) => e.timer || e.stream || e.settled);
    }, { intervals: [25], message: 'waiting for the page to have every read it opened answered' })
    .toBe(true);
  await nextFrame(page);
}

/// The page has read the body of an answer to `path` and run what it
/// does with it — the event a "the stale answer did not repaint" spec
/// is about, rather than a sleep it hopes the answer fits inside.
export async function answerRead(page: Page, path: RegExp): Promise<void> {
  await expect
    .poll(async () => ((await pageRequests(page))?.opened ?? []).some((e) => path.test(e.path) && e.read), {
      intervals: [25],
      message: `waiting for the page to read its answer to ${path}`,
    })
    .toBe(true);
}

/// The page has rendered `ready` into the document — or thrown trying, so
/// a spec's own "it threw" assertion names the throw instead of a missing
/// element one budget later. What a plugin mount or a crash pin waits for;
/// they slept 1 500 or 2 000 ms until backlog 840c5a76, a bet that the
/// bundle had loaded and run by then. Rendered, not visible: the
/// authoring workspace mounts StepDagEditor inside a closed <details>.
export async function paintedOrThrew(ready: Locator, errs: ReadonlyArray<string>): Promise<void> {
  await expect
    .poll(async () => errs.length > 0 || (await ready.count()) > 0, {
      message: `waiting for the page to render ${ready.toString()}, or throw`,
    })
    .toBe(true);
}

/// The next frame the page paints: two animation frames, the first
/// before the paint and the second after it. What a spec that reads the
/// render an answer causes is waiting for.
export async function nextFrame(page: Page): Promise<void> {
  await page
    .evaluate(() => new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r()))))
    .catch(() => undefined);
}

/**
 * Mount a page and wait for the AppShell + the page-level h1 to
 * render. Returns once the SPA's first paint has settled, so
 * subsequent role lookups don't race against hydration.
 *
 * Both waits run under the suite's STATED expect budget
 * (playwright.mocked.config.ts), never a number of their own. They
 * carried 10 000 ms each until backlog e614c5de: a page whose h1 paints
 * only once its first read answers (the workflow authoring workspace)
 * missed that cap under gate load on 2026-09-24 — "h1 … element(s) not
 * found, Timeout: 10000ms" on gate 2ab44d1d, green on a re-gate of the
 * same head — while the budget the suite had declared for exactly that
 * load was 15 000. Every spec mounts through here, so a cap tighter than
 * the budget here is one tighter than the budget everywhere.
 */
export async function mountPage(
  page: Page,
  path: string,
  opts: { titleMatch?: RegExp; root?: string } = {},
): Promise<void> {
  await page.goto(path);
  // AppShell renders for every authed route — except the handful that
  // deliberately render outside it to take the whole viewport (login,
  // the full-page step surface). Those pass their own root; waiting
  // for `.app-shell` there fails on a page that is working correctly.
  await expect(page.locator(opts.root ?? '.app-shell')).toBeVisible();
  if (opts.titleMatch) {
    await expect(page.locator('h1').first()).toContainText(opts.titleMatch);
  }
}
