// THE IT SYSTEM MAP — design 0524fc95, car 2 (car 1 is the server
// read, GET /api/yard/regions). /it is a map of eight region cards
// read from that ONE endpoint, each a door to its floor: the panels
// that already exist, one click deeper. This pins the three things
// the packet names: the map renders eight cards from a fixture
// payload, a troubled card looks troubled, and a card click opens its
// floor (a yard region opens the Train Yard on that region's panel;
// receiving opens its own page).

import { expect, test, type Page, type Route } from '@playwright/test';
import { YARD_REGIONS, installSmokeMocks } from './_smokeMocks';

const trend = (metric: string, unit: string, current: number | null, previous: number | null) => ({
  metric, unit, current, previous, samples: current === null ? 0 : 5, previous_samples: previous === null ? 0 : 4,
});

/** The wire shape car 1 serialises, in map order, with the gates
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

test('the map renders eight cards from the regions payload, in map order, with the count and the trend', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');

  const cards = page.locator('.yard-region');
  await expect(cards).toHaveCount(8);
  const names = await cards.evaluateAll((els) => els.map((el) => el.getAttribute('data-region')));
  expect(names).toEqual(['dock', 'gates', 'track', 'shed', 'arrivals', 'garage', 'receiving', 'marshalling']);

  // The count over its bound, the state, the trend in its unit.
  const dock = page.locator('.yard-region[data-region="dock"]');
  await expect(dock).toContainText('3 / 5');
  await expect(dock).toContainText('clear');
  await expect(dock).toContainText('dock wait · 4.3 vs 3 hours');
  // A half nobody measured is a dash, never a zero.
  await expect(page.locator('.yard-region[data-region="track"]')).toContainText('— vs 14 minutes');
  // An unread region prints no number at all.
  await expect(page.locator('.yard-region[data-region="marshalling"]')).toContainText('no reading');
});

test('a troubled card looks troubled and names why', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');

  const troubled = page.locator('.yard-region[data-state="troubled"]');
  await expect(troubled).toHaveCount(2);
  const gates = page.locator('.yard-region[data-region="gates"]');
  await expect(gates).toHaveAttribute('data-state', 'troubled');
  // The yard's own trouble badge and lamp, and the reason printed on
  // the card — a verdict must name what failed.
  await expect(gates.locator('.yard-trouble')).toHaveText('troubled');
  await expect(gates.locator('.yard-lamp-dot.err')).toHaveCount(1);
  await expect(gates).toContainText('1 bay holds a corpse');
  // A clear card carries none of that.
  const dock = page.locator('.yard-region[data-region="dock"]');
  await expect(dock.locator('.yard-trouble')).toHaveCount(0);
  await expect(dock.locator('.yard-lamp-dot.ok')).toHaveCount(1);
});

test('a card click opens its floor — the yard on that region\'s panel, or the region\'s own page', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  await expect(page.locator('.yard-region')).toHaveCount(8);

  // A yard region: the Train Yard, focused on the dock's panel.
  await page.locator('.yard-region[data-region="dock"]').click();
  await expect(page).toHaveURL(/\/it\/yard\/dock$/);
  await expect(page.locator('.yard-panel-h', { hasText: 'Entity · loading dock' })).toBeVisible();

  // Back returns to the map.
  await page.goBack();
  await expect(page).toHaveURL(/\/it$/);
  await expect(page.locator('.yard-region')).toHaveCount(8);

  // The shed: the inspection shed's proof lanes.
  await page.locator('.yard-region[data-region="shed"]').click();
  await expect(page).toHaveURL(/\/it\/yard\/shed$/);
  await expect(page.locator('.yard-panel-h', { hasText: 'Entity · inspection shed' })).toBeVisible();

  // A region with a page of its own.
  await page.goto('/it');
  await page.locator('.yard-region[data-region="receiving"]').click();
  await expect(page).toHaveURL(/\/it\/operate\/receiving$/);
});

test('a regions read that fails is said, never drawn as a clear map', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(YARD_REGIONS, (r) =>
    r.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify('the backend is down') }),
  );
  await page.goto('/it');
  await expect(page.locator('.load-failed')).toContainText('The regions cannot be read');
  await expect(page.locator('.yard-region')).toHaveCount(0);
});
