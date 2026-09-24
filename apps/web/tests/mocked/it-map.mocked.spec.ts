// THE IT WORLD MAP — design d2154293, car 1 (over 0524fc95's regions
// read, GET /api/yard/regions). /it is ONE SVG world: the territories
// laid out along the packet flow (world.ts), each drawn
// in the yard's strokes with the region's count, state and trend
// inside its outline, the why on a troubled one, and a door to its
// floor. This pins what the packet names: the world paints one
// territory per declared region from a fixture payload, a troubled one
// carries its why where it is, a click opens the floor — and the
// floor's panel opens with the region's own state and why at its head
// (a106309c), from the same read, so clicking in cannot contradict the
// map.
//
// THE ARITY IS READ FROM THE LAYOUT, NEVER TYPED HERE (backlog
// 94c6ffd0). These counts were the literals 8 and 8 until the shop
// floor became the ninth region, and then five specs failed for
// arithmetic rather than for anything they were built to protect. The
// layout declares how many territories and rails there are, world.ts
// is pinned to the server's own list by world.test.ts, and this reads
// it — so a tenth region is a fixture row here and nothing else.

import { expect, test, type Page, type Route } from '@playwright/test';
import { BORDERS, TERRITORIES } from '../../src/it/yard/world';
import { YARD_BORDERS, YARD_REGIONS, installSmokeMocks } from './_smokeMocks';

const trend = (metric: string, unit: string, current: number | null, previous: number | null) => ({
  metric, unit, current, previous, samples: current === null ? 0 : 5, previous_samples: previous === null ? 0 : 4,
});

/** The wire shape the server serialises, in map order, with the gates
 *  troubled — a bay holding a corpse — and marshalling unread. */
const REGIONS = {
  window_hours: 24,
  now: '2026-09-19T05:00:00Z',
  regions: [
    { name: 'dock', count: 3, bound: 5, state: 'clear', why: '3 cars parked', trend: trend('dock wait', 'hours', 4.25, 3) },
    { name: 'gates', count: 1, bound: 3, state: 'troubled', why: '1 bay holds a corpse — a gate-run past its own deadline', trend: trend('gate duration', 'minutes', 11, 9.5) },
    { name: 'track', count: 0, bound: 1, state: 'clear', why: 'no train in transit', trend: trend('time at CI', 'minutes', null, 14) },
    { name: 'shed', count: 2, state: 'attention', why: '2 landed cars await their probe', trend: trend('time to proven', 'hours', 1, 1.5) },
    { name: 'arrivals', count: 17, state: 'clear', why: '17 trains arrived in the window', trend: trend('arrivals', 'per day', 17, 12) },
    { name: 'garage', count: 0, state: 'clear', why: 'nothing gated red', trend: trend('reds', 'per day', 0, 2) },
    // Design 62de32ae car A: a non-clear state names the declared band
    // that decided it and how long the record says it has held, and
    // the count and the KPI carry their units.
    {
      name: 'receiving', count: 4, unit: 'packets standing', state: 'attention', why: '4 inbound, oldest 5 days',
      band: { id: 'receiving-aging', reads: 'oldest 5d > the 3-day triage band', hold_minutes: 0, since: '2026-09-18T00:00:00+00:00', held_minutes: 2160, held: '36h' },
      kpi: [{ name: 'oldest untriaged', value: 5, unit: 'days', text: 'oldest untriaged 5 days' }],
      trend: trend('inbound', 'per day', 4, 6),
    },
    { name: 'marshalling', count: null, state: 'troubled', why: 'the station registry could not be read', trend: trend('served', 'per day', null, null) },
    { name: 'shop-floor', count: 2, bound: 6, state: 'clear', why: '2 runs in flight, 1 crew on the floor', trend: trend('build duration', 'minutes', 64, 58) },
    { name: 'publish', count: 0, state: 'clear', why: 'no pull request awaiting a merge', trend: trend('publishes', 'per day', 1, 1) },
  ],
  // THE HUD'S BLOCK (design 00774ca8): the server's rows — one reading a
  // floor, one whose edges were unread, one balanced at a true zero with
  // a troubled stuck count — and its machine cell.
  thirds: [
    {
      third: 'queue-management', regions: ['receiving', 'marshalling'],
      balance: { unit: 'inbound packets', in_means: 'an inbound packet opened', out_means: 'an inbound packet taken off the queue',
        in: 40, out: 28, net: 12, in_count: 40, out_count: 28 },
      stuck: { third: 'queue-management', stuck: 3, waiting: 0, oldest_hours: 170, regions: ['receiving', 'marshalling'],
        unknown: ['station q.platform-admin.task: 305 standing — the flow cube is blind to its predicate'] },
    },
    {
      third: 'actors-building', regions: ['shop-floor', 'gates', 'garage'],
      balance: { unit: 'runs', in_means: 'a run opened', out_means: 'a run closed',
        in: null, out: null, net: null, in_count: null, out_count: null, why: 'the agent-run packets could not be read' },
      stuck: { third: 'actors-building', stuck: 0, waiting: 0, unknown: [], oldest_hours: null, regions: [] },
    },
    {
      third: 'delivery', regions: ['dock', 'track', 'arrivals', 'shed', 'publish'],
      balance: { unit: 'cars', in_means: 'a green gate parked as a car', out_means: 'a car closed',
        in: 9, out: 9, net: 0, in_count: 9, out_count: 9 },
      stuck: { third: 'delivery', stuck: 1, waiting: 2, unknown: [], oldest_hours: 30, regions: ['shed'] },
    },
  ],
  machines: {
    running: 12, idle: 11, failed: 0, unknown: 2, total: 25,
    failed_or_unknown: [
      { region: 'marshalling', id: 'station:design-review', name: 'design-review', state: 'unknown', why: 'the flow cube is blind to its predicate' },
      { region: 'receiving', id: 'runner:forge', name: 'forge runner', state: 'unknown', why: 'no ops-request answered in the window' },
    ],
  },
};

/** The rails (design d2154293, car 2): one busy with a machine silent
 *  past its own cadence, one the server could not read at all, the rest
 *  quiet. The shapes are the server's — `boss_jobs::borders`. */
const rail = (
  from: string,
  to: string,
  over: Record<string, unknown> = {},
): Record<string, unknown> => ({
  from, to,
  crossing: 'a packet crossed',
  rate: { metric: 'crossings', unit: 'per day', current: 2, previous: 3, samples: 2, previous_samples: 3 },
  last_crossed: '2026-09-19T04:00:00Z',
  waiting: 0,
  holds: [],
  machine: { name: 'the receiving desk', kind: 'actors', last_fired: null, silent_for_minutes: null,
    expected_every_minutes: null, silent: null, why: 'no machine moves this hop' },
  state: 'clear',
  why: 'nothing waiting; 2 crossings in 24h',
  ...over,
});

const BORDERS_PAYLOAD = {
  window_hours: 24,
  now: '2026-09-19T05:00:00Z',
  borders: [
    // The rate could not be computed and neither could the queue: the
    // rail must read UNKNOWN, not empty.
    rail('receiving', 'marshalling', {
      rate: { metric: 'crossings', unit: 'per day', current: null, previous: null, samples: 0, previous_samples: 0 },
      last_crossed: null, waiting: null, state: 'troubled',
      why: 'the workflow registry that names the inbound kinds could not be read',
    }),
    // The shop floor split this hop in two (backlog 94c6ffd0): a run
    // OPENS on a packet, and its branch takes a bay once built — then
    // parks on green (design 62de32ae decision 3).
    rail('marshalling', 'shop-floor'),
    rail('shop-floor', 'gates'),
    rail('gates', 'dock'),
    // Traffic waiting and the machine silent past its declared cadence.
    rail('dock', 'track', {
      crossing: 'a car boarded a train',
      rate: { metric: 'crossings', unit: 'per day', current: 24, previous: 18, samples: 24, previous_samples: 18 },
      waiting: 3,
      holds: [{ what: 'fix/a-car', why: 'parked, waiting for the boarding depth or the clock rule' }],
      machine: { name: 'train-board-on-dock-depth', kind: 'cadence', last_fired: '2026-09-19T02:00:00Z',
        silent_for_minutes: 180, expected_every_minutes: 30, silent: true,
        why: 'its own firing in cadence_firings' },
      state: 'troubled',
      why: '3 packets waiting and train-board-on-dock-depth silent for 180m — it declares every 30m',
    }),
    rail('track', 'arrivals'),
    rail('arrivals', 'shed'),
    rail('arrivals', 'publish'),
    rail('gates', 'garage'),
    rail('track', 'garage'),
  ],
};

async function mocks(page: Page): Promise<void> {
  await installSmokeMocks(page);
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route(YARD_REGIONS, (r) => json(r, REGIONS));
  await page.route(YARD_BORDERS, (r) => json(r, BORDERS_PAYLOAD));
}

test('the world paints a territory per region in one SVG, along the flow, with the count and the trend inside each', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');

  const svg = page.locator('section.yard svg');
  await expect(svg).toHaveCount(1);
  const territories = svg.locator('.territory');
  await expect(territories).toHaveCount(TERRITORIES.length);
  const names = await territories.evaluateAll((els) => els.map((el) => el.getAttribute('data-region')));
  expect(new Set(names)).toEqual(new Set(TERRITORIES.map((t) => t.name)));
  // The flow reads left to right in the order a car walks it (design
  // 62de32ae decision 3): gated BEFORE it parks on the dock.
  const xOf = async (name: string) =>
    Number(await svg.locator(`.territory[data-region="${name}"] rect`).first().getAttribute('x'));
  const line = ['receiving', 'marshalling', 'shop-floor', 'gates', 'dock', 'track', 'arrivals', 'shed'];
  const xs = await Promise.all(line.map(xOf));
  expect([...xs].sort((a, b) => a - b)).toEqual(xs);
  // The borders are drawn: one rail per declared hop, the garage fed by both gates and track.
  await expect(svg.locator('[data-border="shop-floor→gates"]')).toHaveCount(1);
  await expect(svg.locator('[data-border="gates→dock"]')).toHaveCount(1);
  await expect(svg.locator('[data-border="dock→track"]')).toHaveCount(1);
  await expect(svg.locator('[data-border="gates→garage"]')).toHaveCount(1);
  await expect(svg.locator('[data-border="track→garage"]')).toHaveCount(1);

  // The count over its bound, the state, the trend in its unit — inside the outline.
  const dock = svg.locator('.territory[data-region="dock"]');
  await expect(dock).toContainText('3 / 5');
  await expect(dock).toContainText('clear');
  await expect(dock).toContainText('dock wait');
  await expect(dock).toContainText('4.3 vs 3 hours');
  // A half nobody measured is a dash, never a zero.
  await expect(svg.locator('.territory[data-region="track"]')).toContainText('— vs 14 minutes');
  // An unread region prints no number at all.
  await expect(svg.locator('.territory[data-region="marshalling"]')).toContainText('no reading');
});

test('a troubled territory looks troubled where it is and carries its why', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');

  const svg = page.locator('section.yard svg');
  await expect(svg.locator('.territory[data-state="troubled"]')).toHaveCount(2);
  const gates = svg.locator('.territory[data-region="gates"]');
  await expect(gates).toHaveAttribute('data-state', 'troubled');
  // The yard's own trouble strokes — the outline and the lamp — and
  // the reason printed inside the outline: a verdict must name what
  // failed. The whole why rides the title.
  await expect(gates.locator('rect.shed.err')).toHaveCount(1);
  await expect(gates.locator('.lamp.err')).toHaveCount(1);
  await expect(gates.locator('text.why')).toContainText('1 bay holds a corpse');
  await expect(gates.locator('title')).toHaveText(/1 bay holds a corpse — a gate-run past its own deadline/);
  // A clear territory carries none of that.
  const dock = svg.locator('.territory[data-region="dock"]');
  await expect(dock.locator('text.why')).toHaveCount(0);
  await expect(dock.locator('rect.shed.err')).toHaveCount(0);
  await expect(dock.locator('.lamp.ok')).toHaveCount(1);
  // Attention wears the warn stroke.
  await expect(svg.locator('.territory[data-region="shed"] rect.shed.warn')).toHaveCount(1);
});

test('a state that is not clear says how long it has held and names the band that decided it', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const receiving = page.locator('section.yard svg .territory[data-region="receiving"]');
  await expect(receiving).toHaveAttribute('data-state', 'attention');
  await expect(receiving.locator('text.state')).toHaveText('attention for 36h');
  // The band against its number, where the state is — not in a legend.
  // (wrapped over tspans, so read line by line)
  await expect(receiving.locator('text.why.warn tspan').first()).toHaveText('oldest 5d > the');
  await expect(receiving.locator('text.why.warn tspan').nth(1)).toHaveText('3-day triage band');
  // The KPI in its unit, as the server wrote it.
  await expect(receiving.locator('text.kpi tspan').first()).toHaveText('oldest untriaged 5');
  await expect(receiving.locator('text.kpi tspan').nth(1)).toHaveText('days');
  // The hover carries the count in its unit and the whole band.
  await expect(receiving.locator('title').first()).toContainText(
    'receiving · 4 packets standing · attention for 36h [oldest 5d > the 3-day triage band]',
  );
});

test('a territory click opens its floor, and the floor opens with the region\'s state and why at the head of its panel', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const svg = page.locator('section.yard svg');
  await expect(svg.locator('.territory')).toHaveCount(TERRITORIES.length);

  // A troubled yard region: the Train Yard on the gates' panel, headed
  // by the map's own verdict — the floor cannot contradict the map.
  await svg.locator('.territory[data-region="gates"]').click();
  await expect(page).toHaveURL(/\/it\/yard\/gates$/);
  await expect(page.locator('.yard-panel-h', { hasText: 'Entity · approach' })).toBeVisible();
  const head = page.locator('.yard-region-head');
  await expect(head).toHaveAttribute('data-region', 'gates');
  await expect(head).toHaveAttribute('data-state', 'troubled');
  await expect(head).toContainText('1 bay holds a corpse');
  await expect(head.locator('.yard-lamp-dot.err')).toHaveCount(1);

  // Back returns to the world.
  await page.goBack();
  await expect(page).toHaveURL(/\/it$/);
  await expect(page.locator('section.yard svg .territory')).toHaveCount(TERRITORIES.length);

  // A clear region's floor says so at its head, too.
  await page.locator('section.yard svg .territory[data-region="dock"]').click();
  await expect(page).toHaveURL(/\/it\/yard\/dock$/);
  await expect(page.locator('.yard-panel-h', { hasText: 'Entity · loading dock' })).toBeVisible();
  await expect(page.locator('.yard-region-head')).toContainText('dock · clear — 3 cars parked');

  // A region whose floor is a QUEUE BOARD. Its page retired on car 4
  // of design d2154293: the click is a zoom into the region like every
  // other, and the board mounts under it.
  await page.goto('/it');
  await page.locator('section.yard svg .territory[data-region="receiving"]').click();
  await expect(page).toHaveURL(/\/it\/yard\/receiving$/);
});

// THE HUD FRAME (design 00774ca8): whole-system figures, one row per
// third in the payload's order, a machine cell beside them, and the four
// pictures — value, zero, floor, unread — each drawn as itself.
test('the HUD frame stands above the map, one row per third, each figure the server’s', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const hud = page.locator('[data-hud]');
  await expect(hud).toHaveAttribute('data-read', 'ok');
  await expect(hud.locator('.hud-age')).toContainText('window 24h');
  // Above the map: the frame comes first on the page.
  const hudTop = (await hud.boundingBox())!.y;
  const mapTop = (await page.locator('section.yard svg').boundingBox())!.y;
  expect(hudTop).toBeLessThan(mapTop);

  const rows = hud.locator('.hud-row');
  expect(await rows.evaluateAll((els) => els.map((el) => el.getAttribute('data-third')))).toEqual([
    'queue-management', 'actors-building', 'delivery',
  ]);

  // A value, with its rates: the server's net, never a client sum.
  const qm = hud.locator('.hud-row[data-third="queue-management"]');
  await expect(qm.locator('[data-cell="balance"]')).toContainText('+12/day');
  await expect(qm.locator('[data-cell="balance"] .hud-rates [data-fig="value"]')).toHaveText(['40', '28']);
  await expect(qm.locator('[data-cell="balance"] .hud-rates')).toContainText('inbound packets/day');
  // A floor: ≥ n, then ? in its own band.
  const floor = qm.locator('[data-cell="stuck"] [data-fig="floor"]');
  await expect(floor).toContainText('≥ 3');
  await expect(floor.locator('.q-band')).toHaveText('?');
  await expect(floor.locator('.plate-troubled')).toHaveCount(1);
  // Unread: ? in a dashed housing, three times, never 0.
  const ab = hud.locator('.hud-row[data-third="actors-building"]');
  await expect(ab.locator('[data-cell="balance"] [data-fig="unread"]')).toHaveCount(3);
  await expect(ab.locator('[data-cell="balance"]')).not.toContainText('0');
  // A true zero: 0, with no mark; the stuck count above zero is a plate.
  const d = hud.locator('.hud-row[data-third="delivery"]');
  await expect(d.locator('[data-cell="balance"] [data-fig="zero"]').first()).toHaveText('0');
  await expect(d.locator('[data-cell="stuck"] .plate-troubled')).toHaveText('1');
  await expect(ab.locator('[data-cell="stuck"] [data-fig="zero"]')).toHaveCount(2);
  // Decision 4's one exception: the arrivals territory's own trend.
  await expect(d.locator('[data-cell="arrivals"]')).toContainText('17');
  await expect(d.locator('[data-cell="arrivals"]')).toContainText('12');
  await expect(qm.locator('[data-cell="arrivals"]')).toHaveCount(0);

  // The machine cell: failed and unjudged against a total, each
  // unjudged machine a door to its region's map.
  const machines = hud.locator('[data-machines]');
  const line = machines.locator('.hud-machine-line');
  await expect(line.locator('[data-fig]')).toHaveText(['0', '2', '25']);
  await expect(line.locator('[data-fig="zero"]')).toHaveCount(1);
  await expect(line).toContainText('unjudged of');
  await expect(machines.locator('a[data-machine]')).toHaveCount(2);
  await machines.locator('a[data-machine="station:design-review"]').click();
  await expect(page).toHaveURL(/\/it\/yard\/marshalling$/);
  // It does not follow the zoom: the same frame stands over the region.
  await expect(page.locator('[data-hud] .hud-row')).toHaveCount(3);
  await expect(page.locator('[data-hud] [data-strip]')).toHaveCount(1);
});

test('a regions read that fails turns every HUD cell to ? and says when, keeping no value', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(YARD_REGIONS, (r) =>
    r.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify('the backend is down') }),
  );
  await page.goto('/it');
  const hud = page.locator('[data-hud]');
  await expect(hud).toHaveAttribute('data-read', 'failed');
  await expect(hud.locator('.hud-age')).toContainText(/read failed \d\d:\d\dZ · no good read yet/);
  await expect(hud.locator('.hud-row')).toHaveCount(3);
  await expect(hud.locator('[data-fig="value"], [data-fig="zero"], [data-fig="floor"]')).toHaveCount(0);
  expect(await hud.locator('[data-fig="unread"]').count()).toBeGreaterThan(0);
});

test('a regions read that fails is said, never drawn as a clear world', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(YARD_REGIONS, (r) =>
    r.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify('the backend is down') }),
  );
  await page.goto('/it');
  await expect(page.locator('.load-failed')).toContainText('The regions cannot be read');
  await expect(page.locator('.territory')).toHaveCount(0);
});

test('a border carries its traffic, what waits on it and the machine that moves it', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const svg = page.locator('section.yard svg');
  // One crossing token per declared rail.
  await expect(svg.locator('.crossing')).toHaveCount(BORDERS.length);

  // The boarding rail: three waiting, a heavy traffic band from the
  // measured rate, and the machine's lamp lit because IT declared the
  // cadence it has been silent past.
  const boarding = svg.locator('.crossing[data-crossing="dock→track"]');
  await expect(boarding).toHaveAttribute('data-state', 'troubled');
  await expect(boarding).toHaveAttribute('data-waiting', '3');
  await expect(boarding.locator('text.token-count')).toHaveText('3');
  await expect(boarding.locator('text.rate')).toHaveText('24/d');
  await expect(boarding.locator('.machine-lamp.err')).toHaveCount(1);
  await expect(svg.locator('[data-traffic="dock→track"]')).toHaveAttribute('data-density', 'heavy');
  // BORDERS ARE DRAWN, NOT TOOLTIPPED (design 62de32ae decision 6): the
  // machine's name and its status are WRITTEN on the rail, with no
  // hover title standing in for them.
  await expect(boarding.locator('text.machine-name')).toHaveText(['train-board-on-dock-', 'depth']);
  await expect(boarding.locator('text.machine-status')).toHaveText('SILENT 180m');
  await expect(boarding.locator('title')).toHaveCount(0);
  // And the rail is as wide as its rate: 24 a day draws wider than 2.
  const widthOf = async (key: string) =>
    Number(await svg.locator(`path.rail[data-rail="${key}"]`).getAttribute('data-width'));
  expect(await widthOf('dock→track')).toBeGreaterThan(await widthOf('gates→dock'));

  // A click opens the crossing INLINE, under the map: everything the
  // rail knows, where it can be read, touched and screenshotted.
  await expect(page.locator('.crossing-panel')).toHaveCount(0);
  await boarding.click();
  const panel = page.locator('.crossing-panel[data-panel="dock→track"]');
  await expect(panel).toBeVisible();
  await expect(boarding).toHaveAttribute('aria-expanded', 'true');
  await expect(panel).toContainText('a car boarded a train');
  await expect(panel).toContainText('24 vs 18 /day');
  await expect(panel).toContainText('fix/a-car — parked, waiting for the boarding depth');
  await expect(panel).toContainText('train-board-on-dock-depth · SILENT 180m');
  await expect(panel).toContainText('its own firing in cadence_firings');
  await expect(panel).toContainText('2026-09-19 04:00 UTC · 1h ago');
  // Three wait and the server listed one: the panel says so rather
  // than under-reporting the queue.
  await expect(panel).toContainText('+2 more waiting, not listed');
  // Another rail's click swaps the panel; the same rail's closes it.
  await svg.locator('.crossing[data-crossing="gates→dock"]').click();
  await expect(page.locator('.crossing-panel[data-panel="gates→dock"]')).toBeVisible();
  await expect(panel).toHaveCount(0);
  await svg.locator('.crossing[data-crossing="gates→dock"]').click();
  await expect(page.locator('.crossing-panel')).toHaveCount(0);

  // The client-side sum of these rails is retired (design 00774ca8
  // decision 10): the whole-system figures are the HUD frame's.
  await expect(page.locator('.yard-flow', { hasText: 'crossings in' })).toHaveCount(0);
});

test('a border the server could not measure reads unknown, never zero', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const svg = page.locator('section.yard svg');
  const blind = svg.locator('.crossing[data-crossing="receiving→marshalling"]');
  await expect(blind).toHaveAttribute('data-waiting', 'unknown');
  // The token prints `?` and the rate prints `?` — no zero anywhere on
  // the rail, and the traffic band is its own, not the empty one.
  await expect(blind.locator('text.token-count')).toHaveText('?');
  await expect(blind.locator('text.rate')).toHaveText('?');
  await expect(svg.locator('[data-traffic="receiving→marshalling"]')).toHaveAttribute(
    'data-density',
    'unknown',
  );
  // Opened, the crossing says the same: no reading, and which read
  // failed — never a zero.
  await blind.click();
  const panel = page.locator('.crossing-panel[data-panel="receiving→marshalling"]');
  await expect(panel).toContainText('the workflow registry that names the inbound kinds could not be read');
  await expect(panel.locator('dd').nth(1)).toHaveText('no reading');
  await expect(panel).toContainText('waiting: no reading');
  await expect(panel).toContainText('nothing crossed in the two windows read');
});

test('a borders read that fails is said, and the territories still paint', async ({ page }) => {
  await installSmokeMocks(page);
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route(YARD_REGIONS, (r) => json(r, REGIONS));
  await page.route(YARD_BORDERS, (r) =>
    r.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify('the backend is down') }),
  );
  await page.goto('/it');
  await expect(page.locator('.load-failed')).toContainText('The borders cannot be read');
  // The world is still a world: every territory, every rail, and every
  // number on them unknown.
  await expect(page.locator('section.yard svg .territory')).toHaveCount(TERRITORIES.length);
  await expect(page.locator('section.yard svg .crossing')).toHaveCount(BORDERS.length);
  await expect(page.locator('section.yard svg .crossing[data-waiting="unknown"]')).toHaveCount(BORDERS.length);
});
