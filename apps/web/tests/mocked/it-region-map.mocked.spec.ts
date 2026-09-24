// CLICKING A TERRITORY SWAPS THE VIEW — David's correction,
// 2026-09-20 (backlog ca37478f), over design d2154293 car 3.
//
// Car 3 built the literal reading of "zoom into the region by clicking
// to see" (feedback c3105b2a): one SVG, one coordinate space, the
// viewBox walking into a territory's rect, every mark growing in
// proportion. David: "I realize I made the 'zoom the world map'
// direction way too literal. I just wanted the world map view to get
// replaced with the more detailed region map view on click but not
// literally increase the size of content on the world map."
//
// So this spec now asserts the OPPOSITE of what it used to. The world
// map is REPLACED by the region's own map, which owns its whole canvas
// and lays out for what the region contains rather than for the slot
// its rectangle occupied on the world line. The route is unchanged —
// /it/yard/<region> was already right; only what it drew was wrong.
//
// WHAT THIS SPEC CAN OBSERVE: that the world map is gone from the DOM
// and the region's map is present; that the region map carries the
// region's own head (its count, its state, its why) and its slice of
// the floor with the ids and tags the fixtures put there, each wagon a
// button into the entity panel (design fe77a1d2, car 2); that the
// floor panels still mount beneath it, with no second map; and that
// Escape and the way-out control both return to the world. There is no camera left to observe, and the
// easing and interpolation it needed are deleted rather than unused.

import { expect, test, type Page, type Route } from '@playwright/test';
import { TERRITORIES } from '../../src/it/yard/world';
import { YARD_REGIONS, installSmokeMocks } from './_smokeMocks';

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

/** Two cars standing on the loading dock — what the dock's interior has
 *  to paint. The nameplate is taken off the branch, as the floor takes
 *  every nameplate. */
const DOCK_QUEUE = {
  discipline: ['parked_since'],
  data: [
    { id: 'car-1111', kind: 'ship-a-change', title: 'Teach the dock to breathe', status: 'open', metadata: { branch: 'fix/dock-breathes' } },
    { id: 'car-2222', kind: 'ship-a-change', title: 'Name the failing check', status: 'open', metadata: { branch: 'fix/name-the-check' } },
  ],
};

async function mocks(page: Page): Promise<void> {
  await installSmokeMocks(page);
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route(YARD_REGIONS, (r) => json(r, REGIONS));
  await page.route(/\/api\/stations\/loading-dock\/queue$/, (r) => json(r, DOCK_QUEUE));
}

/** The world map's own SVG, scoped by its aria-label: a region map is
 *  a `section.yard svg` too. */
const WORLD_SVG = 'section[aria-label="the IT world map"] svg';

/** A region's own map — the surface that REPLACES the world. */
const regionMap = (name: string) => `section[aria-label="the ${name} region map"]`;
const regionSvg = (name: string) => `${regionMap(name)} svg`;

test('/it/yard/dock REPLACES the world with the dock\'s own map — not the world drawn nearer', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const svg = page.locator(WORLD_SVG);
  await expect(svg.locator('.territory')).toHaveCount(TERRITORIES.length);

  // The world at rest, and the node it is drawn in. The handle is the
  // load-bearing part, exactly as it was when this spec asserted the
  // opposite: it proves which way the swap went.
  const node = await svg.elementHandle();
  expect(node).not.toBeNull();

  await svg.locator('.territory[data-region="dock"]').click();
  await expect(page).toHaveURL(/\/it\/yard\/dock$/);

  // THE WORLD IS GONE. Replaced, not zoomed — and the old SVG node is
  // detached, which is the assertion that earns the spec's title.
  await expect(page.locator(WORLD_SVG)).toHaveCount(0);
  expect(await node!.evaluate((el) => el.isConnected)).toBe(false);
  await expect(page.locator('.territory')).toHaveCount(0);

  // THE REGION'S OWN MAP, in its place: its head says what the region
  // is, in words, at region scale rather than wrapped into a slot.
  const map = page.locator(regionMap('dock'));
  await expect(map).toHaveCount(1);
  await expect(map).toHaveAttribute('data-state', 'clear');
  await expect(map.locator('.region-name')).toHaveText('dock');
  await expect(map.locator('.region-count')).toContainText('2');
  await expect(map.locator('.region-why')).toContainText('2 cars parked');

  // THE DOCK'S OWN SLICE OF THE FLOOR (design fe77a1d2, car 2): its
  // stretch of the mainline with the same wagons, ids and tags the
  // floor carries — drawn once, here, and not again under it.
  const floor = page.locator(`${regionSvg('dock')} .floor[data-floor="dock"]`);
  await expect(floor).toHaveCount(1);
  await expect(floor.locator('.token.wagon')).toHaveCount(2);
  await expect(floor.locator('.token.wagon[data-car="car-1111"]')).toHaveAttribute('data-station', 'dock');
  await expect(floor.locator('.token.wagon[data-car="car-1111"] title')).toHaveText(/Teach the dock to breathe/);
  await expect(page.locator('section[aria-label="the yard map"]')).toHaveCount(0);

  // The floor's panels are still mounted beneath it — the yard's own
  // deck, on the same page. The swap changed the map, not the floor.
  await expect(page.locator('.yard-panel-h', { hasText: 'Entity · loading dock' })).toBeVisible();
  await expect(page.locator('.yard-region-head')).toContainText('dock · clear — 2 cars parked');
});

test("a wagon on the region's own map selects into the entity panel, as the floor's did", async ({ page }) => {
  await mocks(page);
  await page.goto('/it/yard/dock');
  const wagon = page.locator(`${regionSvg('dock')} .floor .token.wagon[data-car="car-2222"]`);
  await expect(wagon).toHaveCount(1);
  await expect(wagon).toHaveAttribute('role', 'button');

  // The keyboard reaches it the way the pointer does; an SVG group has
  // no fill for a pointer to land on reliably, so both are the handler.
  await wagon.focus();
  await page.keyboard.press('Enter');
  await expect(page.locator('.yard-panel-h', { hasText: 'Entity · car' })).toBeVisible();
  await expect(wagon).toHaveClass(/selected/);

  // And the area itself: the dock's siding selects the dock again.
  await page.locator(`${regionSvg('dock')} .floor .machine[aria-label^="loading dock"]`).dispatchEvent('click');
  await expect(page.locator('.yard-panel-h', { hasText: 'Entity · loading dock' })).toBeVisible();
  await expect(wagon).not.toHaveClass(/selected/);
});

test('Escape goes back to the world, and so does the way-out control', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const svg = page.locator(WORLD_SVG);
  await expect(svg.locator('.territory')).toHaveCount(TERRITORIES.length);

  await svg.locator('.territory[data-region="dock"]').click();
  await expect(page).toHaveURL(/\/it\/yard\/dock$/);
  await expect(page.locator(regionMap('dock'))).toHaveCount(1);

  await page.keyboard.press('Escape');
  await expect(page).toHaveURL(/\/it$/);
  // The world is back, whole: eight territories, no region map.
  await expect(page.locator(WORLD_SVG).locator('.territory')).toHaveCount(TERRITORIES.length);
  await expect(page.locator(regionMap('dock'))).toHaveCount(0);

  // The way out is SAID, not only bound to a key.
  await page.locator(`${WORLD_SVG} .territory[data-region="dock"]`).click();
  await expect(page).toHaveURL(/\/it\/yard\/dock$/);
  await page.locator(`${regionMap('dock')} button.leave`).click();
  await expect(page).toHaveURL(/\/it$/);
  await expect(page.locator(WORLD_SVG).locator('.territory')).toHaveCount(TERRITORIES.length);
});

// ---------------------------------------------------------------------
// CAR 4 — the two territories that hold QUEUES.
// ---------------------------------------------------------------------
//
// Until this car receiving and marshalling printed "this region's
// floor is a page of its own" where their activity belonged. They now
// draw a platform per queue: the station's or channel's name, what is
// standing over its bound, a mark per packet on the track, the bound
// where it falls, and what left in the window. What this spec can
// observe is the picture the operator gets — the platforms, the marks
// and, the reason the car has a rule of its own, that a number the
// server could not count renders as `?` and NOT as 0.

/** Two stations, one of them blind to the flow cube — the honest case
 *  the marshalling read model already models (`basis: unavailable`). */
const STATION_LOAD = {
  data: [
    { station: 'q.platform-admin.task', kind: 'constraint', depth: 30, wip_limit: 24, over_limit: true, oldest_age_days: 3, capability_roles: ['platform-admin'] },
    { station: 'my-watchlist', kind: 'actor', depth: 2, wip_limit: null, over_limit: false, oldest_age_days: 1, capability_roles: null },
  ],
};
const STATION_FLOW = {
  window_hours: 24,
  as_of: '2026-09-19T05:00:00Z',
  data: [
    { station: 'q.platform-admin.task', basis: 'step-events', arrived: 9, served: 4, net: 5 },
    { station: 'my-watchlist', basis: 'unavailable', arrived: null, served: null, net: null, unavailable_reason: 'a per-actor watchlist is not a log predicate' },
  ],
};

/** One inbound kind, with a packet standing well past the 14-day band. */
const INBOUND_WORKFLOWS = [{ kind: 'user-feedback', category: 'platform', status: 'active' }];
const INBOUND_JOBS = {
  total: 1,
  data: [
    { id: 'fb-1', kind: 'user-feedback', title: 'the map should feel like a world', status: 'open', opened_on: '2026-08-01', priority: 'standard', steps: [] },
  ],
};

test("marshalling's own map draws a platform per station — its packets, its bound, and a rate it could not count as unknown", async ({ page }) => {
  await mocks(page);
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route(/\/api\/stations\/load$/, (r) => json(r, STATION_LOAD));
  await page.route(/\/api\/stations\/flow/, (r) => json(r, STATION_FLOW));

  await page.goto('/it/yard/marshalling');
  await expect(page.locator(WORLD_SVG)).toHaveCount(0);
  await expect(page.locator(regionMap('marshalling'))).toHaveCount(1);

  const interior = page.locator(`${regionSvg('marshalling')} .interior[data-interior="marshalling"]`);
  await expect(interior.locator('.platform')).toHaveCount(2);
  await expect(interior).not.toContainText("page of its own");

  // The station over its WIP bound: 30 standing against 24, the marks
  // past the bound flagged, and the bound drawn on the track.
  const over = interior.locator('.platform[data-platform="q.platform-admin.task"]');
  await expect(over).toContainText('30 / 24');
  await expect(over.locator('.mark')).not.toHaveCount(0);
  await expect(over.locator('.mark.flagged')).not.toHaveCount(0);
  await expect(over.locator('.bound')).toHaveCount(1);
  await expect(over.locator('title')).toHaveText(/oldest 3 d/);

  // THE UNKNOWN. The watchlist's depth was read, so it stands; its
  // rate was not, so the platform says `?` — in its own band, and
  // never the 0 that would read as "nothing is being worked".
  const blind = interior.locator('.platform[data-platform="my-watchlist"]');
  await expect(blind).toContainText('2');
  await expect(blind.locator('text.rate')).toHaveText('?');
  await expect(blind.locator('text.rate')).toHaveClass(/unknown/);
  await expect(blind.locator('title')).toHaveText(/a per-actor watchlist is not a log predicate/);
  // A counted rate is a figure, not the unknown band — the two must
  // not look alike.
  await expect(over.locator('text.rate')).not.toHaveClass(/unknown/);

  // The board itself is mounted under the region map: the page that
  // used to live at /it/operate/marshalling, minus its page header.
  await expect(page.locator('.my-root')).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Marshalling Yard' })).toHaveCount(0);
});

test("receiving's own map stands its inbound packets by channel and flags what is past the age band", async ({ page }) => {
  await mocks(page);
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route(/\/api\/workflows$/, (r) => json(r, INBOUND_WORKFLOWS));
  await page.route(/\/api\/jobs\?kind=user-feedback/, (r) => json(r, INBOUND_JOBS));

  await page.goto('/it/yard/receiving');
  await expect(page.locator(WORLD_SVG)).toHaveCount(0);
  const receiving = page.locator(regionMap('receiving'));
  await expect(receiving).toHaveCount(1);
  await expect(receiving).not.toContainText('page of its own');

  const interior = page.locator(`${regionSvg('receiving')} .interior[data-interior="receiving"]`);
  const feedback = interior.locator('.platform[data-platform="feedback"]');
  await expect(feedback).toContainText('1');
  // Opened 2026-08-01: past the 14-day band, so its mark is flagged —
  // the trouble is drawn where the trouble is.
  await expect(feedback.locator('.mark.flagged')).toHaveCount(1);
  await expect(feedback.locator('title')).toHaveText(/past the 14-day band/);
  // A channel nothing stands on is drawn empty, not omitted: "where
  // is the work NOT" is an answer a board still owes.
  await expect(interior.locator('.platform[data-platform="design"] .mark')).toHaveCount(0);

  // And the inbound board is mounted under the region map, header dropped.
  await expect(page.locator('.ry-root')).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Receiving Yard' })).toHaveCount(0);
});

test('a queue read that failed is said, never drawn as an empty region', async ({ page }) => {
  await mocks(page);
  await page.route(/\/api\/stations\/load$/, (r) =>
    r.fulfill({ status: 500, contentType: 'application/json', body: '"the station registry is down"' }));

  await page.goto('/it/yard/marshalling');
  const map = page.locator(regionMap('marshalling'));
  await expect(map).toContainText('cannot be read');
  await expect(page.locator(`${regionSvg('marshalling')} [data-interior]`)).toHaveCount(0);
});

test('a direct load of /it/yard/<region> renders the region map and never the world', async ({ page }) => {
  await mocks(page);
  await page.goto('/it/yard/gates');
  // The seeded-camera question this replaces ("does it fly in from the
  // world?") cannot arise once the view is swapped: there is one
  // surface for the route, and it is the region's.
  await expect(page.locator(regionMap('gates'))).toHaveCount(1);
  await expect(page.locator(WORLD_SVG)).toHaveCount(0);
  await expect(page.locator('.territory')).toHaveCount(0);
  await expect(page.locator(`${regionMap('gates')} .region-name`)).toHaveText('gates');
});

test('a region the layout does not know leaves the WORLD on screen, never a map of nothing', async ({ page }) => {
  await mocks(page);
  await page.goto('/it/yard/atlantis');
  await expect(page.locator(WORLD_SVG).locator('.territory')).toHaveCount(TERRITORIES.length);
  await expect(page.locator('section[aria-label$="region map"]')).toHaveCount(0);
});
