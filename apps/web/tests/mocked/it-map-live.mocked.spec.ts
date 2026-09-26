// REAL PACKETS MOVING ON THE MAP — behind the flight `it-map-live`
// (design e765b3fc §3, car M2 on feedback 84cba7e2). The rules are
// pinned in live-motion.test.ts; this pins the page against a MOCKED
// GET /api/yard/moves/stream carrying the server's own frames (car M1,
// crates/core/boss-jobs/src/http/moves.rs): one move is one transit
// and then one ping; 41 moves are 40 dots and "+1"; an off-ramp, an
// undeclared route, a hand-off and a train each draw as the design
// says; a hidden tab draws nothing and runs no frames, then pings once
// with its count; forty dots fit the 2 ms a frame is allowed; and a
// feed that cannot be read is said on the map. Flight off, nothing of
// it is mounted and the rate-replay blocks run as before.
//
// A mocked stream is a whole body the browser reads and then ends
// (audit-log-page.mocked.spec.ts says what that can reach): its
// `retry` holds the reconnect an hour off, so each test sees its frames
// exactly once.

import { expect, test, type Page, type Route } from '@playwright/test';
import { LIVE_TEXT } from '../../src/it/yard/live-motion';
import { TERRITORIES } from '../../src/it/yard/world';
import { BORDERS, routesPayload } from '../fixtures/yard';
import { YARD_BORDERS, YARD_REGIONS, YARD_ROUTES, installSmokeMocks } from './_smokeMocks';

const FLIGHTS = /\/api\/flights\/mine(\?|$)/;
const STREAM = /\/api\/yard\/moves\/stream(\?|$)/;
const MOVES = /\/api\/yard\/moves(\?|$)/;
const NOW = '2026-09-26T12:00:00Z';
const LIVE = ['it-map-transit', 'it-map-live'];
/** The flight's declared draw budget (design e765b3fc §3). */
const FRAME_BUDGET_MS = 2;

const json = (r: Route, b: unknown) => r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });

const regions = () => ({
  window_hours: 24,
  now: NOW,
  regions: TERRITORIES.map(({ name }) => ({
    name, count: 1, unit: 'things', state: 'clear', why: 'fine',
    trend: { metric: 'crossings', unit: 'per day', current: 4, previous: 4, samples: 4, previous_samples: 4 },
  })),
});

/** Every section busy and flowing — so the rate-replay blocks WOULD run
 *  if the live flight did not switch them off. */
const borders = () => ({
  window_hours: 24,
  now: NOW,
  borders: BORDERS.map(({ from, to }) => ({
    from, to, crossing: 'a packet crossed', state: 'clear', why: 'flowing',
    rate: { metric: 'crossings', unit: 'per day', current: 480, previous: 400, samples: 480, previous_samples: 400 },
    last_crossed: '2026-09-26T11:58:00Z', waiting: 0, holds: [], holds_by_class: null,
    flowing: true, held_since: null, flowing_why: 'last crossed 2m ago',
    machine: { name: 'a rule', kind: 'dispatcher-rule', last_fired: null, silent_for_minutes: null,
      expected_every_minutes: null, silent: null, why: 'no firing recorded' },
  })),
});

type Wire = Readonly<Record<string, unknown>>;
/** One `Recorded` row as moves.rs serialises it: the seq, the move flattened. */
const move = (seq: number, over: Wire = {}): Wire => ({
  seq,
  at: '2026-09-26T11:59:30Z',
  packet: `car-${seq}`,
  kind: 'ship-a-change',
  label: `feat/change-${seq}`,
  from: 'shop-floor',
  to: 'gates',
  declared: true,
  cause_event_id: `00000000-0000-0000-0000-${String(seq).padStart(12, '0')}`,
  cause_seq: 1000 + seq,
  cause_kind: 'jobs.step.updated',
  handoff_from: null,
  lineage: null,
  aboard: [],
  ...over,
});

/** The stream's body: the resync a fresh connection is sent, then each
 *  move under its seq — the frames `sse()` in moves.rs writes. */
const body = (moves: ReadonlyArray<Wire>): string =>
  [
    'retry: 3600000\n',
    `event: resync\ndata: ${JSON.stringify({ frame: 'resync', seq: 0, reason: 'connected: the current placement is the baseline' })}\n`,
    ...moves.map((m) => `id: ${String(m.seq)}\nevent: move\ndata: ${JSON.stringify(m)}\n`),
  ].join('\n') + '\n';

async function mocks(page: Page, flights: ReadonlyArray<string>, moves: ReadonlyArray<Wire>): Promise<void> {
  await installSmokeMocks(page);
  await page.route(FLIGHTS, (r) => json(r, { flights }));
  await page.route(YARD_REGIONS, (r) => json(r, regions()));
  await page.route(YARD_BORDERS, (r) => json(r, borders()));
  await page.route(YARD_ROUTES, (r) => json(r, routesPayload()));
  await page.route(STREAM, (r) =>
    r.fulfill({ status: 200, headers: { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' }, body: body(moves) }),
  );
}

const live = (page: Page) => page.locator('section.transit [data-live]');

/** Stop the page's clock where it stands, so a 700 ms ping is still
 *  there when the test looks — the clock then moves only by runFor. */
async function pause(page: Page): Promise<void> {
  const now = await page.evaluate(() => Date.now());
  await page.clock.pauseAt(now + 10);
}

test('flight OFF: no live layer, and the rate-replay blocks run as before', async ({ page }) => {
  await mocks(page, ['it-map-transit'], [move(1)]);
  await page.goto('/it');
  await expect(page.locator('section.transit[data-transit]')).toBeVisible();
  await expect(page.locator('[data-live]')).toHaveCount(0);
  expect(await page.locator('[data-train="shop-floor→gates"]').count()).toBeGreaterThan(0);
  await expect(page.locator('[data-replay]')).toContainText('replayed ×60');
});

test('one move is one transit, then one ping — and the replay blocks are off', async ({ page }) => {
  await page.clock.install({ time: new Date(NOW) });
  await mocks(page, LIVE, [move(1)]);
  await page.goto('/it');
  const g = live(page);
  await expect(g.locator('[data-dot]')).toHaveCount(1);
  await pause(page);
  const dot = g.locator('[data-dot="1"]');
  await expect(dot).toHaveAttribute('data-route', 'shop-floor→gates');
  await expect(dot).toHaveAttribute('data-packet', 'car-1');
  // Hovering names the packet, the time it moved and the event that moved it.
  await expect(dot.locator('title')).toContainText('feat/change-1');
  await expect(dot.locator('title')).toContainText('moved 11:59:30Z');
  await expect(dot.locator('title')).toContainText('jobs.step.updated 00000000-0000-0000-0000-000000000001');
  await expect(g.locator('[data-ping]')).toHaveCount(0);
  // The rate replay is not drawn beside the real moves, and the page says what a dot is.
  await expect(page.locator('[data-train]')).toHaveCount(0);
  await expect(page.locator('[data-replay]')).toContainText(LIVE_TEXT);

  await page.clock.runFor(6_100);
  await expect(g.locator('[data-dot]')).toHaveCount(0);
  await expect(g.locator('[data-ping]')).toHaveCount(1);
  await expect(g.locator('[data-ping="gates"]')).toHaveAttribute('data-k', '1');
  // The ping swells once and is gone; an idle map runs no frames.
  await page.clock.runFor(1_000);
  await expect(g.locator('[data-ping]')).toHaveCount(0);
});

test('41 moves are 40 dots and "+1" on the section', async ({ page }) => {
  await mocks(page, LIVE, Array.from({ length: 41 }, (_, i) => move(i + 1)));
  await page.goto('/it');
  const g = live(page);
  await expect(g.locator('[data-dot]')).toHaveCount(40);
  await expect(g.locator('[data-motion-more="shop-floor→gates"]')).toHaveText('+1');
  // Forty in flight fit the draw budget, measured headless.
  await expect
    .poll(async () => Number((await g.getAttribute('data-frame-ms')) ?? 'NaN'))
    .toBeLessThan(FRAME_BUDGET_MS);
});

test('an off-ramp, an undeclared route, a hand-off and a train each draw as the record says', async ({ page }) => {
  await mocks(page, LIVE, [
    move(1, { from: 'dock', to: null }),
    move(2, { from: 'gates', to: 'receiving', declared: false }),
    move(3, { from: 'gates', to: 'dock', handoff_from: 'gate-1', lineage: 'branch' }),
    move(4, { from: 'gates', to: 'track', kind: 'pr-train', label: 'train 17:40', aboard: ['c1', 'c2', 'c3'] }),
    move(5, { from: null, to: 'receiving', kind: 'backlog-item', label: 'a new item' }),
  ]);
  await page.goto('/it');
  const g = live(page);
  await expect(g.locator('[data-dot]')).toHaveCount(5);
  // Filed onto the map: it travels receiving's served entry.
  await expect(g.locator('[data-dot="5"]')).toHaveAttribute('data-route', '∅→receiving');
  // Leaving by an off-ramp: it travels off the map.
  await expect(g.locator('[data-dot="1"]')).toHaveAttribute('data-off', 'true');
  await expect(g.locator('[data-dot="1"]')).toHaveAttribute('data-route', 'dock→∅');
  // On no declared route: the dashed-red undeclared line, under its dot.
  await expect(g.locator('[data-undeclared-route]')).toHaveCount(1);
  await expect(g.locator('[data-undeclared-route="gates→receiving"]')).toHaveCount(1);
  // A hand-off: the gate-run fades where the car sets off.
  await expect(g.locator('[data-handoff="gate-1"]')).toHaveCount(1);
  // A train: a bar with a pip per car aboard.
  await expect(g.locator('[data-dot="4"] [data-train-pips="3"]')).toHaveCount(1);
  await expect(g.locator('[data-dot="4"] .pip')).toHaveCount(3);
});

test('a hidden tab draws nothing, then pings once per station with its count', async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => true });
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' });
  });
  await page.clock.install({ time: new Date(NOW) });
  await mocks(page, LIVE, [move(1), move(2), move(3, { from: 'gates', to: 'dock' })]);
  await page.goto('/it');
  const g = live(page);
  await expect(g).toHaveAttribute('data-away', '3');
  await expect(g.locator('[data-dot]')).toHaveCount(0);
  await expect(g.locator('[data-ping]')).toHaveCount(0);
  // No frame ran: an idle, hidden map costs nothing.
  await expect(g).not.toHaveAttribute('data-frames', /.*/);

  await pause(page);
  await page.evaluate(() => {
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => false });
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'visible' });
    document.dispatchEvent(new Event('visibilitychange'));
  });
  // No backlog replayed: one ping per station, carrying its count.
  await expect(g.locator('[data-dot]')).toHaveCount(0);
  await expect(g.locator('[data-ping="gates"]')).toHaveAttribute('data-k', '2');
  await expect(g.locator('[data-ping="dock"]')).toHaveAttribute('data-k', '1');
  await expect(g.locator('[data-away-note]')).toHaveText('3 moves while away');
});

test('a feed that cannot be read is said on the map, never drawn as a quiet yard', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(FLIGHTS, (r) => json(r, { flights: LIVE }));
  await page.route(YARD_REGIONS, (r) => json(r, regions()));
  await page.route(YARD_BORDERS, (r) => json(r, borders()));
  await page.route(YARD_ROUTES, (r) => json(r, routesPayload()));
  // The stream refused (the browser gives it up), and the record's read refused too.
  await page.route(STREAM, (r) => r.fulfill({ status: 503, body: 'the moves record is not wired on this jobs API' }));
  await page.route(MOVES, (r) => r.fulfill({ status: 403, body: 'the moves record names packets; this caller may read none' }));
  await page.goto('/it');
  const g = live(page);
  await expect(g).toHaveAttribute('data-feed', 'down');
  await expect(g.locator('[data-feed-down]')).toContainText('403');
  await expect(g.locator('[data-feed-down]')).toContainText('this caller may read none');
});
