// THE STATIONS' PANELS — design e765b3fc, car N3 (decided by David
// 2026-09-25, on feedback 84cba7e2).
//
// "I would be open to revisiting some of the detail pages we have for
// stations as part of this too" (added_2026_09_25_david_4): each station
// or region page becomes the below-the-map detail for its selection, is
// merged, or retires — with no alias and no redirect before 1.0.0. And
// the newcomer's half (David, on this car): nothing a reader needs may
// vanish, so each retired page's content was MOVED into the panel of the
// station it describes before its page went. This pins, as the car's
// plan names its test, that each retired path renders not-found — with
// the Department Map as its one door — and, beside that, that the
// content each page held is in its station's panel:
//
//  - the floors (/it/yard/<region>) and the Crew Board (/it/crew) are
//    their stations' panels — the floor deck, the queue boards;
//  - yard status (/it/operate/yard-status) is the track's, the dock's and
//    the garage's, each its own lanes;
//  - the conductor's feed (/it/operate/conductor) is the track's;
//  - the feedback board (/it/design/feedback) is receiving's, and the
//    backlog board (/it/design/backlog) receiving's and marshalling's;
//  - Operate lost its Receiving Yard, Marshalling Yard, Yard status and
//    Conductor tabs, and Design its Feedback and Backlog tabs;
//  - the map is PINNED at the top ("pinned at the top, always visible"),
//    so the panel scrolls beneath it.
//
// The board tests that were pinned on the floor pages (it-region-map,
// retired with them) are carried here, opened as selections. What those
// pages drew that no board draws — the region's own second map, its
// breadcrumb, its "← the world" button — retired with them: the map is
// already on top.

import { expect, test, type Page, type Route } from '@playwright/test';
import { RETIRED_ROUTES } from './_routes';
import { YARD_REGIONS, YARD_STATUS, installSmokeMocks } from './_smokeMocks';

const json = (r: Route, b: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const trend = (metric: string, unit: string) => ({
  metric, unit, current: 1, previous: 1, samples: 3, previous_samples: 3,
});

const REGIONS = {
  window_hours: 24,
  now: '2026-09-19T05:00:00Z',
  regions: [
    { name: 'dock', count: 2, bound: 5, state: 'clear', why: '2 cars parked', trend: trend('dock wait', 'hours') },
    { name: 'gates', count: 0, bound: 3, state: 'clear', why: 'no bay in use', trend: trend('gate duration', 'minutes') },
    { name: 'track', count: 0, bound: 1, state: 'clear', why: 'no train in transit', trend: trend('time at CI', 'minutes') },
    { name: 'shed', count: 0, state: 'clear', why: 'nothing awaits a probe', trend: trend('time to proven', 'hours') },
    { name: 'arrivals', count: 0, state: 'clear', why: 'nothing landed', trend: trend('arrivals', 'per day') },
    { name: 'garage', count: 0, state: 'clear', why: 'nothing gated red', trend: trend('reds', 'per day') },
    { name: 'receiving', count: 0, state: 'clear', why: 'nothing inbound', trend: trend('inbound', 'per day') },
    { name: 'marshalling', count: 0, state: 'clear', why: 'nothing waiting', trend: trend('served', 'per day') },
    { name: 'shop-floor', count: 0, bound: 6, state: 'clear', why: 'no run in flight, no crew on the floor', trend: trend('build duration', 'minutes') },
    { name: 'publish', count: 0, state: 'clear', why: 'no pull request awaiting a merge', trend: trend('publishes', 'per day') },
  ],
};

/** Two cars standing on the loading dock. */
const DOCK_QUEUE = {
  discipline: ['parked_since'],
  data: [
    { id: 'car-1111', kind: 'ship-a-change', title: 'Teach the dock to breathe', status: 'open', metadata: { branch: 'fix/dock-breathes' } },
    { id: 'car-2222', kind: 'ship-a-change', title: 'Name the failing check', status: 'open', metadata: { branch: 'fix/name-the-check' } },
  ],
};

async function mocks(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(YARD_REGIONS, (r) => json(r, REGIONS));
  await page.route(/\/api\/stations\/loading-dock\/queue$/, (r) => json(r, DOCK_QUEUE));
}

const PANEL = 'section[data-map-panel]';
const board = (page: Page, name: string) => page.locator(`${PANEL} [data-station-board="${name}"]`);

// ---------------------------------------------------------------------
// The retired pages: not found, with the Department Map as the door
// ---------------------------------------------------------------------

test.describe('the pages the Department Map replaced', () => {
  for (const path of RETIRED_ROUTES) {
    test(`${path} is not found, and its one door is the Department Map`, async ({ page }) => {
      await mocks(page);
      await page.goto(path);
      await expect(page.getByRole('heading', { level: 1 })).toHaveText('No page at this address');
      await expect(page.locator('code', { hasText: path })).toHaveCount(1);
      const door = page.getByRole('link', { name: '← Back to the Department Map' });
      await expect(door).toHaveAttribute('href', '/it');
      // In the IT chrome, whose landing the map is.
      await expect(page.locator('.perspective-tabs [aria-current="page"]')).toContainText('IT');
      await door.click();
      await expect(page).toHaveURL(/\/it$/);
      await expect(page.getByRole('heading', { level: 1 })).toHaveText('Department Map');
    });
  }

  test('Operate keeps its desk work and Design its reviews — the station tabs are gone', async ({ page }) => {
    await mocks(page);
    await page.goto('/it/operate/perf');
    await expect(page.locator('nav.it-tabs a')).toHaveText(['Incidents', 'Audit Log', 'Performance', 'Atlas', 'Bottlenecks']);
    await page.goto('/it/design/experiments');
    await expect(page.locator('nav.it-tabs a')).toHaveText(['Reviews', 'Experiments']);
  });

  test('the map says where each retired board lives now, so a reader who knew the tab finds it', async ({ page }) => {
    await mocks(page);
    await page.goto('/it');
    const header = page.locator('.exec-header').first();
    await expect(header).toContainText('Feedback and the IT backlog are in the Receiving station');
    await expect(header).toContainText('the Crew Board in the Shop floor');
    await expect(header).toContainText('yard status and the conductor');
  });
});

// ---------------------------------------------------------------------
// The content each retired page held, in its station's panel
// ---------------------------------------------------------------------

const TRAIN = {
  id: 'train-1', title: 'PR train 2026-09-19 04:30', phase: 'awaiting-ci', at_step: 'ci', block: null,
  ci_result: null, pr_url: null, car_count: 2, channel: null, boarded_at: '2026-09-19T04:30:00Z',
  eta: { kind: 'unknown', reason: 'too few arrivals to say' },
};

const STATUS = {
  trains: [TRAIN],
  dock: [{ id: 'car-1111', title: 'Teach the dock to breathe', branch: 'fix/dock-breathes', parked_since: '2026-09-19T04:00:00Z', red_trains: 0 }],
  held_cars: [{ id: 'car-9', branch: 'fix/held-for-reds', reason: 'two red trains', red_trains: 2, parked_since: '2026-09-18T20:00:00Z' }],
  recent: [{ id: 'train-0', title: 'PR train 2026-09-19 03:00', outcome: 'arrived', journey_seconds: 1500 }],
  stranded: [{ branch: 'fix/green-with-no-car' }],
  held: [{ branch: 'fix/held-green', reason: 'waiting on the migration', since: '2026-09-18T22:00:00Z' }],
  boarding: {
    dock_threshold: 4, cooldown_minutes: 45, at_times: [], dock_depth: 1, threshold_met: false,
    summary: 'Boards at 4 parked cars; 1 car parked now — below the dock threshold.',
  },
  gates: { capacity: 3, active: [] },
  garage: [],
  policy: { stall_hours: 2, max_red_trains: 2 },
  now: '2026-09-19T05:00:00Z',
};

test.describe('each retired page’s content is in its station’s panel', () => {
  test('the track carries the trains in flight, the recent trains and the conductor’s activity', async ({ page }) => {
    await mocks(page);
    await page.route(YARD_STATUS, (r) => json(r, STATUS));
    await page.goto('/it?at=track');
    const status = board(page, 'yard-status');
    await expect(status.locator('.ys-section')).toHaveText(['IN FLIGHT', 'RECENT TRAINS']);
    await expect(status).toContainText('PR train 2026-09-19 04:30');
    await expect(status).toContainText('PR train 2026-09-19 03:00');
    await expect(status.locator('.ys-footnote')).toContainText('a train stalls after 2h without progress');
    // The dock's and the garage's lanes are theirs, not the track's.
    await expect(status).not.toContainText('HELD CARS');
    await expect(status).not.toContainText('STRANDED GREENS');
    const conductor = board(page, 'conductor');
    await expect(conductor.locator('h3')).toHaveText('Conductor activity');
    // Its one read answered the catch-all's empty list: the feed says so.
    await expect(conductor).toContainText('No recent Conductor activity');
  });

  test('the dock carries its cars, its boarding sentence and the held cars', async ({ page }) => {
    await mocks(page);
    await page.route(YARD_STATUS, (r) => json(r, STATUS));
    await page.goto('/it?at=dock');
    const status = board(page, 'yard-status');
    await expect(status.locator('.ys-section')).toHaveText(['THE DOCK', 'HELD CARS']);
    await expect(status.locator('.ys-boarding')).toHaveText(STATUS.boarding.summary);
    await expect(status).toContainText('fix/dock-breathes');
    await expect(status).toContainText('fix/held-for-reds');
    await expect(status).toContainText('two red trains');
    await expect(board(page, 'conductor')).toHaveCount(0);
  });

  test('the garage carries the stranded and the held greens', async ({ page }) => {
    await mocks(page);
    await page.route(YARD_STATUS, (r) => json(r, STATUS));
    await page.goto('/it?at=garage');
    const status = board(page, 'yard-status');
    await expect(status.locator('.ys-section')).toHaveText(['STRANDED GREENS', 'HELD GREENS']);
    await expect(status).toContainText('fix/green-with-no-car');
    await expect(status).toContainText('waiting on the migration');
  });

  test('receiving carries the feedback board and the backlog board; marshalling the backlog board', async ({ page }) => {
    await mocks(page);
    await page.goto('/it?at=receiving');
    // Each board heads itself as a section of the panel, not a page.
    await expect(board(page, 'feedback').locator('h3.tb-title')).toHaveText('Feedback triage');
    await expect(board(page, 'backlog').locator('h3.tb-title')).toHaveText('IT backlog');
    await expect(page.getByRole('heading', { level: 1 })).toHaveText('Department Map');
    // Beside the Receiving Yard's own board, whose page header is dropped.
    await expect(page.locator(`${PANEL} .ry-root`)).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Receiving Yard' })).toHaveCount(0);

    await page.goto('/it?at=marshalling');
    await expect(board(page, 'backlog').locator('h3.tb-title')).toHaveText('IT backlog');
    await expect(board(page, 'feedback')).toHaveCount(0);
    await expect(page.locator(`${PANEL} .my-root`)).toBeVisible();
  });

  test('a station with no board of a retired page carries none', async ({ page }) => {
    await mocks(page);
    await page.goto('/it?at=gates');
    await expect(page.locator(`${PANEL} [data-station-board]`)).toHaveCount(0);
    await expect(page.locator(`${PANEL} [data-contents="gates"] .yard-deck`).first()).toBeVisible();
  });
});

// ---------------------------------------------------------------------
// The map, pinned
// ---------------------------------------------------------------------

test.describe('the map is pinned at the top', () => {
  // A window with room for it (MapPage.svelte PIN_QUERY: 900px tall).
  test.use({ viewport: { width: 1440, height: 1000 } });

  test('the panel scrolls beneath the map, and the map stays on the screen', async ({ page }) => {
    await mocks(page);
    await page.goto('/it?at=track');
    const pin = page.locator('[data-map-pin]');
    await expect(pin).toHaveAttribute('data-map-pin', 'pinned');
    expect(await pin.evaluate((el) => getComputedStyle(el).position)).toBe('sticky');
    await expect(board(page, 'conductor')).toBeVisible();

    // Down to the foot of the page: the map holds under the 44px bar.
    await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
    await expect.poll(() => page.evaluate(() => window.scrollY)).toBeGreaterThan(0);
    const box = (await pin.boundingBox())!;
    expect(Math.round(box.y)).toBe(44);
    // The panel's head has gone up under it; the map is what is on top.
    const head = (await page.locator(`${PANEL} .panel-head`).boundingBox())!;
    expect(head.y).toBeLessThan(box.y + box.height);
    const onTop = await page.evaluate(({ x, y }) => document.elementFromPoint(x, y)?.closest('[data-map-pin]') !== null, {
      x: box.x + box.width / 2,
      y: box.y + box.height / 2,
    });
    expect(onTop).toBe(true);
  });

  test('a selection lands the panel right under the pinned map', async ({ page }) => {
    await mocks(page);
    await page.goto('/it?at=dock');
    await expect(page.locator('[data-map-pin]')).toHaveAttribute('data-map-pin', 'pinned');
    const pin = (await page.locator('[data-map-pin]').boundingBox())!;
    const panel = (await page.locator(PANEL).boundingBox())!;
    expect(panel.y).toBeGreaterThanOrEqual(pin.y + pin.height - 1);
    expect(panel.y).toBeLessThan(pin.y + pin.height + 40);
  });

  test('what the pinned map covers is not "in view": the page scroll padding is the bar and the map, and goes with the page', async ({ page }) => {
    await mocks(page);
    await page.goto('/it?at=dock');
    const pin = (await page.locator('[data-map-pin]').boundingBox())!;
    const padding = () => page.evaluate(() => document.documentElement.style.scrollPaddingTop);
    // The map's height as the layout rounds it (clientHeight), against the
    // box's fractional one: within a pixel.
    await expect.poll(async () => Math.abs(parseFloat(await padding()) - (44 + pin.height))).toBeLessThanOrEqual(1);
    await page.goto('/it/estate');
    await expect(page.getByRole('heading', { level: 1 })).toHaveText('The estate');
    await expect.poll(padding).toBe('');
  });
});

test.describe('where a pinned map would leave the panel no room, it scrolls with the page', () => {
  test('a laptop-height window (1280×800)', async ({ page }) => {
    await mocks(page);
    await page.goto('/it?at=track');
    const pin = page.locator('[data-map-pin]');
    await expect(pin).toHaveAttribute('data-map-pin', 'scrolls');
    expect(await pin.evaluate((el) => getComputedStyle(el).position)).toBe('static');
    expect(await page.evaluate(() => document.documentElement.style.scrollPaddingTop)).toBe('');
  });

  test('a phone', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 1000 });
    await mocks(page);
    await page.goto('/it?at=track');
    await expect(page.locator('[data-map-pin]')).toHaveAttribute('data-map-pin', 'scrolls');
  });
});

// ---------------------------------------------------------------------
// The floors' boards, carried from the retired floor pages
// ---------------------------------------------------------------------

test.describe('what the floor pages held, in the panel', () => {
  test('the dock’s panel opens its floor on the loading dock, headed by the region’s verdict', async ({ page }) => {
    await mocks(page);
    await page.goto('/it?at=dock');
    await expect(page.locator(`${PANEL} .yard-panel-h`, { hasText: 'Entity · loading dock' })).toBeVisible();
    await expect(page.locator(`${PANEL} .yard-region-head`)).toContainText('dock · clear — 2 cars parked');
    // The departure board is the dock's, titled for it.
    const head = page.locator(`${PANEL} .head[data-board="dock"]`);
    await expect(head).toContainText('Departure board · dock');
    await expect(head).toContainText('2 in flight · 0 landed');
  });

  test("a quiet alerts strip under a troubled station does not call the station's machines fine", async ({ page }) => {
    await mocks(page);
    const troubled = {
      ...REGIONS,
      regions: REGIONS.regions.map((r) =>
        r.name === 'shed' ? { ...r, state: 'troubled', why: '2 landed cars of ours owe a proof' } : r),
    };
    await page.route(YARD_REGIONS, (r) => json(r, troubled));
    await page.goto('/it?at=shed');
    const strip = page.locator(`${PANEL} .yard-alerts[data-alerts="shed"]`);
    await expect(strip).toContainText("No machine alerts in the shed — its troubled state above is the region's own reading");
    await expect(strip).not.toContainText('every machine here is working');
  });

  // Stations OVERLAP: on 2026-09-23 the sidings summed to 517 over 303
  // distinct packets, and nothing on the board said so (backlog 140a2222).
  test('the marshalling board says its sidings overlap, so the depth column is not read as a count of the work', async ({ page }) => {
    await mocks(page);
    await page.route(/\/api\/stations\/load$/, (r) =>
      json(r, {
        data: [
          { station: 'q.platform-admin.task', kind: 'constraint', depth: 297, also_elsewhere: 213 },
          { station: 'a.platform-admin.opus-5-1m', kind: 'constraint', depth: 213, also_elsewhere: 213 },
          { station: 'q.platform-admin.sign-off', kind: 'constraint', depth: 6, also_elsewhere: 0 },
          { station: 'loading-dock', kind: 'batch', depth: 1, also_elsewhere: 0 },
        ],
        distinct_packets: 303,
      }));
    await page.route(/\/api\/stations\/flow/, (r) => json(r, { window_hours: 24, as_of: '2026-09-19T05:00:00Z', data: [] }));
    await page.goto('/it?at=marshalling');
    const overlap = page.locator(`${PANEL} .my-root .my-overlap`);
    await expect(overlap).toContainText('The depths sum to 517 but hold 303 distinct packets');
    await expect(overlap).toContainText('a.platform-admin.opus-5-1m 213 of 213');
  });

  // The waits table cut to 12 of 320 real obligations on 2026-09-23 and
  // said neither number (backlog 18683a0a, page-audit 7c228914 gap 8).
  test('the marshalling waits table counts what its cap leaves out, so a limit does not read as a filter', async ({ page }) => {
    await mocks(page);
    const wait = (i: number, simulated: boolean) => ({
      job_id: `wait-${i}`, job_kind: 'backlog-item', job_title: `packet ${i}`, step_title: 'Build the change',
      status: 'ready', assignee_id: null, simulated, partition: simulated ? 'simulated' : 'real',
      exact: true, waiting_days: 20 - i,
    });
    await page.route(/\/api\/stations\/load$/, (r) =>
      json(r, { data: [{ station: 'q.platform-admin.task', kind: 'constraint', depth: 30, wip_limit: 24, over_limit: true, oldest_age_days: 3 }] }));
    await page.route(/\/api\/stations\/flow/, (r) =>
      json(r, { window_hours: 24, as_of: '2026-09-19T05:00:00Z', data: [{ station: 'q.platform-admin.task', basis: 'step-events', arrived: 9, served: 4, net: 5 }] }));
    await page.route(/\/api\/jobs\/queue-age$/, (r) =>
      json(r, {
        data: [...Array.from({ length: 14 }, (_, i) => wait(i, false)), wait(99, true)],
        total: 15,
        now: '2026-09-19T05:00:00Z',
      }));
    await page.goto('/it?at=marshalling');
    await expect(page.locator(`${PANEL} .my-root .my-waits-count`)).toHaveText(
      '12 of 14 outstanding obligations, longest first · +2 more not shown · 1 on simulated or shadow packets not ranked',
    );
    await expect(page.locator(`${PANEL} .my-root a`, { hasText: /^packet \d+$/ })).toHaveCount(12);
  });

  test('a queue read that failed is said on the board, never drawn as an empty station', async ({ page }) => {
    await mocks(page);
    await page.route(/\/api\/stations\/load$/, (r) => json(r, 'the station registry is down', 500));
    await page.goto('/it?at=marshalling');
    await expect(page.locator(`${PANEL} .my-root .load-failed`).first()).toContainText('did not answer');
    await expect(page.locator(`${PANEL} .my-root`)).not.toContainText('Every watched station is clear.');
  });

  // The live shape of the shop floor (2026-09-24): every open session is
  // the SAME actor. The region map keyed its platforms by actor and threw
  // each_key_duplicate, leaving the page on "Reading the regions…"
  // (backlog 846ab934). The region map is gone; the station's panel draws
  // the Crew Board over the same reads, and must stand on that shape.
  test('the shop floor’s panel stands two sessions of one actor, and draws its Crew Board', async ({ page }) => {
    await mocks(page);
    const session = (id: string, started: string) => ({
      id, kind: 'work-session', title: 'a session', status: 'open',
      metadata: { actor: 'claude@algedonic.dev', host: 'boss-dev', started_at: started, last_active_at: started, prompt_count: 3 },
    });
    await page.route(/\/api\/jobs\?kind=work-session/, (r) =>
      json(r, { total: 2, data: [
        session('87b3cf48-f17b-4126-98b4-7673a3c3576b', '2026-09-24T03:00:00Z'),
        session('7bb6e37d-d168-40b8-9675-70a42229a2bb', '2026-09-24T04:00:00Z'),
      ] }));
    await page.goto('/it?at=shop-floor');
    await expect(page.getByText('Reading the regions…')).toHaveCount(0);
    await expect(page.locator(`${PANEL} .crew-root`)).toBeVisible();
    await expect(page.locator(`${PANEL} .crew-section`).first()).toHaveText('00 — THE BUILDER PIPELINE');
    await expect(page.locator(`${PANEL}`)).not.toContainText('cannot be drawn');
  });

  // THE PLANT (design 62de32ae, decision 11): the host runners stand along
  // the map's edge with their names and states written.
  test('the map draws its plant along the edge, each machine with its name and state', async ({ page }) => {
    await mocks(page);
    await page.route(YARD_REGIONS, (r) =>
      json(r, {
        ...REGIONS,
        plant: [
          { id: 'runner:host:forge', name: 'forge runner', state: 'idle', why: 'last df answered exit 0' },
          { id: 'runner:host:boss-gcp', name: 'boss-gcp runner', state: 'failed', why: 'the last converge request was refused' },
        ],
      }));
    await page.goto('/it');
    const plant = page.locator('section[aria-label="the plant — machinery serving every region"]');
    await expect(plant).toHaveCount(1);
    await expect(plant).toContainText('Plant · serves every region');
    const glyphs = plant.locator('.glyph');
    await expect(glyphs).toHaveCount(2);
    await expect(glyphs.first()).toHaveAttribute('data-machine', 'runner:host:boss-gcp');
    await expect(glyphs.first()).toContainText('boss-gcp runner · failed');
    await expect(plant.locator('[data-machine="runner:host:forge"]')).toContainText('forge runner · idle');
  });
});
