// /it — the IT world, every control the default page renders, pinned
// (page audit e18f7540, step `test`).
//
// it-map.mocked.spec.ts pins what the world MEANS: a territory per
// region with its count, state, band and trend; the rails with their
// traffic and machine; a click into a floor; the HUD's four pictures;
// and the regions and borders reads each failing to a `load-failed`
// line. it-map-transit and it-map-motion pin the two flights. Before
// this spec the rest of the default page's controls — the ten doors
// walked one by one and back, the crossings by keyboard, the panel's
// own close button, every machine link, the page's words, an empty and
// a malformed backend, the loading line, a read that fails after a
// good one, the poll — were reached only by the two crawls.
//
// THE INVENTORY, measured on origin/main 507d2308 on 2026-09-25 (the
// measure step read a9028721; since then the plant strip landed with
// car E, and publish's door with 594ffe96):
//   links     10 territory doors (WorldMap `a.territory`), plus one HUD
//                link per failed-or-unjudged machine (`a[data-machine]`);
//                since car N1 of design e765b3fc each SELECTS its region
//                (`/it?at=<name>`), and the selection's panel adds two:
//                `close` and the door to the region's floor page
//   buttons   10 crossing toggles (`g.crossing[role=button]`: click,
//                Enter, Space, Escape), 1 panel `close`
//   forms     0, inputs 0 (the motion bar's are behind it-map-motion)
//   reads     2 — GET /api/yard/regions, GET /api/yard/borders, both
//                every 10 s
//   writes    0
//   no control: the plant strip (pinned in it-region-map), the foot
//                lines, the window line
//
// THIS STEP PINS WHAT THE PAGE DOES TODAY; it does not fix the gaps.
// Where today's behaviour IS an open filed gap, the test says so in its
// name ("CURRENT, gap N (item)") and asserts today's paint, so the car
// that fixes the gap has to flip that assertion. The eight gaps:
//   gap 1 bfab29bf  publish's door opened the TRACK — FIXED on main by
//                   594ffe96 (regions.ts `regionHref`), pinned in
//                   it-map; bfab29bf is its still-open twin. The walk
//                   below lands publish on publish too.
//   gap 2 cb40ba2b  no incident figure on the HUD — CURRENT, below
//   gap 3 336484ae  the human's held steps drawn nowhere — a server
//                   payload (regions.rs shop-floor); nothing on this
//                   page can draw what the server does not send, so it
//                   is pinned in boss_jobs by the car that sends it
//   gap 4 f9850601  one surface, three names — ANSWERED by car N1 of
//                   design e765b3fc: "Department Map", below
//   gap 5 b5b921ce  18 dead style rules in WorldMap.svelte — no
//                   behaviour a browser can see; the deletion is its car
//   gap 6 909adf3b  the 10 s poll has no in-flight guard — CURRENT, below
//   gap 7 cf7ad2f2  8 of the 10 doors land off the catalog — ANSWERED
//                   by car N1 of design e765b3fc: each door selects its
//                   region on /it, below
//   gap 8 8c7c2f4b  the world and the HUD render inside no boundary —
//                   CURRENT, below

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { FAILURE_MARKER } from './_routes';
import { YARD_BORDERS, YARD_REGIONS, installSmokeMocks } from './_smokeMocks';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { BORDERS, TERRITORIES } from '../../src/it/yard/world';

const json = (r: Route, b: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const WORLD = 'section.yard[aria-label="the IT world map"]';
const SVG = `${WORLD} > svg`;
const HUD = '[data-hud]';

const trend = (current: number | null, previous: number | null) => ({
  metric: 'crossings', unit: 'per day', current, previous,
  samples: current === null ? 0 : 3, previous_samples: previous === null ? 0 : 3,
});

/** Every declared region, clear, with a count — and the HUD's block:
 *  three thirds and three listed machines, one in a region the catalog
 *  holds (marshalling) and one in a region it does not (gates). */
const regions = (over: Record<string, unknown> = {}) => ({
  window_hours: 24,
  now: '2026-09-25T06:00:00Z',
  regions: TERRITORIES.map(({ name }) => ({
    name, count: 2, state: 'clear', why: `${name} is quiet`, trend: trend(2, 3),
  })),
  thirds: [
    { third: 'queue-management', regions: ['receiving', 'marshalling'],
      balance: { unit: 'inbound packets', in_means: 'opened', out_means: 'taken', in: 5, out: 4, net: 1, in_count: 5, out_count: 4 },
      stuck: { third: 'queue-management', stuck: 0, waiting: 0, unknown: [], oldest_hours: null, regions: [] } },
    { third: 'actors-building', regions: ['shop-floor', 'gates', 'garage'],
      balance: { unit: 'runs', in_means: 'opened', out_means: 'closed', in: 2, out: 2, net: 0, in_count: 2, out_count: 2 },
      stuck: { third: 'actors-building', stuck: 0, waiting: 0, unknown: [], oldest_hours: null, regions: [] } },
    { third: 'delivery', regions: ['dock', 'track', 'arrivals', 'shed', 'publish'],
      balance: { unit: 'cars', in_means: 'parked', out_means: 'closed', in: 3, out: 3, net: 0, in_count: 3, out_count: 3 },
      stuck: { third: 'delivery', stuck: 0, waiting: 0, unknown: [], oldest_hours: null, regions: [] } },
  ],
  machines: {
    running: 4, idle: 3, failed: 1, unknown: 2, total: 10,
    failed_or_unknown: [
      { region: 'gates', id: 'bay:2', name: 'bay 2', state: 'failed', why: 'its gate-run is past its own deadline' },
      { region: 'marshalling', id: 'station:design-review', name: 'design-review', state: 'unknown', why: 'the flow cube is blind to its predicate' },
      { region: 'receiving', id: 'desk:triage', name: 'triage desk', state: 'unknown', why: 'no triage recorded in the window' },
    ],
  },
  ...over,
});

const borders = (over: Record<string, unknown> = {}) => ({
  window_hours: 24,
  now: '2026-09-25T06:00:00Z',
  borders: BORDERS.map(({ from, to }) => ({
    from, to, crossing: `a packet crossed ${from} to ${to}`, state: 'clear', why: `${from}→${to} is quiet`,
    rate: trend(2, 3), last_crossed: '2026-09-25T05:00:00Z', waiting: 0, holds: [],
    machine: { name: 'the rule', kind: 'dispatcher-rule', last_fired: null, silent_for_minutes: null,
      expected_every_minutes: null, silent: null, why: 'no firing recorded' },
  })),
  ...over,
});

async function mocks(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(YARD_REGIONS, (r) => json(r, regions()));
  await page.route(YARD_BORDERS, (r) => json(r, borders()));
}

/** The paths the catalog holds — the one list (nav-catalog.ts), never a
 *  second one typed here. */
const CATALOGUED = new Set(Object.values(ROUTE_CATALOG).map((e) => e.path));

// ---------------------------------------------------------------------
// THE PAGE'S WORDS
// ---------------------------------------------------------------------

test('gap 4 (f9850601) answered: the page says its words, and is named one way — "Department Map", as its row is', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  await expect(page.locator(`${SVG} .territory`)).toHaveCount(TERRITORIES.length);

  // The header, verbatim. It answered to "Train Yard" (the row), "The IT
  // world" (the title) and "IT · Forge line" (the eyebrow) until car N1
  // of design e765b3fc named the row and the title "Department Map" and
  // the eyebrow "IT", the department it is the map of.
  await expect(page.locator('.exec-eyebrow')).toHaveText('IT');
  await expect(page.locator('h1.exec-title')).toHaveText('Department Map');
  expect(ROUTE_CATALOG['system-yard'].path).toBe('/it');
  expect(ROUTE_CATALOG['system-yard'].label).toBe('Department Map');
  // Car N3 of the same design added where the boards of the retired
  // pages live now, so a reader who knew the old tab finds it.
  await expect(page.locator('.exec-header p')).toHaveText(
    'The territories along the packet flow, and the borders between them carrying what crosses, what waits and the machine that moves it. Select a territory to open its detail below the map. ' +
      'Feedback and the IT backlog are in the Receiving station\'s panel (the backlog in Marshalling\'s too), the Crew Board in the Shop floor\'s, and yard status and the conductor\'s activity in the Track\'s, with the Dock\'s and the Garage\'s lanes in theirs.',
  );

  // The HUD's words: its title, its age, its three rows, its keys.
  const hud = page.locator(HUD);
  await expect(hud.locator('.hud-title')).toHaveText('The whole system');
  await expect(hud.locator('.hud-age')).toHaveText(/^read \d+s ago, window 24h$/);
  await expect(hud.locator('.hud-label')).toHaveText(['Queue management', 'Actors building', 'Delivery']);
  await expect(hud.locator('.hud-row').first().locator('.hud-key')).toHaveText(['balance', 'stuck', 'waiting']);
  await expect(hud.locator('[data-machines] > .hud-key')).toHaveText('machines');
  await expect(hud.locator('.hud-machine-line')).toHaveText(/1\s*failed\s*·\s*2\s*unjudged of\s*10/);

  // The world's own label, and the line under it: the window, and when
  // it was read.
  await expect(page.locator(`${SVG}`)).toHaveAttribute('aria-label', 'the IT world: the territories along the packet flow');
  await expect(page.locator('.yard-flow')).toHaveText(/window 24h against the 24h before · read \d{1,2}:\d\d/);
  // Nothing on the default page is a failure line.
  await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
});

// ---------------------------------------------------------------------
// THE LINKS — ten doors and the HUD's machine links
// ---------------------------------------------------------------------

test('every territory door selects its own region under the map, and back returns to the bare map', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const territories = page.locator(`${SVG} .territory`);
  await expect(territories).toHaveCount(TERRITORIES.length);

  for (const { name } of TERRITORIES) {
    const door = page.locator(`${SVG} .territory[data-region="${name}"]`);
    await expect(door).toHaveAttribute('href', `/it?at=${name}`);
    await door.click();
    await expect(page).toHaveURL(new RegExp(`/it\\?at=${name}$`));
    // Its own region's panel, under the map that stays — publish
    // included (gap 1, fixed by 594ffe96): a door that lands on a
    // neighbour is the wrong target answering.
    await expect(page.locator('section[data-map-panel]')).toHaveAttribute('data-selection', name);
    await expect(territories).toHaveCount(TERRITORIES.length);
    await page.goBack();
    await expect(page).toHaveURL(/\/it$/);
    await expect(page.locator('section[data-map-panel]')).toHaveCount(0);
    await expect(page.locator('h1.exec-title')).toHaveText('Department Map');
  }
});

test('gap 7 (cf7ad2f2) answered: every one of the ten doors lands on the catalogued Department Map, selecting its region', async ({ page }) => {
  // Two of the ten landed on a catalogued route and eight on floor pages
  // no catalog row named, until car N1 of design e765b3fc made each door
  // a selection on the one catalogued page, /it.
  await mocks(page);
  await page.goto('/it');
  // evaluateAll does not wait: read the doors once the map is drawn, or
  // an early read answers "no doors" (measured red once, car N2's run).
  await expect(page.locator(`${SVG} .territory`)).toHaveCount(TERRITORIES.length);
  const hrefs = await page
    .locator(`${SVG} .territory`)
    .evaluateAll((els) => els.map((el) => el.getAttribute('href') ?? ''));
  expect(hrefs).toHaveLength(TERRITORIES.length);
  const paths = hrefs.map((h) => new URL(h, 'http://boss.test').pathname);
  expect(paths.filter((p) => !CATALOGUED.has(p))).toEqual([]);
  expect(hrefs.map((h) => new URL(h, 'http://boss.test').searchParams.get('at')).sort()).toEqual(
    TERRITORIES.map((t) => t.name).sort(),
  );
});

test('every listed machine in the HUD selects its own region on the map, and back returns to the bare map', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const links = page.locator(`${HUD} a[data-machine]`);
  await expect(links).toHaveCount(3);
  // What each link says: failed or unjudged, the region, the machine —
  // and its why on the hover.
  await expect(links).toHaveText([
    'failed · gates · bay 2',
    'unjudged · marshalling · design-review',
    'unjudged · receiving · triage desk',
  ]);
  await expect(page.locator(`${HUD} a[data-machine="bay:2"]`)).toHaveAttribute('title', 'its gate-run is past its own deadline');

  for (const [id, region] of [['bay:2', 'gates'], ['station:design-review', 'marshalling'], ['desk:triage', 'receiving']] as const) {
    const link = page.locator(`${HUD} a[data-machine="${id}"]`);
    await expect(link).toHaveAttribute('href', `/it?at=${region}`);
    await link.click();
    await expect(page).toHaveURL(new RegExp(`/it\\?at=${region}$`));
    await expect(page.locator('section[data-map-panel]')).toHaveAttribute('data-selection', region);
    // The frame stands over the selection: the same links, the map whole.
    await expect(page.locator(`${HUD} a[data-machine]`)).toHaveCount(3);
    await expect(page.locator(`${SVG} .territory`)).toHaveCount(TERRITORIES.length);
    await page.goBack();
    await expect(page).toHaveURL(/\/it$/);
    await expect(page.locator(`${SVG} .territory`)).toHaveCount(TERRITORIES.length);
  }
});

test('a machine block with nothing failed or unjudged lists no link and prints its zeros as zeros', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(YARD_REGIONS, (r) =>
    json(r, regions({ machines: { running: 3, idle: 2, failed: 0, unknown: 0, total: 5, failed_or_unknown: [] } })));
  await page.route(YARD_BORDERS, (r) => json(r, borders()));
  await page.goto('/it');
  const machines = page.locator(`${HUD} [data-machines]`);
  await expect(machines.locator('.hud-machine-line [data-fig]')).toHaveText(['0', '0', '5']);
  await expect(machines.locator('.hud-machine-line [data-fig="zero"]')).toHaveCount(2);
  await expect(machines.locator('a[data-machine]')).toHaveCount(0);
  await expect(machines.locator('ul.hud-listed')).toHaveCount(0);
});

// ---------------------------------------------------------------------
// THE BUTTONS — ten crossings and the panel's close
// ---------------------------------------------------------------------

test('each of the ten crossings opens its own panel under the map', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const crossings = page.locator(`${SVG} .crossing[role="button"]`);
  await expect(crossings).toHaveCount(BORDERS.length);
  for (const { from, to } of BORDERS) {
    const key = `${from}→${to}`;
    const crossing = page.locator(`${SVG} .crossing[data-crossing="${key}"]`);
    // What the button promises a screen reader: the rail, its state
    // and why, and that it opens the crossing.
    const label = (await crossing.getAttribute('aria-label')) ?? '';
    expect(label.startsWith(`${from} → ${to} · clear — ${key} is quiet. `)).toBe(true);
    expect(label.endsWith('. Open the crossing.')).toBe(true);
    await crossing.click();
    const panel = page.locator('.crossing-panel');
    await expect(panel).toHaveCount(1);
    await expect(panel).toHaveAttribute('data-panel', key);
    await expect(panel).toHaveAttribute('aria-label', `the crossing ${key}`);
    await expect(panel.locator('.crossing-title')).toHaveText(`${from} → ${to}`);
    await expect(panel.locator('.crossing-why')).toHaveText(`${key} is quiet`);
    await expect(panel).toContainText(`a packet crossed ${from} to ${to}`);
    await expect(crossing).toHaveAttribute('aria-expanded', 'true');
  }
});

test('a crossing opens on Enter, closes on Space, and Escape closes it', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const key = 'dock→track';
  const crossing = page.locator(`${SVG} .crossing[data-crossing="${key}"]`);
  const panel = page.locator(`.crossing-panel[data-panel="${key}"]`);
  await expect(crossing).toHaveAttribute('tabindex', '0');
  await expect(crossing).toHaveAttribute('aria-expanded', 'false');

  await crossing.focus();
  await page.keyboard.press('Enter');
  await expect(panel).toBeVisible();
  await expect(crossing).toHaveAttribute('aria-expanded', 'true');

  // Space toggles it shut, and does not scroll the page under the reader.
  const scrolled = await page.evaluate(() => window.scrollY);
  await page.keyboard.press(' ');
  await expect(page.locator('.crossing-panel')).toHaveCount(0);
  await expect(crossing).toHaveAttribute('aria-expanded', 'false');
  expect(await page.evaluate(() => window.scrollY)).toBe(scrolled);

  await page.keyboard.press(' ');
  await expect(panel).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.locator('.crossing-panel')).toHaveCount(0);
  await expect(crossing).toHaveAttribute('aria-expanded', 'false');

  // Escape on a closed crossing leaves it closed rather than opening it.
  await page.keyboard.press('Escape');
  await expect(page.locator('.crossing-panel')).toHaveCount(0);
});

test('the panel\'s close button says what it closes and closes it', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const key = 'gates→dock';
  const crossing = page.locator(`${SVG} .crossing[data-crossing="${key}"]`);
  await crossing.click();
  const panel = page.locator(`.crossing-panel[data-panel="${key}"]`);
  await expect(panel).toBeVisible();
  const close = panel.locator('button.crossing-close');
  await expect(close).toHaveText('close');
  await expect(close).toHaveAttribute('aria-label', 'close the crossing');
  await close.click();
  await expect(page.locator('.crossing-panel')).toHaveCount(0);
  await expect(crossing).toHaveAttribute('aria-expanded', 'false');
  // And the next click opens it again — the close left no state behind.
  await crossing.click();
  await expect(panel).toBeVisible();
});

// ---------------------------------------------------------------------
// THE READS — loading, empty, malformed, failed after a good one, the poll
// ---------------------------------------------------------------------

test('while the regions are being read the page says so, and the HUD says it is reading', async ({ page }) => {
  await installSmokeMocks(page);
  let release: () => void = () => {};
  const held = new Promise<void>((r) => (release = r));
  await page.route(YARD_REGIONS, async (r) => {
    await held;
    await json(r, regions()).catch(() => {});
  });
  await page.route(YARD_BORDERS, (r) => json(r, borders()));
  await page.goto('/it');
  await expect(page.locator('.yard-empty')).toHaveText('Reading the regions…');
  await expect(page.locator(`${HUD} .hud-age`)).toHaveText('reading…');
  await expect(page.locator(`${HUD} [data-fig="unread"]`).first()).toBeVisible();
  await expect(page.locator(`${SVG}`)).toHaveCount(0);
  await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  release();
  await expect(page.locator(`${SVG} .territory`)).toHaveCount(TERRITORIES.length);
  await expect(page.getByText('Reading the regions…')).toHaveCount(0);
});

test('an empty backend paints every region at zero and every rail with nothing waiting — no failure line, and no unread figure drawn as a zero', async ({ page }) => {
  // The suite's empty leg (_smokeMocks): one clear region per territory
  // with a count of 0 and no samples, one quiet rail per hop, and no
  // thirds, machine block or plant — the shape an older server sends.
  await installSmokeMocks(page);
  await page.goto('/it');
  const territories = page.locator(`${SVG} .territory`);
  await expect(territories).toHaveCount(TERRITORIES.length);
  await expect(page.locator(`${SVG} .territory[data-state="clear"]`)).toHaveCount(TERRITORIES.length);
  await expect(page.locator(`${SVG} .territory[data-region="dock"] text.count`)).toHaveText('0');
  await expect(page.locator(`${SVG} .crossing[data-waiting="0"]`)).toHaveCount(BORDERS.length);
  await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  // No plant sent is no strip — not an estate with nothing running.
  await expect(page.locator('section[aria-label="the plant — machinery serving every region"]')).toHaveCount(0);
  // The HUD's read landed, but the server sent no thirds and no machine
  // count: every figure is the unread `?`, never a 0, and says why.
  const hud = page.locator(HUD);
  await expect(hud).toHaveAttribute('data-read', 'ok');
  await expect(hud.locator('.hud-row')).toHaveCount(3);
  await expect(hud.locator('[data-fig="value"], [data-fig="zero"], [data-fig="floor"]')).toHaveCount(0);
  await expect(hud.locator('.hud-row [data-fig="unread"]').first()).toHaveAttribute('title', 'the server sends no thirds block');
  await expect(hud.locator('[data-machines] [data-fig="unread"]').first()).toHaveAttribute('title', 'the server sends no machine count');
});

test('a server that answers no regions and no borders draws every territory and rail unread — never a clear world', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(YARD_REGIONS, (r) => json(r, regions({ regions: [] })));
  await page.route(YARD_BORDERS, (r) => json(r, borders({ borders: [] })));
  await page.goto('/it');
  await expect(page.locator(`${SVG} .territory`)).toHaveCount(TERRITORIES.length);
  // An answered-but-absent region is troubled and says so; it is never
  // drawn as the clear one an empty payload would otherwise suggest.
  await expect(page.locator(`${SVG} .territory[data-state="troubled"]`)).toHaveCount(TERRITORIES.length);
  const dock = page.locator(`${SVG} .territory[data-region="dock"]`);
  await expect(dock.locator('text.count')).toHaveText('no reading');
  await expect(dock.locator('title')).toHaveText('dock · troubled — the server answered no reading for this region');
  await expect(page.locator(`${SVG} .crossing[data-waiting="unknown"]`)).toHaveCount(BORDERS.length);
  await page.locator(`${SVG} .crossing[data-crossing="gates→dock"]`).click();
  await expect(page.locator('.crossing-panel .crossing-why')).toHaveText('the borders read answered nothing for this rail');
});

test('a regions answer in the wrong shape is a failed read, said in the failure line — never an empty map', async ({ page }) => {
  // `[]` is what a wrong endpoint answers (the suite's catch-all):
  // parseRegions throws, and the throw is the failure line.
  await installSmokeMocks(page);
  await page.route(YARD_REGIONS, (r) => json(r, []));
  await page.route(YARD_BORDERS, (r) => json(r, borders()));
  await page.goto('/it');
  await expect(page.locator(`.yard-empty${FAILURE_MARKER}`)).toHaveText(
    'The regions cannot be read — yard regions: expected an object',
  );
  await expect(page.locator(`${SVG}`)).toHaveCount(0);
  await expect(page.locator(HUD)).toHaveAttribute('data-read', 'failed');
});

test('a borders answer in the wrong shape is said, and the territories still paint', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(YARD_REGIONS, (r) => json(r, regions()));
  await page.route(YARD_BORDERS, (r) => json(r, []));
  await page.goto('/it');
  await expect(page.locator(`.yard-empty${FAILURE_MARKER}`)).toContainText('The borders cannot be read — ');
  await expect(page.locator(`${SVG} .territory`)).toHaveCount(TERRITORIES.length);
  await expect(page.locator(`${SVG} .crossing[data-waiting="unknown"]`)).toHaveCount(BORDERS.length);
});

test('a regions read that fails after a good one keeps none of its values, and the HUD names both times', async ({ page }) => {
  await page.clock.install({ time: new Date('2026-09-25T06:00:00Z') });
  await installSmokeMocks(page);
  // The first regions read answers good; every later one is HELD until
  // the test fails it, so the failure lands where the test says and
  // nowhere else (backlog 3027f808). An installed clock still runs on
  // real time, so on a starved runner the page's own 10 s poll fired
  // before the good read was asserted and the map was already gone: ten
  // real seconds held after goto failed it every time (.territory,
  // expected 10, received 0).
  let n = 0;
  const held: Route[] = [];
  await page.route(YARD_REGIONS, (r) => (++n === 1 ? json(r, regions()) : void held.push(r)));
  await page.route(YARD_BORDERS, (r) => json(r, borders()));
  await page.goto('/it');
  await expect(page.locator(`${SVG} .territory`)).toHaveCount(TERRITORIES.length);
  await expect(page.locator(`${HUD} [data-fig="value"]`).first()).toBeVisible();

  await page.clock.runFor(10_000);
  await expect.poll(() => held.length).toBeGreaterThan(0);
  await Promise.all(held.splice(0).map((r) => json(r, 'the backend is down', 500)));
  await expect(page.locator(`.yard-empty${FAILURE_MARKER}`)).toContainText('The regions cannot be read — /api/yard/regions: HTTP 500');
  await expect(page.locator(`${SVG}`)).toHaveCount(0);
  const hud = page.locator(HUD);
  await expect(hud).toHaveAttribute('data-read', 'failed');
  await expect(hud.locator('.hud-age')).toHaveText(/^read failed \d\d:\d\dZ · last good \d\d:\d\dZ$/);
  // The frame keeps the last good read's SHAPE — its three thirds by
  // name — and none of its values.
  await expect(hud.locator('.hud-label')).toHaveText(['Queue management', 'Actors building', 'Delivery']);
  await expect(hud.locator('[data-fig="value"], [data-fig="zero"], [data-fig="floor"]')).toHaveCount(0);
  await expect(hud.locator('a[data-machine]')).toHaveCount(0);
});

test('CURRENT, gap 6 (909adf3b): the 10 s poll fires again while the last read is still in flight', async ({ page }) => {
  await page.clock.install({ time: new Date('2026-09-25T06:00:00Z') });
  await installSmokeMocks(page);
  const pending: Route[] = [];
  await page.route(YARD_REGIONS, (r) => {
    pending.push(r);
  });
  await page.route(YARD_BORDERS, (r) => json(r, borders()));
  await page.goto('/it');
  // Counted from what goto left, never from zero: an installed clock
  // still runs on real time, so on a starved runner the page's own 10 s
  // poll had already sent a second read before the count was taken (ten
  // real seconds held after goto: expected 1, received 2), and the
  // "exactly one more" below then saw three (backlog 3027f808).
  await expect.poll(() => pending.length).toBeGreaterThan(0);
  const before = pending.length;
  await expect(page.locator('.yard-empty')).toHaveText('Reading the regions…');
  // Ten seconds on, the first answer has still not come: today another
  // read goes out beside it, and whichever lands LAST is drawn — an
  // older answer can overwrite a newer one. Gap 6 skips a tick while
  // one is in flight; its car flips this to "stays at before".
  await page.clock.runFor(10_000);
  await expect.poll(() => pending.length).toBeGreaterThan(before);
  for (const r of pending) await json(r, regions()).catch(() => {});
});

// ---------------------------------------------------------------------
// WHAT THE PAGE PRINTS WITHOUT A CONTROL — and what it does not
// ---------------------------------------------------------------------

test('a region or a border the layout does not know is named at the foot of the map, never dropped', async ({ page }) => {
  await installSmokeMocks(page);
  const base = regions();
  const rails = borders();
  await page.route(YARD_REGIONS, (r) =>
    json(r, { ...base, regions: [...base.regions, { name: 'moon', count: 1, state: 'clear', why: 'a newer server', trend: trend(1, 1) }] }));
  await page.route(YARD_BORDERS, (r) =>
    json(r, { ...rails, borders: [...rails.borders, { ...rails.borders[0], from: 'shed', to: 'moon' }] }));
  await page.goto('/it');
  await expect(page.locator(`${SVG} .territory`)).toHaveCount(TERRITORIES.length);
  await expect(page.locator(`${SVG} text`, { hasText: 'not on the map:' })).toHaveText('not on the map: moon');
  await expect(page.locator(`${SVG} text`, { hasText: 'no rail drawn for:' })).toHaveText('no rail drawn for: shed→moon');
});

test('CURRENT, gap 2 (cb40ba2b): the HUD carries no incident figure and no door to /it/operate', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const hud = page.locator(HUD);
  await expect(hud.locator('.hud-row')).toHaveCount(3);
  // Gap 2 adds an incidents figure — open count, oldest age, troubled
  // when any is urgent, linking /it/operate — from the regions read;
  // its car flips both lines.
  await expect(hud.locator('[data-cell="incidents"]')).toHaveCount(0);
  await expect(hud.locator('a[href="/it/operate"]')).toHaveCount(0);
});

test('CURRENT, gap 8 (8c7c2f4b): a world that cannot be drawn leaves no failure line', async ({ page }) => {
  // The shape that blanked the shop floor on 2026-09-24 (846ab934): two
  // machines with ONE id in one territory, which WorldMap keys its
  // machinery by. RegionMap has a boundary that names the crash; the
  // world does not. Gap 8 wraps WorldMap and HudFrame in the same
  // boundary, and its car flips this to one `load-failed` line naming
  // the error.
  await installSmokeMocks(page);
  const base = regions();
  const twin = { id: 'bay:1', name: 'bay 1', state: 'running', why: 'a gate is running' };
  await page.route(YARD_REGIONS, (r) =>
    json(r, {
      ...base,
      regions: base.regions.map((g) => (g.name === 'gates' ? { ...g, machines: [twin, { ...twin }] } : g)),
    }));
  await page.route(YARD_BORDERS, (r) => json(r, borders()));
  const errors: string[] = [];
  page.on('pageerror', (e) => errors.push(e.message));
  await page.goto('/it');
  await expect.poll(() => errors.join('\n')).toMatch(/each_key_duplicate|duplicate key/i);
  await expect(page.locator(`.territory[data-region="gates"] [data-machine="bay:1"]`)).toHaveCount(0);
  await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
});

// ---------------------------------------------------------------------
// THE WRITES — none
// ---------------------------------------------------------------------

/// The one non-GET every route sends: the shell's route-open record
/// (shell/surface-opens.ts). It is the chrome's write, not this page's.
const SHELL_WRITES: ReadonlySet<string> = new Set(['/api/surface-opens']);

test('the page writes nothing: every control on it is a read or a view change', async ({ page }) => {
  const writes: string[] = [];
  page.on('request', (req: Request) => {
    const path = new URL(req.url()).pathname;
    if (req.method() !== 'GET' && path.startsWith('/api/') && !SHELL_WRITES.has(path)) {
      writes.push(`${req.method()} ${path}`);
    }
  });
  await mocks(page);
  await page.goto('/it');
  await expect(page.locator(`${SVG} .territory`)).toHaveCount(TERRITORIES.length);
  const crossing = page.locator(`${SVG} .crossing[data-crossing="dock→track"]`);
  await crossing.click();
  await page.locator('.crossing-panel button.crossing-close').click();
  await crossing.focus();
  await page.keyboard.press('Enter');
  await page.keyboard.press('Escape');
  await page.locator(`${SVG} .territory[data-region="shed"]`).click();
  await expect(page).toHaveURL(/\/it\?at=shed$/);
  await page.locator('section[data-map-panel] a[data-close]').click();
  await expect(page).toHaveURL(/\/it$/);
  await page.locator(`${SVG} .territory[data-region="shed"]`).click();
  await expect(page).toHaveURL(/\/it\?at=shed$/);
  await page.goBack();
  await expect(page).toHaveURL(/\/it$/);
  await expect(page.locator(`${SVG} .territory`)).toHaveCount(TERRITORIES.length);
  expect(writes).toEqual([]);
});
