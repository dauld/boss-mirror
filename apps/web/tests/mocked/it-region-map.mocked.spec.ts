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
import { YARD_BORDERS, YARD_REGIONS, installSmokeMocks } from './_smokeMocks';

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

  // THE HEAD AGAINST THE DRAWING (design 62de32ae, decision 5): the
  // stations stand 32 while the head counts none of them as
  // marshalling's, and the map SAYS so rather than contradicting itself.
  await expect(page.locator('[data-drawn="marshalling"]')).toContainText(
    'the platforms stand 32 — the head counts 0: a packet stands at every station it matches',
  );

  // The board itself is mounted under the region map: the page that
  // used to live at /it/operate/marshalling, minus its page header.
  await expect(page.locator('.my-root')).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Marshalling Yard' })).toHaveCount(0);
});

// EACH STATION AT THE PARTITION (design 62de32ae, the rest of decision
// 5; car E on c3105b2a). Live on 2026-09-24 the platforms stood 562
// under a head of 236, each station at its full depth. The server now
// says how many of MARSHALLING'S members stand at each station, and the
// platform stands that — its depth kept in its note.
test("marshalling's platforms stand the server's partition per station, not each station's depth", async ({ page }) => {
  await mocks(page);
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route(/\/api\/stations\/load$/, (r) => json(r, STATION_LOAD));
  await page.route(/\/api\/stations\/flow/, (r) => json(r, STATION_FLOW));
  await page.route(YARD_REGIONS, (r) =>
    json(r, {
      ...REGIONS,
      regions: REGIONS.regions.map((x) =>
        x.name === 'marshalling'
          ? { ...x, count: 6, unit: 'packets at stations', places: [
              { name: 'q.platform-admin.task', count: 5 },
              { name: 'my-watchlist', count: 2 },
            ] }
          : x,
      ),
    }));

  await page.goto('/it/yard/marshalling');
  const interior = page.locator(`${regionSvg('marshalling')} .interior[data-interior="marshalling"]`);
  const task = interior.locator('.platform[data-platform="q.platform-admin.task"]');
  await expect(task).toContainText('5 / 24');
  await expect(task.locator('title')).toHaveText(/5 of the 30 here are marshalling’s/);
  // Five against a bound of 24 while the station holds 30: the flag
  // never counts more packets than the platform stands.
  await expect(task.locator('.mark')).toHaveCount(5);
  // 5 + 2 over a head of 6: the one packet at both stations is SAID.
  await expect(page.locator('[data-drawn="marshalling"]')).toHaveText(
    'the platforms stand 7 — the head counts 6 packets at stations: the 1 more are packets standing at more than one station, drawn at each and counted once',
  );
});

// THE PLANT (decision 11): the host runners stand along the world's
// edge with their names and states written, not as 10px squares in
// receiving.
test('the world draws its plant along the edge, each machine with its name and state', async ({ page }) => {
  await mocks(page);
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
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
  // The failed one first, with its state in words, and blinking red.
  const glyphs = plant.locator('.glyph');
  await expect(glyphs).toHaveCount(2);
  await expect(glyphs.first()).toHaveAttribute('data-machine', 'runner:host:boss-gcp');
  await expect(glyphs.first()).toContainText('boss-gcp runner · failed');
  await expect(glyphs.first().locator('title')).toHaveText(/the last converge request was refused/);
  await expect(plant.locator('[data-machine="runner:host:forge"]')).toContainText('forge runner · idle');
  // No territory carries a host runner.
  await expect(page.locator(`${WORLD_SVG} [data-machine^="runner:host"]`)).toHaveCount(0);
});

// Stations OVERLAP: on 2026-09-23 the sidings summed to 517 over 303
// distinct packets — all 213 agent-station packets also stood in
// q.platform-admin.task — and nothing on the board said so (backlog
// 140a2222). The board states it, from the server's own counts.
test('the board says its sidings overlap, so the depth column is not read as a count of the work', async ({ page }) => {
  await mocks(page);
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
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
  await page.route(/\/api\/stations\/flow/, (r) => json(r, STATION_FLOW));

  await page.goto('/it/yard/marshalling');
  const overlap = page.locator('.my-root .my-overlap');
  await expect(overlap).toContainText('The depths sum to 517 but hold 303 distinct packets');
  await expect(overlap).toContainText('a.platform-admin.opus-5-1m 213 of 213');
});

// The waits table cut to 12 of 320 real obligations on 2026-09-23 and
// said neither number (backlog 18683a0a, page-audit 7c228914 gap 8).
// Fourteen real waits and one simulated: twelve rows, and a line under
// them that counts the two it cut and the one it would not rank.
test('the waits table counts what its cap leaves out, so a limit does not read as a filter', async ({ page }) => {
  await mocks(page);
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route(/\/api\/stations\/load$/, (r) => json(r, STATION_LOAD));
  await page.route(/\/api\/stations\/flow/, (r) => json(r, STATION_FLOW));
  const wait = (i: number, simulated: boolean) => ({
    job_id: `wait-${i}`, job_kind: 'backlog-item', job_title: `packet ${i}`, step_title: 'Build the change',
    status: 'ready', assignee_id: null, simulated, partition: simulated ? 'simulated' : 'real',
    exact: true, waiting_days: 20 - i,
  });
  await page.route(/\/api\/jobs\/queue-age$/, (r) =>
    json(r, {
      data: [...Array.from({ length: 14 }, (_, i) => wait(i, false)), wait(99, true)],
      total: 15,
      now: '2026-09-19T05:00:00Z',
    }));

  await page.goto('/it/yard/marshalling');
  const count = page.locator('.my-root .my-waits-count');
  await expect(count).toHaveText(
    '12 of 14 outstanding obligations, longest first · +2 more not shown · 1 on simulated or shadow packets not ranked',
  );
  await expect(page.locator('.my-root a', { hasText: /^packet \d+$/ })).toHaveCount(12);
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

// ---------------------------------------------------------------------
// THE TWO REGION VIEWS THAT NEVER DREW ON LIVE DATA (backlog 846ab934,
// the IT map review of 2026-09-24, findings 1 and 9).
// ---------------------------------------------------------------------

/** The live shape of the shop floor, measured 2026-09-24 (the review's
 *  world-titles.txt, lines 25–29): every open session is the SAME
 *  actor — one agent identity, many sessions. Two here is enough to
 *  collide. The platforms were keyed by actor, so the region map threw
 *  each_key_duplicate and the page sat on "Reading the regions…"; a
 *  fixture with a different actor per session could never catch it. */
const ONE_ACTOR_SESSIONS = {
  total: 2,
  data: [
    { id: '87b3cf48-f17b-4126-98b4-7673a3c3576b', kind: 'work-session', title: 'a session', status: 'open',
      metadata: { actor: 'claude@algedonic.dev', host: 'boss-dev', started_at: '2026-09-24T03:00:00Z',
        last_active_at: '2026-09-24T04:40:00Z', prompt_count: 40 } },
    { id: '7bb6e37d-d168-40b8-9675-70a42229a2bb', kind: 'work-session', title: 'another session', status: 'open',
      metadata: { actor: 'claude@algedonic.dev', host: 'boss-dev', started_at: '2026-09-24T04:00:00Z',
        last_active_at: '2026-09-24T04:45:00Z', prompt_count: 7 } },
  ],
};

/** One builder run on the first session, building an item. Built when
 * the read is answered, not when this file loads, so its age is ten
 * minutes however long the worker has been running (backlog de205627). */
const oneRun = () => ({
  total: 1,
  data: [
    { id: 'fa4014f2-1794-4268-9881-f4e616f7d0ad', kind: 'agent-run', status: 'open',
      title: 'builder run: the map should feel like a world',
      metadata: { packet: 'c3105b2a-001d-4ae4-a172-5b7f8aba2598', step: 'build', agent: 'claude@algedonic.dev',
        session: '87b3cf48-f17b-4126-98b4-7673a3c3576b',
        // Ten minutes before the page reads it: the board ages a run on
        // the wall clock, and a fixed stamp would age past the 4h bound.
        opened_at: new Date(Date.now() - 10 * 60_000).toISOString() },
      steps: [{ spec_slug: 'building', title: 'Building', status: 'active' }] },
  ],
});

// THE SHOP FLOOR AS ACTORS (design 62de32ae, decision 8; car E on
// c3105b2a): one lamp per session and per run, labelled and aged, under
// the identity they share. The live shape is the fixture: two sessions
// of ONE actor, which once crashed the region (846ab934).
test('the shop floor draws its actors — a lamp per session and per run, under the identity they share', async ({ page }) => {
  await mocks(page);
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route(/\/api\/jobs\?kind=work-session/, (r) => json(r, ONE_ACTOR_SESSIONS));
  await page.route(/\/api\/jobs\?kind=agent-run/, (r) => json(r, oneRun()));

  await page.goto('/it/yard/shop-floor');
  const map = page.locator(regionMap('shop-floor'));
  await expect(map).toHaveCount(1);
  await expect(page.getByText('Reading the regions…')).toHaveCount(0);

  const interior = page.locator(`${regionSvg('shop-floor')} .interior[data-interior="shop-floor"]`);
  // ONE identity heads the group, not one crew per session.
  await expect(interior.locator('.actor')).toHaveCount(1);
  await expect(interior.locator('.actor[data-actor="claude@algedonic.dev"]')).toContainText('2 sessions · 1 run');
  // A lamp per session, keyed by the session — the key the server's
  // machines already use — and told apart on the picture by its id.
  await expect(interior.locator('.lamp-row.session')).toHaveCount(2);
  const busy = interior.locator('[data-lamp-row="session:87b3cf48-f17b-4126-98b4-7673a3c3576b"]');
  await expect(busy).toContainText('session 87b3cf48 · boss-dev');
  await expect(interior.locator('[data-lamp-row="session:7bb6e37d-d168-40b8-9675-70a42229a2bb"]')).toContainText(
    'session 7bb6e37d',
  );
  // The run: its own id, the item it builds, and its age.
  const run = interior.locator('[data-lamp-row="run:fa4014f2-1794-4268-9881-f4e616f7d0ad"]');
  await expect(run).toHaveCount(1);
  await expect(run).toContainText('run fa4014f2 → c3105b2a build');
  await expect(run).toHaveAttribute('data-lamp', 'at-work');
  await expect(run.locator('text.age')).toHaveText(/^at work \d/);
  // The platforms and the machinery strip gave way to the lamps.
  await expect(interior.locator('.platform')).toHaveCount(0);
  await expect(page.locator(`${regionSvg('shop-floor')} .machinery`)).toHaveCount(0);
  await expect(page.locator('.load-failed')).toHaveCount(0);
});

test('publish, which has no floor yet, SAYS so rather than reading forever', async ({ page }) => {
  await mocks(page);
  await page.goto('/it/yard/publish');
  const map = page.locator(regionMap('publish'));
  await expect(map).toHaveCount(1);
  await expect(map.locator('.region-why')).toContainText('no pull request awaiting a merge');
  // Publish is neither a floor region nor a platform region, so no read
  // will ever land for it: a loading line there is a lie that never
  // resolves, and reads as an outage.
  await expect(map).toContainText('no floor drawn for this region yet');
  await expect(map).not.toContainText('reading what is inside');
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

// ---------------------------------------------------------------------
// A REGION OWNS ITS PAGE (design 62de32ae, decision 7).
// ---------------------------------------------------------------------
//
// The review of 2026-09-24 (finding 8): the view swapped and the page
// around it did not. Every region's heading read "The IT world", the
// world's summary sat between the region map and its floor, the
// garage's departure board listed dock cars, and "No alerts — every
// machine is working or idle by design" stood under a troubled shed.

const dockRail = (from: string, to: string, over: Record<string, unknown> = {}) => ({
  from, to, crossing: 'a packet crossed', state: 'clear', why: 'nothing waiting',
  rate: { metric: 'crossings', unit: 'per day', current: 5, previous: 4, samples: 5, previous_samples: 4 },
  last_crossed: null, waiting: 0, holds: [],
  machine: { name: 'auto-park-on-gate-green', kind: 'dispatcher-rule', last_fired: '2026-09-19T04:53:00Z',
    silent_for_minutes: 7, expected_every_minutes: null, silent: false, why: '' },
  ...over,
});

test('a region page is headed by the region, with the way back and its own rails in place of the world summary', async ({ page }) => {
  await mocks(page);
  await page.route(YARD_BORDERS, (r) =>
    r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        window_hours: 24,
        now: '2026-09-19T05:00:00Z',
        borders: [
          dockRail('gates', 'dock', { waiting: 6 }),
          dockRail('dock', 'track', { state: 'troubled', why: 'the boarding rule is silent' }),
          dockRail('track', 'arrivals'),
        ],
      }),
    }));
  await page.goto('/it/yard/dock');

  await expect(page.getByRole('heading', { level: 1 })).toHaveText('IT · Dock');
  await expect(page.getByRole('heading', { name: 'The IT world' })).toHaveCount(0);
  const crumbs = page.locator('nav.crumbs');
  await expect(crumbs).toContainText('The IT world');
  await expect(crumbs.locator('[aria-current="page"]')).toContainText('Dock');

  // The dock's rails, in then out — not the world's crossings total.
  const rails = page.locator('.region-rails[data-rails="dock"] .rail-line');
  await expect(rails).toHaveCount(2);
  await expect(rails.nth(0)).toHaveText('in from gates · 5/day · 6 waiting · auto-park-on-gate-green · fired 7m ago');
  await expect(rails.nth(1)).toContainText('out to track · 5/day · nothing waiting');
  await expect(rails.nth(1)).toContainText('troubled — the boarding rule is silent');
  await expect(page.getByText(/crossings in 24h/)).toHaveCount(0);

  // The departure board is the dock's, titled for it.
  const board = page.locator('.head[data-board="dock"]');
  await expect(board).toContainText('Departure board · dock');
  await expect(board).toContainText('2 in flight · 0 landed');

  // The breadcrumb is the way back, as a link.
  await crumbs.getByRole('link', { name: 'The IT world' }).click();
  await expect(page).toHaveURL(/\/it$/);
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('The IT world');
});

test("a quiet alerts strip under a troubled region does not call the region's machines fine", async ({ page }) => {
  await mocks(page);
  const troubled = {
    ...REGIONS,
    regions: REGIONS.regions.map((r) =>
      r.name === 'shed' ? { ...r, state: 'troubled', why: '2 landed cars of ours owe a proof' } : r),
  };
  await page.route(YARD_REGIONS, (r) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(troubled) }));
  await page.goto('/it/yard/shed');

  const strip = page.locator('.yard-alerts[data-alerts="shed"]');
  await expect(strip).toContainText("No machine alerts in the shed — its troubled state above is the region's own reading");
  await expect(strip).not.toContainText('every machine here is working');
});
