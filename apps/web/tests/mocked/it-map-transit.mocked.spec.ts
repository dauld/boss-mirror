// THE IT MAP AS A TRANSIT MONITOR — behind its flight (design 16091dfb,
// answered by David 2026-09-25; backlog ced4ca8b). The layout and every
// word are pinned in transit.test.ts; this pins the page: with the
// flight OFF the world map is the map it was and no transit map is
// drawn; ON, the same two reads are drawn as stations and sections — a
// held section red with its held-since, blocks waiting on the approach
// and a count past the cap, a moving block where the rate earns one,
// the planned tenant branch dashed, a troubled station pulsing, the
// alarms board carrying the server's why — and a station opens its
// region page. Reduced motion moves nothing. A phone keeps its strip.

import { expect, test, type Page, type Route } from '@playwright/test';
import { SECTIONS, STATIONS, MAX_BLOCKS } from '../../src/it/yard/transit';
import { BORDERS, TERRITORIES } from '../../src/it/yard/world';
import { YARD_BORDERS, YARD_REGIONS, installSmokeMocks } from './_smokeMocks';

const FLIGHTS = /\/api\/flights\/mine(\?|$)/;
const NOW = '2026-09-24T12:00:00Z';

const json = (r: Route, b: unknown) => r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });

/** Every region clear but the gates (troubled) and receiving (attention). */
const regions = () => ({
  window_hours: 24,
  now: NOW,
  regions: TERRITORIES.map(({ name }) => ({
    name,
    count: 1,
    unit: 'things',
    state: name === 'gates' ? 'troubled' : name === 'receiving' ? 'attention' : 'clear',
    why: name === 'gates' ? 'a bay holds a corpse' : name === 'receiving' ? '80 packets standing, the oldest 5 days' : 'fine',
    trend: { metric: 'crossings', unit: 'per day', current: 4, previous: 4, samples: 4, previous_samples: 4 },
  })),
});

/** Every section busy and flowing with 7 waiting, but dock→track held
 *  since 08:48 and receiving→marshalling unread. */
const borders = () => ({
  window_hours: 24,
  now: NOW,
  borders: BORDERS.map(({ from, to }) => {
    const key = `${from}→${to}`;
    const base = {
      from, to, crossing: 'a packet crossed', state: 'clear', why: 'flowing',
      rate: { metric: 'crossings', unit: 'per day', current: 480, previous: 400, samples: 480, previous_samples: 400 },
      last_crossed: '2026-09-24T11:58:00Z', waiting: 7, holds: [],
      holds_by_class: { machine: 3, person: 2, unknown: 1, stuck: 1 },
      flowing: true, held_since: null, flowing_why: 'last crossed 2m ago, inside 4× its mean gap',
      machine: { name: 'a rule', kind: 'dispatcher-rule', last_fired: null, silent_for_minutes: null,
        expected_every_minutes: null, silent: null, why: 'no firing recorded' },
    };
    if (key === 'dock→track') {
      return { ...base, rate: { ...base.rate, current: 6 }, last_crossed: '2026-09-24T08:48:00Z', flowing: false,
        held_since: '2026-09-24T08:48:00Z', flowing_why: 'quiet 3h 12m > 4 mean gaps' };
    }
    if (key === 'receiving→marshalling') {
      return { ...base, rate: { ...base.rate, current: null, previous: null, samples: 0, previous_samples: 0 },
        waiting: null, holds_by_class: null, flowing: null, flowing_why: 'its crossings could not be read' };
    }
    return base;
  }),
});

async function mocks(page: Page, flights: ReadonlyArray<string>): Promise<void> {
  await installSmokeMocks(page);
  await page.route(FLIGHTS, (r) => json(r, { flights }));
  await page.route(YARD_REGIONS, (r) => json(r, regions()));
  await page.route(YARD_BORDERS, (r) => json(r, borders()));
}

test('flight OFF: the world map is the map, and no transit map is drawn', async ({ page }) => {
  await mocks(page, []);
  await page.goto('/it');
  await expect(page.locator('section.yard svg .territory')).toHaveCount(TERRITORIES.length);
  await expect(page.locator('[data-transit]')).toHaveCount(0);
  await expect(page.locator('[data-station]')).toHaveCount(0);
});

test('flight ON: the same reads drawn as a transit monitor', async ({ page }) => {
  await mocks(page, ['it-map-transit']);
  await page.goto('/it');
  const transit = page.locator('section.transit[data-transit]');
  await expect(transit).toBeVisible();
  // The world map is not ALSO mounted: one map, never two.
  await expect(page.locator('section.yard svg .territory')).toHaveCount(0);

  // A station per region, a section per border.
  await expect(transit.locator('[data-station]')).toHaveCount(STATIONS.length);
  await expect(transit.locator('path[data-section]')).toHaveCount(SECTIONS.length);
  await expect(transit.locator('[data-station="gates"]')).toHaveAttribute('data-state', 'troubled');
  await expect(transit.locator('[data-station="receiving"]')).toHaveAttribute('data-state', 'attention');

  // The server's stillness, on the track itself.
  await expect(transit.locator('[data-section="dock→track"]')).toHaveAttribute('data-ground', 'held');
  await expect(transit.locator('[data-headway="dock→track"]')).toHaveText('6/d · held 08:48Z');
  await expect(transit.locator('[data-section="receiving→marshalling"]')).toHaveAttribute('data-ground', 'unknown');
  await expect(transit.locator('[data-headway="receiving→marshalling"]')).toHaveText('no reading');
  // The headway a dispatcher reads: 480 a day is a 3-minute gap.
  await expect(transit.locator('[data-headway="shop-floor→gates"]')).toHaveText('480/d · 3m gap');

  // Waiting blocks on the approach, the rest a count.
  await expect(transit.locator('[data-waiting="shop-floor→gates"]')).toHaveCount(MAX_BLOCKS);
  await expect(transit.locator('[data-more="shop-floor→gates"]')).toHaveText(`+${7 - MAX_BLOCKS}`);
  // Nothing drawn waiting where the count was not read.
  await expect(transit.locator('[data-waiting="receiving→marshalling"]')).toHaveCount(0);

  // A moving block where the rate earns one (480/d is one every 3 s at ×60), none on a held section.
  expect(await transit.locator('[data-train="shop-floor→gates"]').count()).toBeGreaterThan(0);
  await expect(transit.locator('[data-train="shop-floor→gates"] animateMotion').first()).toHaveAttribute('dur', '3.000s');
  await expect(transit.locator('[data-train="dock→track"]')).toHaveCount(0);
  await expect(transit.locator('[data-replay]')).toContainText('replayed ×60');

  // The planned tenant branch, and a troubled station pulsing.
  await expect(transit.locator('[data-planned="tenant"] [data-owner="David"]')).toHaveCount(1);
  await expect(transit.locator('[data-pulse="gates"]')).toHaveCount(1);
  await expect(transit.locator('[data-pulse]')).toHaveCount(1);

  // The alarms board: troubled first, with the server's why.
  const alarms = transit.locator('aside.alarms .alarm');
  await expect(alarms).toHaveCount(2);
  await expect(alarms.nth(0)).toHaveAttribute('data-alarm', 'gates');
  await expect(alarms.nth(0)).toContainText('a bay holds a corpse');
  await expect(alarms.nth(1)).toHaveAttribute('data-alarm', 'receiving');
  await expect(alarms.nth(1)).toContainText('80 packets standing, the oldest 5 days');

  // A station selects itself (design e765b3fc, car N1): the transit map
  // stays on top, the dock's panel opens under it, and the panel is the
  // door to the dock's floor.
  await transit.locator('[data-station="dock"]').click();
  await expect(page).toHaveURL(/\/it\?at=dock$/);
  await expect(transit).toBeVisible();
  await expect(page.locator('section[data-map-panel]')).toHaveAttribute('data-selection', 'dock');
  await page.locator('section[data-map-panel] a[data-floor]').click();
  await expect(page).toHaveURL(/\/it\/yard\/dock$/);
  await expect(page.locator('nav.crumbs[data-region="dock"]')).toBeVisible();
});

test('reduced motion: nothing moves and nothing pulses, and the stall still reads', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await mocks(page, ['it-map-transit']);
  await page.goto('/it');
  const transit = page.locator('section.transit');
  await expect(transit).toHaveAttribute('data-motion', 'reduced');
  await expect(transit.locator('[data-station]')).toHaveCount(STATIONS.length);
  await expect(transit.locator('[data-train]')).toHaveCount(0);
  await expect(transit.locator('animateMotion')).toHaveCount(0);
  await expect(transit.locator('[data-pulse]')).toHaveCount(0);
  await expect(transit.locator('[data-section="dock→track"]')).toHaveAttribute('data-ground', 'held');
  await expect(transit.locator('[data-replay]')).toContainText('Reduced motion is on');
});

test('a phone keeps its strip under the flight, with no horizontal scroll', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await mocks(page, ['it-map-transit']);
  await page.goto('/it');
  await expect(page.locator('.strip-third').first()).toBeVisible();
  await expect(page.locator('[data-transit]')).toHaveCount(0);
  await expect(page.locator('section.yard svg .territory')).toHaveCount(0);
  const [scroll, client] = await page.evaluate(() => [document.documentElement.scrollWidth, document.documentElement.clientWidth]);
  expect(scroll).toBeLessThanOrEqual(client);
});

test('at a narrow desktop the board scrolls inside itself, never the page', async ({ page }) => {
  await page.setViewportSize({ width: 800, height: 900 });
  await mocks(page, ['it-map-transit']);
  await page.goto('/it');
  await expect(page.locator('section.transit[data-transit]')).toBeVisible();
  const [scroll, client] = await page.evaluate(() => [document.documentElement.scrollWidth, document.documentElement.clientWidth]);
  expect(scroll).toBeLessThanOrEqual(client);
});
