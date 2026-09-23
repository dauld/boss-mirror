// /ux/calendar/me — "My schedule", every control and render state
// pinned as the page behaves TODAY (page audit 0e4fef17, step `test`).
//
// Before this spec the route was reached by the two crawls only:
// route-smoke renders it under an all-modules manifest and asserts no
// crash, and outage-crawl lists it as SILENT because the mocked session
// has no employee, so its one read never fires (gap 6, d4b08eca). No
// spec clicked a week button, rendered a reservation, failed the read,
// or rendered the module-disabled notice the live instance actually
// shows — its manifest has `modules.calendar = false` (gap 7, 8ea82533).
//
// Two renders, because the live instance and the page disagree:
//   State A — the calendar module off (the live instance, 2026-09-23):
//             ModuleDisabled, one button.
//   State B — the module on: MyCalendarPage, three buttons, one read,
//             no writes, no links.
//
// Lines that pin a FILED gap's current behaviour name the gap. They are
// meant to be edited by the car that fixes it, so the fix shows up here
// as a changed expectation instead of a silently-passing one.
//
// Time is fixed (a Wednesday, UTC) so the week the page computes is a
// known Mon–Sun and the read's `start`/`end` can be asserted exactly.
// The mocked /api/jobs/live answers `sim_clock: {}`, so appNow() is the
// wall clock this spec fixes.

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { mountPage, settledReads } from './_helpers';
import { installSmokeMocks, installTenantManifest, MODULES_LIVE } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const PATH = '/ux/calendar/me';
const EMP_ID = 'emp-001'; // installSmokeMocks' roster persona

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const RESERVATIONS = /\/api\/calendar\/reservations\?/;

/// 2026-09-23 is a Wednesday; its week is Mon 21 – Sun 27 September.
const NOW = new Date('2026-09-23T15:00:00Z');
const WEEK = { start: '2026-09-21T00:00:00.000Z', end: '2026-09-28T00:00:00.000Z' };
const PREV = { start: '2026-09-14T00:00:00.000Z', end: '2026-09-21T00:00:00.000Z' };
const NEXT = { start: '2026-09-28T00:00:00.000Z', end: '2026-10-05T00:00:00.000Z' };
const DAY_HEADERS = [
  'Mon, Sep 21', 'Tue, Sep 22', 'Wed, Sep 23', 'Thu, Sep 24',
  'Fri, Sep 25', 'Sat, Sep 26', 'Sun, Sep 27',
];

test.use({ timezoneId: 'UTC', locale: 'en-US' });

const reservation = (
  id: string, start: string, end: string, reason_kind: string, reason_ref_id: string,
  notes: string | null = null,
) => ({
  id, subject: { subject_kind: 'employee', id: EMP_ID },
  window: { start, end }, reason_kind, reason_ref_id, strength: 'hard', notes,
  created_by: 'test', created_at: '2026-09-01T00:00:00Z', cancelled_at: null,
});

/// The shell, the calendar module on (installSmokeMocks' MODULES_ON), and
/// a session that resolves to the roster employee — the one shape under
/// which the page's read fires at all.
async function installEmployeeSession(page: Page): Promise<void> {
  await page.clock.setFixedTime(NOW);
  await installSmokeMocks(page);
  await page.route(/\/api\/session$/, (r) => json(r, { employee_id: EMP_ID, username: 'ceo' }));
}

/// The shell's own non-GET: App.svelte records every route open
/// (shell/surface-opens.ts, 628f182b). It is the chrome's write, not
/// this page's, so the page's write count excludes it by path.
const SHELL_WRITES: ReadonlySet<string> = new Set(['/api/surface-opens']);

/// Record every reservations read and every non-GET the page sends.
function watch(page: Page): { reads: URL[]; writes: Request[] } {
  const seen = { reads: [] as URL[], writes: [] as Request[] };
  page.on('request', (req) => {
    const url = new URL(req.url());
    if (!url.pathname.startsWith('/api/')) return;
    if (req.method() !== 'GET') {
      if (!SHELL_WRITES.has(url.pathname)) seen.writes.push(req);
      return;
    }
    if (url.pathname === '/api/calendar/reservations') seen.reads.push(url);
  });
  return seen;
}

function readWindow(url: URL | undefined): Record<string, string | null> {
  return {
    resource_kind: url?.searchParams.get('resource_kind') ?? null,
    resource_id: url?.searchParams.get('resource_id') ?? null,
    start: url?.searchParams.get('start') ?? null,
    end: url?.searchParams.get('end') ?? null,
  };
}

const body = (page: Page) => page.locator('.catalog');
const button = (page: Page, name: string) => page.getByRole('button', { name, exact: true });

test.describe('/ux/calendar/me — State A, the calendar module off (the live instance)', () => {
  for (const [name, modules] of [
    ['a manifest listing no modules', MODULES_LIVE],
    ['a manifest with calendar = false', { calendar: false }],
  ] as const) {
    test(`${name} renders ModuleDisabled, and its one button goes home and back`, async ({ page }) => {
      const seen = watch(page);
      await installEmployeeSession(page);
      await installTenantManifest(page, modules);
      await mountPage(page, PATH);

      const notice = page.locator('.module-disabled');
      await expect(notice.locator('h1')).toHaveText('Not enabled for this tenant');
      // Gap 1 / 8(b) (eff0c5e5, 7c196817): the route is gated by the
      // Release calendar row, so the notice names it — not the "My
      // schedule" row the reader clicked.
      await expect(notice.locator('strong')).toHaveText(ROUTE_CATALOG.calendar.label);
      await expect(notice.locator('strong')).toHaveText('Release calendar');
      expect(ROUTE_CATALOG.schedule.path).toBe(PATH);
      await expect(notice).toContainText(
        "The Release calendar module is turned off in this tenant's tenant.toml. The page exists in the platform — the active tenant just doesn't surface it.",
      );
      await expect(notice).toContainText(
        'To enable: set calendar = true in examples/<tenant>/seeds/tenant.toml under [modules], redeploy, and the page comes back.',
      );
      // One control, no links; the page behind the gate never mounts.
      await expect(notice.getByRole('button')).toHaveCount(1);
      await expect(notice.locator('a')).toHaveCount(0);
      await expect(page.locator('.week-controls')).toHaveCount(0);

      await notice.getByRole('button', { name: 'Back to home' }).click();
      await expect.poll(() => new URL(page.url()).pathname).toBe('/');
      await expect(page.locator('.module-disabled')).toHaveCount(0);

      await page.goBack();
      await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
      await expect(page.locator('.module-disabled h1')).toHaveText('Not enabled for this tenant');

      expect(seen.reads, 'the gated page never reads reservations').toHaveLength(0);
      expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
    });
  }
});

test.describe('/ux/calendar/me — State B, the module on: identity', () => {
  test('no session: "Sign in to see your week.", the three buttons disabled, no read', async ({ page }) => {
    const seen = watch(page);
    await page.clock.setFixedTime(NOW);
    await installSmokeMocks(page); // /api/session answers {} — unauthenticated
    await mountPage(page, PATH, { titleMatch: /^My Week$/ });

    await expect(page.locator('.exec-eyebrow')).toHaveText('Calendar');
    await expect(body(page)).toContainText('Reservations from the global calendar primitive');
    await expect(body(page).locator('p.empty')).toHaveText('Sign in to see your week.');
    for (const name of ['← Prev week', 'This week', 'Next week →']) {
      await expect(button(page, name)).toBeDisabled();
    }
    await expect(page.locator('.week-grid')).toHaveCount(0);
    await page.waitForTimeout(500);
    expect(seen.reads).toHaveLength(0);
  });

  // Gap 4 (cfe3f465): while the session is still loading, the page tells
  // a signed-in user to sign in.
  test('a session still loading paints "Sign in to see your week." until it resolves', async ({ page }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => { release = resolve; });
    await installEmployeeSession(page);
    await page.route(/\/api\/session$/, async (r) => {
      await held;
      await json(r, { employee_id: EMP_ID, username: 'ceo' });
    });
    await page.route(RESERVATIONS, (r) => json(r, []));
    await page.goto(PATH);

    await expect(body(page).locator('p.empty')).toHaveText('Sign in to see your week.');
    await expect(page.locator('h1').first()).toHaveText('My Week');
    release();
    await expect(page.locator('h1').first()).toHaveText(`${EMP_ID} — week of Mon, Sep 21`);
    await expect(page.getByText('Sign in to see your week.')).toHaveCount(0);
  });

  // Gap 4 (cfe3f465), second half: a guest is `ready` with an id that is
  // its username, so the read fires for an id nothing reserves against
  // and the week paints empty with nothing saying why.
  test('a guest session reads reservations for the guest username and paints an empty week', async ({ page }) => {
    const seen = watch(page);
    await installEmployeeSession(page);
    await page.route(/\/api\/session$/, (r) => json(r, { username: 'guest', role: 'audit-readonly' }));
    await page.route(RESERVATIONS, (r) => json(r, []));
    await mountPage(page, PATH, { titleMatch: /week of Mon, Sep 21/ });

    await expect(page.locator('.week-col-empty')).toHaveCount(7);
    expect(await settledReads(page, () => seen.reads.length, 1)).toBe(1);
    // guestEmployee(username) takes the username as its id.
    expect(readWindow(seen.reads[0])).toEqual({ resource_kind: 'employee', resource_id: 'guest', ...WEEK });
    await expect(page.locator('h1').first()).toHaveText('guest — week of Mon, Sep 21');
  });
});

test.describe('/ux/calendar/me — State B, the module on: the read and the week', () => {
  test('mount reads this Mon–Sun once, for the session employee, and an empty answer paints seven dashes', async ({ page }) => {
    const seen = watch(page);
    await installEmployeeSession(page);
    await page.route(RESERVATIONS, (r) => json(r, []));
    await mountPage(page, PATH, { titleMatch: new RegExp(`^${EMP_ID} — week of Mon, Sep 21$`) });

    expect(await settledReads(page, () => seen.reads.length, 1)).toBe(1);
    expect(readWindow(seen.reads[0])).toEqual({ resource_kind: 'employee', resource_id: EMP_ID, ...WEEK });

    await expect(page.locator('.week-col-header')).toHaveText(DAY_HEADERS);
    await expect(page.locator('.week-col-empty')).toHaveText(Array(7).fill('—'));
    await expect(page.locator('.week-cell')).toHaveCount(0);
    // The page has no links anywhere in its body.
    await expect(body(page).locator('a')).toHaveCount(0);
    expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
  });

  test('Prev week, Next week and This week each re-read their own week and retitle the page', async ({ page }) => {
    const seen = watch(page);
    await installEmployeeSession(page);
    await page.route(RESERVATIONS, (r) => json(r, []));
    await mountPage(page, PATH, { titleMatch: /week of Mon, Sep 21$/ });
    await expect.poll(() => seen.reads.length).toBe(1);

    const title = page.locator('h1').first();
    await button(page, '← Prev week').click();
    await expect(title).toHaveText(`${EMP_ID} — week of Mon, Sep 14`);
    await expect.poll(() => seen.reads.length).toBe(2);
    expect(readWindow(seen.reads[1])).toEqual({ resource_kind: 'employee', resource_id: EMP_ID, ...PREV });
    await expect(page.locator('.week-col-header').first()).toHaveText('Mon, Sep 14');

    await button(page, 'This week').click();
    await expect(title).toHaveText(`${EMP_ID} — week of Mon, Sep 21`);
    await expect.poll(() => seen.reads.length).toBe(3);
    expect(readWindow(seen.reads[2])).toEqual({ resource_kind: 'employee', resource_id: EMP_ID, ...WEEK });

    await button(page, 'Next week →').click();
    await expect(title).toHaveText(`${EMP_ID} — week of Mon, Sep 28`);
    await expect.poll(() => seen.reads.length).toBe(4);
    expect(readWindow(seen.reads[3])).toEqual({ resource_kind: 'employee', resource_id: EMP_ID, ...NEXT });
    await expect(page.locator('.week-col-header').last()).toHaveText('Sun, Oct 4');

    // Buttons move the week in page state only — the URL does not change,
    // so there is no history entry to go back through.
    expect(new URL(page.url()).pathname).toBe(PATH);
    expect(new URL(page.url()).search).toBe('');
    expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
  });

  test('a pending read paints "Loading reservations…"', async ({ page }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => { release = resolve; });
    await installEmployeeSession(page);
    await page.route(RESERVATIONS, async (r) => {
      await held;
      await json(r, []);
    });
    await mountPage(page, PATH, { titleMatch: /week of Mon, Sep 21$/ });

    await expect(body(page).locator('p.empty')).toHaveText('Loading reservations…');
    await expect(page.locator('.week-grid')).toHaveCount(0);
    release();
    await expect(page.locator('.week-col-empty')).toHaveCount(7);
  });

  test('reservations land on their days in start order, with the reason label, the raw ref and the notes', async ({ page }) => {
    await installEmployeeSession(page);
    await page.route(RESERVATIONS, (r) =>
      json(r, [
        // Deliberately out of order: Wednesday's later block first.
        reservation('r-meet', '2026-09-23T14:00:00Z', '2026-09-23T15:00:00Z', 'meeting', 'mtg-7', 'Brew plan'),
        reservation('r-step', '2026-09-23T09:00:00Z', '2026-09-23T11:30:00Z', 'job-step', '3f0c1a2b-step-uuid'),
        reservation('r-pm', '2026-09-21T08:00:00Z', '2026-09-21T09:00:00Z', 'preventive-maintenance-visit', 'pm-1'),
        reservation('r-train', '2026-09-22T10:00:00Z', '2026-09-22T12:00:00Z', 'training', 'trn-1'),
        // Spans Thursday into Friday, so it shows on both.
        reservation('r-pto', '2026-09-24T12:00:00Z', '2026-09-25T12:00:00Z', 'pto', 'pto-1'),
        reservation('r-travel', '2026-09-26T07:00:00Z', '2026-09-26T19:00:00Z', 'travel', 'trip-1'),
        // A kind outside the page's closed switch renders as itself.
        reservation('r-custom', '2026-09-27T10:00:00Z', '2026-09-27T11:00:00Z', 'site-visit', 'sv-1'),
      ]),
    );
    await mountPage(page, PATH, { titleMatch: /week of Mon, Sep 21$/ });

    const col = (i: number) => page.locator('.week-col').nth(i);
    await expect(page.locator('.week-col')).toHaveCount(7);

    await expect(col(0).locator('.week-cell-time')).toHaveText(['8:00 AM–9:00 AM']);
    await expect(col(0).locator('[class*="chip-reason-"]')).toHaveText(['preventive maintenance visit']);
    await expect(col(1).locator('[class*="chip-reason-"]')).toHaveText(['Training']);

    // Wednesday: sorted by start, not by arrival.
    await expect(col(2).locator('.week-cell-time')).toHaveText(['9:00 AM–11:30 AM', '2:00 PM–3:00 PM']);
    await expect(col(2).locator('[class*="chip-reason-"]')).toHaveText(['Job step', 'Meeting']);
    // Gap 5 (b6c4d9a1): a job-step's ref is a bare step id, as text, not a link.
    await expect(col(2).locator('.week-cell-ref')).toHaveText(['3f0c1a2b-step-uuid', 'mtg-7']);
    await expect(col(2).locator('.week-cell-notes')).toHaveText(['Brew plan']);

    await expect(col(3).locator('[class*="chip-reason-"]')).toHaveText(['PTO']);
    await expect(col(4).locator('[class*="chip-reason-"]')).toHaveText(['PTO']);
    await expect(col(4).locator('.week-cell-time')).toHaveText(['12:00 PM–12:00 PM']);
    await expect(col(5).locator('[class*="chip-reason-"]')).toHaveText(['Travel']);
    await expect(col(6).locator('[class*="chip-reason-"]')).toHaveText(['site-visit']);

    await expect(page.locator('.week-col-empty')).toHaveCount(0);
    await expect(page.locator('.week-cell')).toHaveCount(8);
    // Gap 5 (b6c4d9a1): no cell can be followed anywhere.
    await expect(body(page).locator('a')).toHaveCount(0);
  });
});

test.describe('/ux/calendar/me — State B: a failed read is said, never drawn as an empty week', () => {
  // Gap 8(a) (7c196817): the 502/503 words name `calendar_api_url`,
  // which is not the knob behind the gateway's calendar route.
  for (const status of [503, 502]) {
    test(`HTTP ${status} says the calendar service is unavailable`, async ({ page }) => {
      await installEmployeeSession(page);
      await page.route(RESERVATIONS, (r) => json(r, { error: 'down' }, status));
      await mountPage(page, PATH, { titleMatch: /week of Mon, Sep 21$/ });

      await expect(body(page).locator('p.empty')).toHaveText(
        "Couldn't load calendar: calendar service unavailable — wire calendar_api_url",
      );
      await expect(page.locator('.week-grid')).toHaveCount(0);
      await expect(page.locator('.week-col-empty')).toHaveCount(0);
      // The words are honest, but the line is painted with class `empty`
      // — the class of "Sign in…" and "Loading…" — not the shared
      // FAILURE_MARKER the outage crawl reads (_routes.ts). Pinned as it
      // is, so the car that adopts the marker changes this line.
      await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
      // The week buttons stay live, so the reader can retry another week.
      await expect(button(page, 'Next week →')).toBeEnabled();
    });
  }

  test('any other status names its code', async ({ page }) => {
    await installEmployeeSession(page);
    await page.route(RESERVATIONS, (r) => json(r, { error: 'boom' }, 500));
    await mountPage(page, PATH, { titleMatch: /week of Mon, Sep 21$/ });

    await expect(body(page).locator('p.empty')).toHaveText("Couldn't load calendar: calendar HTTP 500");
    await expect(page.locator('.week-grid')).toHaveCount(0);
  });

  test('a network failure shows the browser\'s own message', async ({ page }) => {
    await installEmployeeSession(page);
    await page.route(RESERVATIONS, (r) => r.abort('failed'));
    await mountPage(page, PATH, { titleMatch: /week of Mon, Sep 21$/ });

    await expect(body(page).locator('p.empty')).toHaveText("Couldn't load calendar: Failed to fetch");
    await expect(page.locator('.week-grid')).toHaveCount(0);
  });

  test('a failed week then a good week: the failure line clears and the grid returns', async ({ page }) => {
    await installEmployeeSession(page);
    await page.route(RESERVATIONS, (r) => {
      const start = new URL(r.request().url()).searchParams.get('start');
      return start === WEEK.start ? json(r, { error: 'down' }, 503) : json(r, []);
    });
    await mountPage(page, PATH, { titleMatch: /week of Mon, Sep 21$/ });
    await expect(body(page).locator('p.empty')).toContainText("Couldn't load calendar:");

    await button(page, 'Next week →').click();
    await expect(page.locator('.week-col-empty')).toHaveCount(7);
    await expect(page.getByText("Couldn't load calendar:")).toHaveCount(0);
  });
});
