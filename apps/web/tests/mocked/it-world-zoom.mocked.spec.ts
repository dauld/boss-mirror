// CLICKING A TERRITORY IS A ZOOM, NOT A DEPARTURE — design d2154293,
// car 3, over car 1's world (it-map.mocked.spec.ts). David's feedback
// c3105b2a asked to "zoom into the region by clicking to see" what is
// moving inside it, and the packet's claim is stronger than "a floor
// opens": the world NEVER LEAVES. /it/yard/dock is the same SVG, the
// same node, with the camera walked into the dock's rect and the dock's
// own wagons drawn inside it.
//
// WHAT THIS SPEC CAN OBSERVE, and what it cannot. It CAN observe that
// the SVG element the world was drawn in is the SAME DOM node after the
// click (a node handle, checked for identity — a route that swapped
// pages would tear it down); that the settled viewBox contains the
// clicked territory's own rect and is a fraction of the world's; that
// the plates inside carry the ids and tags of the cars the fixtures put
// on the dock; and that Escape puts the camera back exactly where it
// started. It CANNOT observe that the walk between the two boxes LOOKS
// like a camera move — the frames are requestAnimationFrame work, and
// the assertions here read the ends, not the middle. The easing and the
// interpolation are unit-pinned in world-zoom.test.ts instead; what is
// left unpinned by both, honestly, is whether the motion reads well to
// a person, which is a thing to look at rather than a thing to assert.

import { expect, test, type Page, type Route } from '@playwright/test';
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

/** The world's own SVG. Scoped by the map's aria-label because the
 *  yard's detailed floor map below it is a `section.yard svg` too —
 *  until cars 4-5 re-home its sidings inside the territories, both are
 *  on the page. */
const WORLD_SVG = 'section[aria-label="the IT world map"] svg';

/** The four numbers of the SVG's viewBox, as the browser has them now. */
async function viewBox(page: Page): Promise<readonly number[]> {
  const raw = await page.locator(WORLD_SVG).getAttribute('viewBox');
  return (raw ?? '').split(' ').map(Number);
}

/** The territory's own rect, in the same coordinates. */
async function territoryRect(page: Page, name: string): Promise<Readonly<{ x: number; y: number; w: number; h: number }>> {
  const rect = page.locator(`${WORLD_SVG} .territory[data-region="${name}"] rect.shed`);
  const [x, y, w, h] = await Promise.all([
    rect.getAttribute('x'), rect.getAttribute('y'), rect.getAttribute('width'), rect.getAttribute('height'),
  ]);
  return { x: Number(x), y: Number(y), w: Number(w), h: Number(h) };
}

test('/it/yard/dock paints the dock interior inside the world — the same SVG, zoomed, never a second page', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const svg = page.locator(WORLD_SVG);
  await expect(svg.locator('.territory')).toHaveCount(8);

  // The world at rest, and the node it is drawn in. The handle is the
  // load-bearing part: if the click swapped surfaces, this element is
  // detached afterwards, whatever the new page happens to look like.
  const worldBox = await viewBox(page);
  const dock = await territoryRect(page, 'dock');
  const node = await svg.elementHandle();
  expect(node).not.toBeNull();
  await expect(svg.locator('[data-interior]')).toHaveCount(0);

  await svg.locator('.territory[data-region="dock"]').click();
  await expect(page).toHaveURL(/\/it\/yard\/dock$/);

  // THE INTERIOR: the dock's own wagons, inside the dock's outline.
  const interior = svg.locator('.interior[data-interior="dock"]');
  await expect(interior).toHaveCount(1);
  await expect(interior.locator('.plate')).toHaveCount(2);
  await expect(interior.locator('.plate[data-car="car-1111"]')).toHaveAttribute('data-station', 'dock');
  await expect(interior).toContainText('dock');
  await expect(interior.locator('.plate[data-car="car-1111"] title')).toHaveText(/Teach the dock to breathe/);

  // NEVER LEFT THE MAP: the world is still there, and it is the SAME
  // element — not a re-render of an equivalent one. This is the
  // assertion that earns the spec's title, and it caught the defect it
  // was written for: two `{:else if}` arms in App.svelte for the two
  // routes tore the SVG down on every click.
  await expect(page.locator(WORLD_SVG)).toHaveCount(1);
  expect(await node!.evaluate((el) => el.isConnected)).toBe(true);
  expect(await svg.evaluate((el, prev) => el === prev, node)).toBe(true);

  // THE CAMERA: settled on the dock — the box contains the dock's rect
  // and is a fraction of the world it came from.
  await expect
    .poll(async () => {
      const [, , w] = await viewBox(page);
      return w! < worldBox[2]! / 2;
    }, { message: 'the viewBox never zoomed in on the dock' })
    .toBe(true);
  const [x, y, w, h] = await viewBox(page);
  expect(x!).toBeLessThanOrEqual(dock.x);
  expect(y!).toBeLessThanOrEqual(dock.y);
  expect(x! + w!).toBeGreaterThanOrEqual(dock.x + dock.w);
  expect(y! + h!).toBeGreaterThanOrEqual(dock.y + dock.h);

  // The territories the camera left are still there, faded, not gone:
  // one world, not a page showing one region.
  await expect(svg.locator('.territory')).toHaveCount(8);
  await expect(svg.locator('.territory.away')).toHaveCount(7);
  await expect(svg.locator('.territory.here[data-region="dock"]')).toHaveCount(1);

  // The floor's panels are mounted under the zoomed world — the yard's
  // own deck, on the same page.
  await expect(page.locator('.yard-panel-h', { hasText: 'Entity · loading dock' })).toBeVisible();
  await expect(page.locator('.yard-region-head')).toContainText('dock · clear — 2 cars parked');
});

test('Escape puts the camera back at the world, and so does the frame around the territory', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const svg = page.locator(WORLD_SVG);
  await expect(svg.locator('.territory')).toHaveCount(8);
  const worldBox = await viewBox(page);

  await svg.locator('.territory[data-region="dock"]').click();
  await expect(page).toHaveURL(/\/it\/yard\/dock$/);
  await expect(svg.locator('.interior[data-interior="dock"]')).toHaveCount(1);

  await page.keyboard.press('Escape');
  await expect(page).toHaveURL(/\/it$/);
  await expect
    .poll(async () => (await viewBox(page)).join(' '), { message: 'Escape did not return the camera to the world' })
    .toBe(worldBox.join(' '));
  await expect(svg.locator('[data-interior]')).toHaveCount(0);
  await expect(svg.locator('.territory.away')).toHaveCount(0);

  // The way out is SAID, not only bound to a key: the affordance at the
  // head of the zoomed territory does the same thing.
  await svg.locator('.territory[data-region="dock"]').click();
  await expect(page).toHaveURL(/\/it\/yard\/dock$/);
  await svg.locator('text.leave').click();
  await expect(page).toHaveURL(/\/it$/);
  await expect(svg.locator('[data-interior]')).toHaveCount(0);
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

test('a zoomed marshalling draws a platform per station — its packets, its bound, and a rate it could not count as unknown', async ({ page }) => {
  await mocks(page);
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route(/\/api\/stations\/load$/, (r) => json(r, STATION_LOAD));
  await page.route(/\/api\/stations\/flow/, (r) => json(r, STATION_FLOW));

  await page.goto('/it/yard/marshalling');
  const marshalling = page.locator(`${WORLD_SVG} .territory[data-region="marshalling"]`);
  await expect(marshalling).toHaveClass(/here/);

  const interior = page.locator(`${WORLD_SVG} .interior[data-interior="marshalling"]`);
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

  // The board itself is mounted under the zoomed world: the page that
  // used to live at /it/operate/marshalling, minus its page header.
  await expect(page.locator('.my-root')).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Marshalling Yard' })).toHaveCount(0);
});

test('a zoomed receiving stands its inbound packets by channel and flags what is past the age band', async ({ page }) => {
  await mocks(page);
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route(/\/api\/workflows$/, (r) => json(r, INBOUND_WORKFLOWS));
  await page.route(/\/api\/jobs\?kind=user-feedback/, (r) => json(r, INBOUND_JOBS));

  await page.goto('/it/yard/receiving');
  const receiving = page.locator(`${WORLD_SVG} .territory[data-region="receiving"]`);
  await expect(receiving).toHaveClass(/here/);
  await expect(receiving).not.toContainText("page of its own");

  const interior = page.locator(`${WORLD_SVG} .interior[data-interior="receiving"]`);
  const feedback = interior.locator('.platform[data-platform="feedback"]');
  await expect(feedback).toContainText('1');
  // Opened 2026-08-01: past the 14-day band, so its mark is flagged —
  // the trouble is drawn where the trouble is.
  await expect(feedback.locator('.mark.flagged')).toHaveCount(1);
  await expect(feedback.locator('title')).toHaveText(/past the 14-day band/);
  // A channel nothing stands on is drawn empty, not omitted: "where
  // is the work NOT" is an answer a board still owes.
  await expect(interior.locator('.platform[data-platform="design"] .mark')).toHaveCount(0);

  // And the inbound board is mounted under the world, header dropped.
  await expect(page.locator('.ry-root')).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Receiving Yard' })).toHaveCount(0);
});

test('a queue read that failed is said, never drawn as an empty region', async ({ page }) => {
  await mocks(page);
  await page.route(/\/api\/stations\/load$/, (r) =>
    r.fulfill({ status: 500, contentType: 'application/json', body: '"the station registry is down"' }));

  await page.goto('/it/yard/marshalling');
  const marshalling = page.locator(`${WORLD_SVG} .territory[data-region="marshalling"]`);
  await expect(marshalling).toContainText('cannot be read');
  await expect(page.locator(`${WORLD_SVG} [data-interior]`)).toHaveCount(0);
});

test('a floor lands zoomed on first load — the camera is already there, not flying in from the world', async ({ page }) => {
  await mocks(page);
  await page.goto('/it/yard/gates');
  const svg = page.locator(WORLD_SVG);
  await expect(svg.locator('.territory')).toHaveCount(8);
  const gates = await territoryRect(page, 'gates');
  // Read once, immediately: a load that started at the world and walked
  // in would still be wider than the gates' own rect at this point.
  const [x, y, w, h] = await viewBox(page);
  expect(w!).toBeLessThan(gates.w * 2);
  expect(x!).toBeLessThanOrEqual(gates.x);
  expect(y!).toBeLessThanOrEqual(gates.y);
  expect(x! + w!).toBeGreaterThanOrEqual(gates.x + gates.w);
  expect(y! + h!).toBeGreaterThanOrEqual(gates.y + gates.h);
  await expect(svg.locator('.territory.here[data-region="gates"]')).toHaveCount(1);
});
