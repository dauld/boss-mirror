// THE IT WORLD MAP — design d2154293, car 1 (over 0524fc95's regions
// read, GET /api/yard/regions). /it is ONE SVG world: eight
// territories laid out along the packet flow (world.ts), each drawn
// in the yard's strokes with the region's count, state and trend
// inside its outline, the why on a troubled one, and a door to its
// floor. This pins what the packet names: the world paints eight
// territories from a fixture payload, a troubled one carries its why
// where it is, a click opens the floor — and the floor's panel opens
// with the region's own state and why at its head (a106309c), from
// the same read, so clicking in cannot contradict the map.

import { expect, test, type Page, type Route } from '@playwright/test';
import { YARD_REGIONS, installSmokeMocks } from './_smokeMocks';

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
    { name: 'shed', count: 2, state: 'busy', why: '2 landed cars await their probe', trend: trend('time to proven', 'hours', 1, 1.5) },
    { name: 'arrivals', count: 17, state: 'clear', why: '17 trains arrived in the window', trend: trend('arrivals', 'per day', 17, 12) },
    { name: 'garage', count: 0, state: 'clear', why: 'nothing gated red', trend: trend('reds', 'per day', 0, 2) },
    { name: 'receiving', count: 4, state: 'busy', why: '4 inbound, oldest 5 days', trend: trend('inbound', 'per day', 4, 6) },
    { name: 'marshalling', count: null, state: 'troubled', why: 'the station registry could not be read', trend: trend('served', 'per day', null, null) },
  ],
};

async function mocks(page: Page): Promise<void> {
  await installSmokeMocks(page);
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route(YARD_REGIONS, (r) => json(r, REGIONS));
}

test('the world paints eight territories in one SVG, along the flow, with the count and the trend inside each', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');

  const svg = page.locator('section.yard svg');
  await expect(svg).toHaveCount(1);
  const territories = svg.locator('.territory');
  await expect(territories).toHaveCount(8);
  const names = await territories.evaluateAll((els) => els.map((el) => el.getAttribute('data-region')));
  expect(new Set(names)).toEqual(new Set(['dock', 'gates', 'track', 'shed', 'arrivals', 'garage', 'receiving', 'marshalling']));
  // The flow reads left to right: each territory on the line starts
  // right of the one packets leave to reach it.
  const xOf = async (name: string) =>
    Number(await svg.locator(`.territory[data-region="${name}"] rect`).getAttribute('x'));
  const line = ['receiving', 'marshalling', 'dock', 'gates', 'track', 'arrivals', 'shed'];
  const xs = await Promise.all(line.map(xOf));
  expect([...xs].sort((a, b) => a - b)).toEqual(xs);
  // The borders are drawn: one rail per declared hop, the garage fed by both gates and track.
  await expect(svg.locator('[data-border="gates→track"]')).toHaveCount(1);
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
  // Busy wears the warn stroke.
  await expect(svg.locator('.territory[data-region="shed"] rect.shed.warn')).toHaveCount(1);
});

test('a territory click opens its floor, and the floor opens with the region\'s state and why at the head of its panel', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const svg = page.locator('section.yard svg');
  await expect(svg.locator('.territory')).toHaveCount(8);

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
  await expect(page.locator('section.yard svg .territory')).toHaveCount(8);

  // A clear region's floor says so at its head, too.
  await page.locator('section.yard svg .territory[data-region="dock"]').click();
  await expect(page).toHaveURL(/\/it\/yard\/dock$/);
  await expect(page.locator('.yard-panel-h', { hasText: 'Entity · loading dock' })).toBeVisible();
  await expect(page.locator('.yard-region-head')).toContainText('dock · clear — 3 cars parked');

  // A region with a page of its own.
  await page.goto('/it');
  await page.locator('section.yard svg .territory[data-region="receiving"]').click();
  await expect(page).toHaveURL(/\/it\/operate\/receiving$/);
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
