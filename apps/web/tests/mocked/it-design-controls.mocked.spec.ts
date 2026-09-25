// /it/design — every control the page renders, pinned (page audit
// 79db48ae, step `test`; backlog 6bef2baa asked for exactly this spec).
//
// design-in-working-out.mocked.spec.ts pins what the IN, WORKING and OUT
// thirds SAY (backlog 08372fdb). This spec pins what nothing did: every
// link and where it lands, the Review button's destination on each of
// its three paths, the way back, each read's empty, failed and malformed
// legs, and the words the page shows.
//
// The inventory, measured on origin/main 80e14260 on 2026-09-25. The
// measure step read 25f5e8b8 on 2026-09-23. Since then the queue has
// gained an Answers column (the envelope carries steps: design-review
// v2) and a `decided` panel with its own read, and the failure line
// wears the shared marker:
//   links     4 tabs    — Reviews / Experiments / Feedback / Backlog
//                         (ItTabs group `design`, each a catalogued route)
//             1 per row of the decided panel, WORKING and OUT
//                         (→ /jobs/{id}, the job detail route)
//   buttons   1 per queue row — Review (→ the review step's full-page
//                         surface, back labelled "Design Review"; the
//                         job page when the step cannot be resolved)
//             1 per failure line — Retry (→ that region's read again,
//                         and only that one; 3bbb194a)
//   forms     0, inputs 0
//   reads     3         — GET /api/stations/design-review/queue (mount,
//                         and again on its failure line's Retry)
//                         GET /api/stations/design-decided/queue (mount,
//                         whatever the first read answers — 3bbb194a —
//                         and again on its failure line's Retry)
//                         GET /api/jobs/{id} (on Review, only when the
//                         envelope did not carry the row's review step)
//   writes    0         — decisions are written on the step surface the
//                         Review button opens, not here. There is no
//                         refusal of this page's own to pin.
//
// THIS STEP PINS WHAT THE PAGE DOES TODAY; it does not fix the gaps.
// Where today's behaviour IS a gap, the test says so in its name
// ("CURRENT, gap N (item)") and asserts the current paint, so the car
// that fixes the gap has to flip that assertion. The gaps measured on
// 2026-09-23, and where each stands on 80e14260:
//   1  08372fdb  FIXED   — a saved review reads "saved · n of m answered"
//   2  8c0e11d8  open    — "Nothing is waiting on a decision" while
//                          kinds declaring this page their surface wait
//                          (FOUNDER DECISION, at draft-design)
//   3  84d97547  open    — the Status column prints the constant `open`
//   4  b56aa2c7  open    — the lede claims priority order, and priority
//                          is not shown
//   5  08372fdb  FIXED   — WORKING: decided designs at fold
//   6  08372fdb  FIXED   — OUT: settled in the station's 7-day window
//   7  c3e4edcc  FIXED   — the failure line wears `.load-failed` (the
//                          outage crawl no longer lists the route SILENT)
//   8  c11e9d3c  FIXED server-side — a failed steps read is a 500 now,
//                          not a shorter queue. Invisible to a mocked page.
//   9  6bef2baa  this file
// Found while pinning:
//   U1  67825067  FIXED — a malformed 200 from either station read is
//       that read's failure line, not the honest empty state (the item
//       filed for the /it/operate/marshalling twin took both pages)
//   U2  3bbb194a  FIXED — a failed review-queue read hid the decided
//       panel: its independent read was never made. Each region now
//       loads and fails on its own.
//   U4  3bbb194a  FIXED — neither failure line offered a Retry (the
//       Retry half of measure's gap 7; c3e4edcc closed on the marker
//       alone). Each line now carries one, and it re-runs only its
//       own region's read.
// and not owned by any open item (UNFILED):
//   U3  the eyebrow names "System Model", on an IT page, in both lens
//       rows and in the fallback; the fallback subtitle "Open questions
//       and ADRs" describes the corpus deleted on 2026-09-10, and it is
//       what a failed read shows (the /it/kb twin is 839a7f0f)

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { FAILURE_MARKER } from './_routes';
import { installSmokeMocks } from './_smokeMocks';
import { parseRoute } from '../../src/router';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { FALLBACK_HEADER } from '../../src/it/design/designLens';

const PATH = '/it/design';
const TITLE = { titleMatch: /Design review/ };

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const QUEUE_READ = /\/api\/stations\/design-review\/queue$/;
const DECIDED_READ = /\/api\/stations\/design-decided\/queue$/;
const jobRead = (id: string): RegExp => new RegExp(`/api/jobs/${id}$`);
const esc = (s: string): string => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

// ---------------------------------------------------------------------
// Fixtures. They mirror the live envelopes of 2026-09-25 07:33Z, cut
// down. Dates are relative to now because the Opened and Decided
// columns are, so the envelopes are FUNCTIONS the handlers call: the
// clock is read when a read is answered, not when the file loads
// (scripts/a-mocked-fixture-reads-the-clock-when-it-answers.test.ts).
// ---------------------------------------------------------------------

const DAY = 86_400_000;
/// n whole days ago, plus an hour so the floor never lands on a boundary.
const ago = (n: number): string => new Date(Date.now() - n * DAY - 3_600_000).toISOString();

const Q = (n: number) =>
  Array.from({ length: n }, (_, i) => ({ anchor: `Q${i + 1}`, title: `q${i + 1}`, proposal: 'p' }));

const packet = (id: string, title: string, over: Record<string, unknown> = {}) => ({
  id, kind: 'design-doc', title, status: 'open', priority: 'standard',
  opened_on: ago(1), closed_on: null, tags: [], metadata: {}, simulated: false, ...over,
});

const review = (id: string, status: string, questions: unknown[], resolutions: unknown[], completed_on: string | null = null) =>
  ({ id, kind: 'review-design', spec_slug: 'review', status, completed_on, metadata: { questions, resolutions } });

const fold = (id: string, status: string, metadata: Record<string, unknown> = {}) =>
  ({ id, kind: 'task', spec_slug: 'fold', status, metadata });

/// The live lens, verbatim (infra/platform/stations/design-review.toml v2).
const LENS = {
  eyebrow: 'System Model · Design review',
  title: 'Design review',
  subtitle: 'Design docs waiting on a decision — the packet is the doc',
  panels: ['queue', 'decided'],
  with_steps: true,
};

/// Three rows in the STATION's order: priority, then age. The high one
/// is first although it is not the oldest, and the oldest standard one
/// comes before the newest. Neither sort by date gives this order, so
/// if the page re-sorted, the order test would see it.
const HIGH = 'rev-high';
const BARE = 'rev-bare';
const FRESH = 'rev-fresh';
const queue = () => ({
  station: 'design-review', kind: 'batch', discipline: ['priority', 'age'],
  wip_limit: null, over_limit: false, terminal_window_days: null,
  lens: LENS,
  total: 3,
  data: [
    packet(HIGH, 'Admit every packet through one edge', { priority: 'high', opened_on: ago(2) }),
    packet(BARE, 'A design whose steps the envelope did not carry', { opened_on: ago(9) }),
    packet(FRESH, 'An untouched design', { opened_on: ago(1) }),
  ],
  // BARE has no entry: a packet caught before its steps materialised,
  // or a registry still at v1. Its Review has to resolve the step itself.
  steps: {
    [HIGH]: [review('rs-high', 'ready', Q(3), [{ anchor: 'Q1', decision: 'yes' }, { anchor: 'Q2', decision: 'no' }])],
    [FRESH]: [review('rs-fresh', 'ready', Q(2), [])],
  },
});

const emptyQueue = () => ({ ...queue(), total: 0, data: [], steps: {} });

const W_READY = 'w-ready';
const W_ACTIVE = 'w-active';
const O_DONE = 'o-done';
const decided = () => ({
  station: 'design-decided', kind: 'batch', discipline: ['recency'],
  wip_limit: null, over_limit: false, terminal_window_days: 7,
  lens: { eyebrow: 'System Model · Design review', title: 'Decided designs', panels: [], with_steps: true },
  total: 3,
  data: [
    packet(W_ACTIVE, 'A design being folded'),
    packet(W_READY, 'A design waiting for its fold'),
    packet(O_DONE, 'A settled design', { status: 'closed', closed_on: ago(2), metadata: { outcome: 'published' } }),
  ],
  steps: {
    [W_ACTIVE]: [review('r1', 'completed', Q(1), [{ anchor: 'Q1', decision: 'ok' }], ago(1)), fold('f1', 'active')],
    [W_READY]: [review('r2', 'completed', Q(1), [{ anchor: 'Q1', decision: 'ok' }], ago(3)), fold('f2', 'ready')],
    [O_DONE]: [review('r3', 'completed', Q(1), [{ anchor: 'Q1', decision: 'ok' }], ago(4)),
      fold('f3', 'completed', { folded_into: 'docs/architecture-decisions.md §Design docs' })],
  },
});

const emptyDecided = () => ({ ...decided(), total: 0, data: [], steps: {} });

/// A whole step and a whole job, the shape `GET /api/jobs/{id}`
/// serves. The Review button reads only `steps[].id` and `kind`, but
/// the surface it lands on renders the rest, and a thin fixture throws
/// there.
const fullStep = (jobId: string, id: string, kind: string, slug: string, status: string) => ({
  id, job_id: jobId, kind, spec_slug: slug, title: slug, assignee_id: 'emp-david', status,
  sort_order: 1, blocked_by: [], sign_offs_required: [], sign_offs: [], completed_on: null,
  notes: null, fields: [], metadata: { procedure: 'Answer each question.', questions: Q(1), resolutions: [] },
});
const fullJob = (id: string, steps: readonly unknown[]) => ({
  id, kind: 'design-doc', title: 'A design whose steps the envelope did not carry', status: 'open',
  priority: 'standard', opened_on: ago(9), closed_on: null, owner_id: 'emp-david',
  subject: { subject_kind: 'custom', id: 'boss-platform' }, tags: [], metadata: {}, simulated: false, steps,
});

type Counts = { queue: number; decided: number; jobs: Record<string, number> };

/// The page's world, well-formed. Either station read can be overridden;
/// `jobs` answers the Review button's fallback read per packet. The
/// counts record how many times the page asked.
async function install(
  page: Page,
  over: Partial<{
    queue: (r: Route) => Promise<void>;
    decided: (r: Route) => Promise<void>;
    jobs: Record<string, (r: Route) => Promise<void>>;
  }> = {},
): Promise<Counts> {
  await installSmokeMocks(page);
  const counts: Counts = { queue: 0, decided: 0, jobs: {} };
  await page.route(QUEUE_READ, (r) => {
    counts.queue += 1;
    return over.queue ? over.queue(r) : json(r, queue());
  });
  await page.route(DECIDED_READ, (r) => {
    counts.decided += 1;
    return over.decided ? over.decided(r) : json(r, decided());
  });
  for (const [id, answer] of Object.entries(over.jobs ?? {})) {
    await page.route(jobRead(id), (r) => {
      counts.jobs[id] = (counts.jobs[id] ?? 0) + 1;
      return answer(r);
    });
  }
  return counts;
}

const header = (page: Page) => page.locator('header.exec-header');
const queueTable = (page: Page) => page.locator('table.design-table');
const queueRow = (page: Page, title: string) => queueTable(page).locator('tbody tr').filter({ hasText: title });
const decidedTables = (page: Page) => page.locator('table.decided-table');
const failures = (page: Page) => page.locator(FAILURE_MARKER);
const sectionTitles = (page: Page) => page.locator('section.tab-section > h3');
/// Each failure line's Retry, by the read it names (3bbb194a). Two can
/// show at once, so each carries its region in its accessible name.
const queueRetry = (page: Page) => page.getByRole('button', { name: 'Retry the review queue' });
const decidedRetry = (page: Page) => page.getByRole('button', { name: 'Retry the decided designs' });

/// The empty state's paragraph, verbatim. Whitespace is normalised by
/// toHaveText; the three <code> words are part of the text.
const EMPTY_QUEUE_TEXT =
  'Nothing is waiting on a decision. A design doc reaches this queue as a design-doc packet carrying its own prose and questions — boss design files one. Settled material folds into docs/architecture-decisions.md, the one current-truth record.';

const REVIEW_HREF = (id: string, stepId: string): RegExp =>
  new RegExp(`/jobs/${id}/steps/${stepId}${esc('?from=%2Fit%2Fdesign&from_label=Design%20Review')}$`);

// ---------------------------------------------------------------------
// The route, the chrome and the words
// ---------------------------------------------------------------------

test.describe('/it/design — the route and the words it says', () => {
  test('the path is the catalogued Design route, headed by what the station row declares', async ({ page }) => {
    expect(ROUTE_CATALOG['system-design'].path).toBe(PATH);
    expect(parseRoute(PATH)).toEqual({ kind: 'systemDesign' });
    await install(page);
    await mountPage(page, PATH, TITLE);

    await expect(header(page).locator('.exec-eyebrow')).toHaveText('System Model · Design review');
    await expect(header(page).locator('h1')).toHaveText('Design review');
    await expect(header(page).locator('p')).toHaveText('Design docs waiting on a decision — the packet is the doc');
    // Three sections, in the lens's panel order: IN, then WORKING and OUT.
    await expect(sectionTitles(page)).toHaveText([
      'Waiting on a decision (3)',
      'Decided, being folded (2)',
      'Settled in the last 7 days (1)',
    ]);
    await expect(failures(page)).toHaveCount(0);
  });

  test('the queue reads its lede, columns and rows verbatim, in the station\'s order', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    await expect(page.locator('.design-lede')).toHaveText(
      'In the station\'s order: priority, then age. The first row is the one it would hand out next.',
    );
    await expect(queueTable(page).locator('thead th')).toHaveText(['Packet', 'Status', 'Answers', 'Opened', 'Review']);
    const rows = queueTable(page).locator('tbody tr');
    await expect(rows).toHaveCount(3);
    await expect(rows.nth(0).locator('td')).toHaveText([
      'Admit every packet through one edge', 'open', 'saved · 2 of 3 answered', '2d ago', 'Review',
    ]);
    // No steps on the wire for this packet: its progress is unread, and
    // "—" says so. It never reads as "not started".
    await expect(rows.nth(1).locator('td')).toHaveText([
      'A design whose steps the envelope did not carry', 'open', '—', '9d ago', 'Review',
    ]);
    await expect(rows.nth(2).locator('td')).toHaveText([
      'An untouched design', 'open', 'not started · 2 questions', '1d ago', 'Review',
    ]);
  });

  test('gap 1 (08372fdb) is FIXED — a saved review is marked as work in hand, an untouched one is not', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    await expect(queueRow(page, 'Admit every packet').locator('td.design-progress')).toHaveClass(/design-saved/);
    await expect(queueRow(page, 'An untouched design').locator('td.design-progress')).not.toHaveClass(/design-saved/);
  });

  test('CURRENT, gap 3 (84d97547): the Status column prints the constant "open" on every row', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    // The station admits only open packets, so no row can print anything else.
    await expect(queueTable(page).locator('td.design-status')).toHaveText(['open', 'open', 'open']);
  });

  test('CURRENT, gap 4 (b56aa2c7): the lede claims priority order, and no row shows a priority', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    await expect(page.locator('.design-lede')).toContainText('priority, then age');
    await expect(queueTable(page).locator('thead th')).not.toContainText(['Priority']);
    // The first row is first BECAUSE it is high priority, and the page
    // never says so.
    await expect(queueTable(page)).not.toContainText(/\bhigh\b/i);
  });

  test('WORKING and OUT (gaps 5 and 6, 08372fdb) are FIXED — their columns and every fold label, verbatim', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    const [working, settled] = [decidedTables(page).nth(0), decidedTables(page).nth(1)];

    await expect(working.locator('thead th')).toHaveText(['Packet', 'Decided', 'Fold']);
    await expect(working.locator('tbody tr').nth(0).locator('td')).toHaveText(['A design being folded', '1d ago', 'being folded']);
    await expect(working.locator('tbody tr').nth(1).locator('td')).toHaveText(['A design waiting for its fold', '3d ago', 'waiting for a builder']);

    await expect(settled.locator('thead th')).toHaveText(['Packet', 'Settled', 'Outcome', 'Folded into']);
    await expect(settled.locator('tbody tr').nth(0).locator('td')).toHaveText([
      'A settled design', '2d ago', 'published', 'docs/architecture-decisions.md §Design docs',
    ]);
  });

  test('an empty decided station paints both empty lines, and a station with no window says only "Settled"', async ({ page }) => {
    await install(page, { decided: (r) => json(r, { ...emptyDecided(), terminal_window_days: null }) });
    await mountPage(page, PATH, TITLE);
    await expect(page.getByText('Nothing decided is waiting to be folded.')).toBeVisible();
    await expect(page.getByText('Nothing settled in this window.')).toBeVisible();
    await expect(sectionTitles(page)).toHaveText(['Waiting on a decision (3)', 'Decided, being folded (0)', 'Settled (0)']);
    await expect(failures(page)).toHaveCount(0);
  });

  test('both panels say they are loading until their read answers', async ({ page }) => {
    let releaseQueue!: () => void;
    let releaseDecided!: () => void;
    const queueHeld = new Promise<void>((ok) => (releaseQueue = ok));
    const decidedHeld = new Promise<void>((ok) => (releaseDecided = ok));
    await install(page, {
      queue: async (r) => { await queueHeld; return json(r, queue()); },
      decided: async (r) => { await decidedHeld; return json(r, decided()); },
    });
    await page.goto(PATH);

    await expect(page.getByText('Loading the review queue…')).toBeVisible();
    // The header does not wait: it is the fallback until the row answers.
    await expect(header(page).locator('h1')).toHaveText(FALLBACK_HEADER.title);
    releaseQueue();
    await expect(queueTable(page)).toBeVisible();
    await expect(page.getByText('Loading the decided designs…')).toBeVisible();
    releaseDecided();
    await expect(decidedTables(page)).toHaveCount(2);
    await expect(page.getByText(/Loading the/)).toHaveCount(0);
  });
});

// ---------------------------------------------------------------------
// The controls — each does what its label says, and back returns
// ---------------------------------------------------------------------

test.describe('/it/design — every control', () => {
  test('the four tabs are catalogued routes, Reviews is this page, and each lands where it says and comes back', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    const tabs = page.locator('nav.it-tabs a');
    await expect(tabs).toHaveText(['Reviews', 'Experiments', 'Feedback', 'Backlog']);
    await expect(tabs.nth(0)).toHaveAttribute('aria-current', 'page');
    await expect(page.locator('nav.it-tabs a[aria-current]')).toHaveCount(1);

    // Each tab's target is read from the one catalogue, never a second list.
    const catalogued = new Set(Object.values(ROUTE_CATALOG).map((n) => n.path));
    const hrefs = await tabs.evaluateAll((as) => as.map((a) => a.getAttribute('href') ?? ''));
    expect(hrefs).toEqual([
      ROUTE_CATALOG['system-design'].path,
      ROUTE_CATALOG['system-experiments'].path,
      ROUTE_CATALOG['system-feedback'].path,
      ROUTE_CATALOG['system-backlog'].path,
    ]);
    for (const h of hrefs) {
      expect(catalogued.has(h)).toBe(true);
      expect(parseRoute(h).kind).not.toBe('notFound');
    }

    for (const [i, h] of hrefs.entries()) {
      if (h === PATH) continue;
      await tabs.nth(i).click();
      await expect(page).toHaveURL(new RegExp(`${esc(h)}$`));
      await expect(page.locator('nav.it-tabs a[aria-current="page"]')).toHaveAttribute('href', h);
      await page.goBack();
      await expect(page).toHaveURL(new RegExp(`${esc(PATH)}$`));
      await expect(header(page).locator('h1')).toHaveText('Design review');
      await expect(queueTable(page).locator('tbody tr')).toHaveCount(3);
    }
  });

  test('Review opens the review step the envelope carried, reads nothing to find it, and its Back returns here', async ({ page }) => {
    // The fallback read is REFUSED for this packet. Had the page made
    // it, the button would have landed on /service/…, so landing on the
    // step surface proves the envelope's step was used. (The step
    // surface reads the job itself after the navigation, so a count of
    // that read could not tell the two apart.)
    await install(page, { jobs: { [HIGH]: (r) => json(r, 'down', 503) } });
    await mountPage(page, PATH, TITLE);

    await queueRow(page, 'Admit every packet').getByRole('button', { name: 'Review' }).click();
    await expect(page).toHaveURL(REVIEW_HREF(HIGH, 'rs-high'));
    expect(parseRoute(`/jobs/${HIGH}/steps/rs-high`, '?from=%2Fit%2Fdesign&from_label=Design%20Review')).toMatchObject({
      kind: 'stepFocus', jobId: HIGH, stepId: 'rs-high', from: PATH, fromLabel: 'Design Review',
    });

    const back = page.locator('button.step-focus-back');
    await expect(back).toHaveText('← Design Review');
    await back.click();
    await expect(page).toHaveURL(new RegExp(`${esc(PATH)}$`));
    await expect(queueTable(page).locator('tbody tr')).toHaveCount(3);
  });

  test('the browser\'s back from the review surface returns here as well', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    await queueRow(page, 'An untouched design').getByRole('button', { name: 'Review' }).click();
    await expect(page).toHaveURL(REVIEW_HREF(FRESH, 'rs-fresh'));
    await page.goBack();
    await expect(page).toHaveURL(new RegExp(`${esc(PATH)}$`));
    await expect(header(page).locator('h1')).toHaveText('Design review');
  });

  test('Review on a row without its step resolves the step with one job read, and lands on the same surface', async ({ page }) => {
    const counts = await install(page, {
      jobs: {
        [BARE]: (r) => json(r, fullJob(BARE, [
          fullStep(BARE, 'rs-bare', 'review-design', 'review', 'ready'),
          fullStep(BARE, 'fold-bare', 'task', 'fold', 'pending'),
        ])),
      },
    });
    await mountPage(page, PATH, TITLE);
    expect(counts.jobs[BARE] ?? 0).toBe(0);

    await queueRow(page, 'A design whose steps').getByRole('button', { name: 'Review' }).click();
    await expect(page).toHaveURL(REVIEW_HREF(BARE, 'rs-bare'));
    expect(counts.jobs[BARE]).toBeGreaterThanOrEqual(1);
  });

  test('CURRENT, 6bef2baa: Review whose job read is refused lands on the job page, and nothing says why', async ({ page }) => {
    // designLens.ts calls this "a worse door but a real one". The spec
    // pins it rather than judging it; see the sign-off brief.
    const alerts: string[] = [];
    await install(page, { jobs: { [BARE]: (r) => json(r, 'down', 503) } });
    await mountPage(page, PATH, TITLE);
    page.on('dialog', (d) => { alerts.push(d.message()); void d.dismiss(); });

    await queueRow(page, 'A design whose steps').getByRole('button', { name: 'Review' }).click();
    await expect(page).toHaveURL(new RegExp(`/service/${BARE}$`));
    expect(parseRoute(`/service/${BARE}`)).toEqual({ kind: 'jobDetail', jobId: BARE });
    expect(alerts).toEqual([]);
  });

  test('Review whose job carries no review-design step lands on the job page too', async ({ page }) => {
    await install(page, {
      jobs: { [BARE]: (r) => json(r, fullJob(BARE, [fullStep(BARE, 'fold-bare', 'task', 'fold', 'pending')])) },
    });
    await mountPage(page, PATH, TITLE);
    await queueRow(page, 'A design whose steps').getByRole('button', { name: 'Review' }).click();
    await expect(page).toHaveURL(new RegExp(`/service/${BARE}$`));
  });

  test('each decided row is a link to its packet\'s job detail, and back returns here', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    const links = decidedTables(page).locator('a');
    await expect(links).toHaveText(['A design being folded', 'A design waiting for its fold', 'A settled design']);
    const hrefs = await links.evaluateAll((as) => as.map((a) => a.getAttribute('href')));
    expect(hrefs).toEqual([`/jobs/${W_ACTIVE}`, `/jobs/${W_READY}`, `/jobs/${O_DONE}`]);
    for (const h of hrefs) expect(parseRoute(h ?? '').kind).toBe('jobDetail');

    await links.nth(2).click();
    await expect(page).toHaveURL(new RegExp(`/jobs/${O_DONE}$`));
    await page.goBack();
    await expect(page).toHaveURL(new RegExp(`${esc(PATH)}$`));
    await expect(sectionTitles(page)).toHaveText([
      'Waiting on a decision (3)',
      'Decided, being folded (2)',
      'Settled in the last 7 days (1)',
    ]);
  });

  test('the queue table has no link and no control but its Review buttons', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    await expect(queueTable(page).locator('a')).toHaveCount(0);
    await expect(queueTable(page).locator('button')).toHaveText(['Review', 'Review', 'Review']);
    // The page's own body: its header and its three sections (the
    // shell's chrome carries a search of its own, which is not this page's).
    await expect(page.locator('header.exec-header, section.tab-section').locator('form, input, select, textarea')).toHaveCount(0);
  });
});

// ---------------------------------------------------------------------
// The reads — empty is "nothing here", failed is `.load-failed`, and
// the two are never the same paint
// ---------------------------------------------------------------------

test.describe('/it/design — empty, failed and malformed reads', () => {
  test('an empty review queue paints the empty state and no failure', async ({ page }) => {
    await install(page, { queue: (r) => json(r, emptyQueue()) });
    await mountPage(page, PATH, TITLE);
    await expect(sectionTitles(page).first()).toHaveText('Waiting on a decision (0)');
    await expect(page.locator('section.tab-section').first().locator('p.empty')).toHaveText(EMPTY_QUEUE_TEXT);
    await expect(queueTable(page)).toHaveCount(0);
    await expect(page.locator('.design-lede')).toHaveCount(0);
    await expect(failures(page)).toHaveCount(0);
  });

  test('CURRENT, gap 2 (8c0e11d8): the empty claim is unconditional — the page reads only the design-doc stations', async ({ page }) => {
    const asked: string[] = [];
    page.on('request', (req: Request) => {
      const u = new URL(req.url());
      if (u.pathname.startsWith('/api/stations/') || u.pathname.startsWith('/api/jobs')) asked.push(u.pathname + u.search);
    });
    await install(page, { queue: (r) => json(r, emptyQueue()) });
    await mountPage(page, PATH, TITLE);
    await expect(page.getByText('Nothing is waiting on a decision.', { exact: false })).toBeVisible();
    await expect(decidedTables(page)).toHaveCount(2);
    // No read of the page-audit, retro or incident sign-offs whose kinds
    // name this page in metadata.surfaces, so "nothing is waiting" is
    // said without asking them.
    const stations = [...new Set(asked.filter((p) => p.startsWith('/api/stations/')))].sort();
    expect(stations).toEqual(['/api/stations/design-decided/queue', '/api/stations/design-review/queue']);
    expect(asked.filter((p) => /assignments|kind=page-audit|surfaces/.test(p))).toEqual([]);
  });

  test('a refused review-queue read is a failure line in the page\'s words, never the empty state', async ({ page }) => {
    await install(page, { queue: (r) => json(r, 'down', 503) });
    await mountPage(page, PATH, TITLE);
    const line = page.locator('p.design-error');
    await expect(line).toHaveText('The review queue could not be read: queue: HTTP 503. This is not an empty queue.');
    await expect(line).toHaveClass(/load-failed/);
    await expect(line).toHaveAttribute('role', 'alert');
    await expect(page.getByText('Nothing is waiting on a decision.', { exact: false })).toHaveCount(0);
    await expect(queueTable(page)).toHaveCount(0);
  });

  test('a review-queue read that never answers (network down) is a failure line too', async ({ page }) => {
    await install(page, { queue: (r) => r.abort('connectionrefused') });
    await mountPage(page, PATH, TITLE);
    const line = page.locator('p.design-error.load-failed');
    await expect(line).toContainText('The review queue could not be read: ');
    await expect(line).toContainText('This is not an empty queue.');
    await expect(page.getByText('Nothing is waiting on a decision.', { exact: false })).toHaveCount(0);
  });

  test('CURRENT, U3 (UNFILED): a failed read shows the fallback header, whose eyebrow says System Model and whose subtitle names the deleted corpus', async ({ page }) => {
    await install(page, { queue: (r) => json(r, 'down', 503) });
    await mountPage(page, PATH, TITLE);
    await expect(header(page).locator('.exec-eyebrow')).toHaveText('System Model · Design review');
    await expect(header(page).locator('h1')).toHaveText('Design review');
    await expect(header(page).locator('p')).toHaveText('Open questions and ADRs');
    expect(FALLBACK_HEADER).toEqual({
      eyebrow: 'System Model · Design review',
      title: 'Design review',
      subtitle: 'Open questions and ADRs',
    });
  });

  test('U2 (3bbb194a) is FIXED — a failed review-queue read leaves WORKING and OUT standing: the decided read is made', async ({ page }) => {
    const counts = await install(page, { queue: (r) => json(r, 'down', 503) });
    await mountPage(page, PATH, TITLE);
    await expect(page.locator('p.design-error.load-failed')).toBeVisible();
    await expect(sectionTitles(page)).toHaveText(['Decided, being folded (2)', 'Settled in the last 7 days (1)']);
    await expect(decidedTables(page).locator('a')).toHaveText([
      'A design being folded', 'A design waiting for its fold', 'A settled design',
    ]);
    // The one failure on the page is the queue's: the decided panel's
    // read answered, and it painted what it read.
    await expect(failures(page)).toHaveCount(1);
    expect(counts.decided).toBe(1);
  });

  test('both reads refused: two failure lines, each in its own words and with its own Retry', async ({ page }) => {
    await install(page, { queue: (r) => json(r, 'down', 503), decided: (r) => json(r, 'down', 503) });
    await mountPage(page, PATH, TITLE);
    await expect(failures(page)).toHaveText([
      'The review queue could not be read: queue: HTTP 503. This is not an empty queue.',
      'Could not read the decided designs: HTTP 503',
    ]);
    await expect(queueRetry(page)).toBeVisible();
    await expect(decidedRetry(page)).toBeVisible();
    await expect(sectionTitles(page)).toHaveCount(0);
  });

  test('U4 (3bbb194a) is FIXED — the review queue\'s Retry re-runs its read, and only its read', async ({ page }) => {
    let answered = 0;
    const counts = await install(page, {
      // Refused once, then served: what a Retry after a blip meets.
      queue: (r) => (answered++ === 0 ? json(r, 'down', 503) : json(r, queue())),
    });
    await mountPage(page, PATH, TITLE);
    await expect(page.locator('p.design-error.load-failed')).toBeVisible();
    await expect(decidedTables(page)).toHaveCount(2);
    expect(counts).toMatchObject({ queue: 1, decided: 1 });

    await queueRetry(page).click();
    await expect(queueTable(page).locator('tbody tr')).toHaveCount(3);
    await expect(failures(page)).toHaveCount(0);
    await expect(queueRetry(page)).toHaveCount(0);
    await expect(sectionTitles(page)).toHaveText([
      'Waiting on a decision (3)',
      'Decided, being folded (2)',
      'Settled in the last 7 days (1)',
    ]);
    // The decided panel was not asked again: a Retry is its region's.
    expect(counts).toMatchObject({ queue: 2, decided: 1 });
  });

  test('a review-queue Retry that fails again stays the failure line, with its Retry', async ({ page }) => {
    const counts = await install(page, { queue: (r) => json(r, 'down', 503) });
    await mountPage(page, PATH, TITLE);
    await queueRetry(page).click();
    await expect.poll(() => counts.queue).toBe(2);
    await expect(page.locator('p.design-error.load-failed')).toHaveText(
      'The review queue could not be read: queue: HTTP 503. This is not an empty queue.',
    );
    await expect(queueRetry(page)).toBeVisible();
    await expect(page.getByText('Nothing is waiting on a decision.', { exact: false })).toHaveCount(0);
  });

  test('a refused decided read is its own failure line, and the review queue still renders', async ({ page }) => {
    await install(page, { decided: (r) => json(r, 'down', 503) });
    await mountPage(page, PATH, TITLE);
    const line = page.locator(`${FAILURE_MARKER}[role="alert"]`);
    await expect(line).toHaveText('Could not read the decided designs: HTTP 503');
    await expect(page.getByText('Nothing decided is waiting to be folded.')).toHaveCount(0);
    await expect(page.getByText('Nothing settled in this window.')).toHaveCount(0);
    await expect(queueTable(page).locator('tbody tr')).toHaveCount(3);
    await expect(decidedRetry(page)).toBeVisible();
    await expect(queueRetry(page)).toHaveCount(0);
  });

  test('U4 (3bbb194a) is FIXED — the decided panel\'s Retry re-runs its read, and only its read', async ({ page }) => {
    let answered = 0;
    const counts = await install(page, {
      decided: (r) => (answered++ === 0 ? json(r, 'down', 503) : json(r, decided())),
    });
    await mountPage(page, PATH, TITLE);
    await expect(page.getByText('Could not read the decided designs: HTTP 503')).toBeVisible();
    expect(counts).toMatchObject({ queue: 1, decided: 1 });

    await decidedRetry(page).click();
    await expect(sectionTitles(page)).toHaveText([
      'Waiting on a decision (3)',
      'Decided, being folded (2)',
      'Settled in the last 7 days (1)',
    ]);
    await expect(failures(page)).toHaveCount(0);
    await expect(decidedRetry(page)).toHaveCount(0);
    expect(counts).toMatchObject({ queue: 1, decided: 2 });
  });

  // U1, FIXED by 67825067: both station reads parse through the shared
  // envelope reader (src/data/shape.ts), so a 200 that is not the
  // envelope is the read's failure line, naming the read and what came
  // back. Each line carries its region's Retry (U4, 3bbb194a).
  test('U1 (67825067): a malformed 200 from the review queue is the failure line, never "Nothing is waiting"', async ({ page }) => {
    // A list where the envelope is due — what an api floor's catch-all
    // answers, and what the page used to take as an envelope with no data.
    await install(page, { queue: (r) => json(r, []) });
    await mountPage(page, PATH, TITLE);
    const line = page.locator('p.design-error.load-failed');
    await expect(line).toHaveText(
      'The review queue could not be read: /api/stations/design-review/queue: HTTP 200, but the body is a list, not a {data: [...]} envelope. This is not an empty queue.',
    );
    await expect(line).toHaveAttribute('role', 'alert');
    await expect(page.getByText('Nothing is waiting on a decision.', { exact: false })).toHaveCount(0);
    // Only the queue's section is missing: WORKING and OUT read on their
    // own (U2, 3bbb194a).
    await expect(sectionTitles(page)).toHaveText(['Decided, being folded (2)', 'Settled in the last 7 days (1)']);
    await expect(queueRetry(page)).toBeVisible();
  });

  test('U1 (67825067): a malformed 200 from the decided station is its own failure line, and the review queue still renders', async ({ page }) => {
    await install(page, { decided: (r) => json(r, { unexpected: true }) });
    await mountPage(page, PATH, TITLE);
    await expect(page.locator(`${FAILURE_MARKER}[role="alert"]`)).toHaveText(
      'Could not read the decided designs: /api/stations/design-decided/queue: HTTP 200, but the body is an object with no data list, not a {data: [...]} envelope',
    );
    await expect(page.getByText('Nothing decided is waiting to be folded.')).toHaveCount(0);
    await expect(page.getByText('Nothing settled in this window.')).toHaveCount(0);
    await expect(sectionTitles(page)).toHaveText(['Waiting on a decision (3)']);
    await expect(queueTable(page).locator('tbody tr')).toHaveCount(3);
  });
});
