// THE IT WORLD ON A PHONE — design 62de32ae decision 12, car G.
//
// Measured 2026-09-24 at 390px on /it: the page was 1246px wide. The
// app shell reserved the fixed sidebar's 200px column at every width and
// let the content column grow to the world map's 900px floor, and the
// chrome bar's own tab, search and sign-in ran on to x=877, past the
// edge of the screen. This pins the phone view the design asks for:
//
//  - no horizontal scroll at 390px, and every control of the chrome bar
//    reachable by scrolling the bar itself;
//  - the sidebar collapsed, the content starting at the 16px gutter;
//  - the world drawn as a vertical strip — one row per region in flow
//    order, grouped under the three thirds, each row its state, KPI and
//    the rail coming into it — and the SVG world NOT drawn (not shrunk);
//  - a row is a door to its region, as a territory is;
//  - and at desktop width the world is the SVG, with no strip.

import { expect, test, type Page, type Route } from '@playwright/test';
import { YARD_BORDERS, YARD_REGIONS, installSmokeMocks } from './_smokeMocks';
import { BORDERS } from '../../src/it/yard/world';
const PHONE = { width: 390, height: 844 };

const trend = { metric: 'm', unit: 'per day', current: 1, previous: 1, samples: 1, previous_samples: 1 };

/** The server's partition, as the regions read carries it (car F's
 *  `thirds` block, boss_jobs::regions::THIRDS) — the strip's grouping
 *  is read from here and nowhere else. */
const THIRDS = [
  { third: 'queue-management', regions: ['receiving', 'marshalling'] },
  { third: 'actors-building', regions: ['shop-floor', 'gates', 'garage'] },
  { third: 'delivery', regions: ['dock', 'track', 'arrivals', 'shed', 'publish'] },
] as const;

/** Every region clear with a KPI in its unit, but for three: receiving
 *  on a declared band, the gates troubled, and marshalling unread. */
const REGIONS = {
  window_hours: 24,
  now: '2026-09-24T12:00:00Z',
  thirds: THIRDS.map(({ third, regions }) => ({
    third, regions,
    balance: { unit: 'packets', in_means: 'in', out_means: 'out', in: 1, out: 1, net: 0, in_count: 1, out_count: 1 },
    stuck: { third, stuck: 0, waiting: 0, unknown: [], oldest_hours: null, regions: [] },
  })),
  regions: THIRDS.flatMap((t) => t.regions).map((name) => {
    const base = {
      name, count: 1, state: 'clear', why: 'fine', trend,
      kpi: [{ name: 'kpi', value: 1, unit: 'things', text: `${name} kpi 1 thing` }],
    };
    if (name === 'receiving') {
      return {
        ...base, state: 'attention', why: '4 inbound, oldest 5 days',
        band: { id: 'receiving-aging', reads: 'oldest 5d > the 3-day triage band', hold_minutes: 0, since: null, held_minutes: 2160, held: '36h' },
        kpi: [{ name: 'oldest untriaged', value: 5, unit: 'days', text: 'oldest untriaged 5 days' }],
      };
    }
    if (name === 'gates') return { ...base, state: 'troubled', why: '1 bay holds a corpse' };
    if (name === 'marshalling') return { ...base, count: null, kpi: [], state: 'troubled', why: 'the station registry could not be read' };
    return base;
  }),
};

const rail = (from: string, to: string, over: Record<string, unknown> = {}) => ({
  from, to, crossing: 'a packet crossed', state: 'clear', why: '',
  rate: { metric: 'crossings', unit: 'per day', current: 2, previous: 3, samples: 2, previous_samples: 3 },
  last_crossed: null, waiting: 0, holds: [],
  machine: { name: 'm', kind: 'actors', last_fired: null, silent_for_minutes: null, expected_every_minutes: null, silent: null, why: '' },
  ...over,
});

const BORDERS_PAYLOAD = {
  window_hours: 24,
  now: '2026-09-24T12:00:00Z',
  borders: BORDERS.map(({ from, to }) => {
    if (from === 'receiving') {
      return rail(from, to, {
        waiting: null, state: 'troubled',
        rate: { metric: 'crossings', unit: 'per day', current: null, previous: null, samples: 0, previous_samples: 0 },
      });
    }
    if (from === 'dock' && to === 'track') {
      return rail(from, to, { waiting: 3, state: 'troubled', rate: { metric: 'crossings', unit: 'per day', current: 24, previous: 18, samples: 24, previous_samples: 18 } });
    }
    return rail(from, to);
  }),
};

async function mocks(page: Page): Promise<void> {
  await installSmokeMocks(page);
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route(YARD_REGIONS, (r) => json(r, REGIONS));
  await page.route(YARD_BORDERS, (r) => json(r, BORDERS_PAYLOAD));
}

const noSideScroll = (page: Page) =>
  page.evaluate(() => ({
    scroll: document.documentElement.scrollWidth,
    client: document.documentElement.clientWidth,
  }));

test('at 390px /it has no horizontal scroll, and the chrome bar’s controls are all reachable', async ({ page }) => {
  await page.setViewportSize(PHONE);
  await mocks(page);
  await page.goto('/it');
  await expect(page.locator('.strip .strip-row').first()).toBeVisible();

  const w = await noSideScroll(page);
  expect(w.scroll).toBeLessThanOrEqual(w.client);

  // The bar scrolls inside itself: each control, brought into view,
  // stands inside the screen — none is stranded past its edge.
  for (const control of ['.perspective-more-btn', '.gs-input', '.fb-trigger', '.signin-btn']) {
    const el = page.locator(`.perspective-tabs ${control}`).first();
    if ((await el.count()) === 0) continue;
    await el.scrollIntoViewIfNeeded();
    const box = (await el.boundingBox())!;
    // Half a pixel either side: a control scrolled flush to the edge
    // measures 390.4 on a subpixel layout.
    expect({ control, left: box.x >= -0.5, right: box.x + box.width <= PHONE.width + 0.5 }).toEqual({
      control, left: true, right: true,
    });
  }
  // Scrolling the bar never scrolls the page.
  const after = await noSideScroll(page);
  expect(after.scroll).toBeLessThanOrEqual(after.client);

  // The sidebar collapsed: it no longer holds a column beside the page,
  // and the content starts at the 16px gutter.
  const content = (await page.locator('.shell-content').boundingBox())!;
  expect(content.x).toBe(0);
  expect(content.width).toBe(PHONE.width);
  const strip = (await page.locator('section.strip').boundingBox())!;
  expect(strip.x).toBe(16);
  expect(strip.x + strip.width).toBe(PHONE.width - 16);
});

test('the More menu opens inside the screen at 390px, not clipped by the scrolling bar', async ({ page }) => {
  await page.setViewportSize(PHONE);
  await mocks(page);
  await page.goto('/it');
  const more = page.locator('.perspective-more-btn');
  await more.scrollIntoViewIfNeeded();
  await more.click();
  const menu = page.locator('.perspective-more-menu');
  await expect(menu).toBeVisible();
  const box = (await menu.boundingBox())!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(PHONE.width);
  // What is ON TOP at the menu's first item is the menu: a clipped menu
  // would leave the page under it.
  const item = (await menu.locator('.perspective-more-item').first().boundingBox())!;
  const hit = await page.evaluate(
    ([x, y]) => document.elementFromPoint(x!, y!)?.closest('.perspective-more-menu') !== null,
    [item.x + item.width / 2, item.y + item.height / 2],
  );
  expect(hit).toBe(true);
});

test('on a phone the world is a strip: one row per region in flow order, under the three thirds', async ({ page }) => {
  await page.setViewportSize(PHONE);
  await mocks(page);
  await page.goto('/it');
  const strip = page.locator('section.strip');
  await expect(strip.locator('.strip-row')).toHaveCount(REGIONS.regions.length);
  // The SVG world is not drawn at all — not shrunk, not hidden.
  await expect(page.locator('section.yard svg')).toHaveCount(0);

  const thirds = strip.locator('.strip-third');
  expect(await thirds.evaluateAll((els) => els.map((el) => el.getAttribute('data-third')))).toEqual(
    THIRDS.map((t) => t.third),
  );
  await expect(strip.locator('.strip-third-label')).toHaveText(['Queue management', 'Actors building', 'Delivery']);
  for (const t of THIRDS) {
    const rows = strip.locator(`.strip-third[data-third="${t.third}"] .strip-row`);
    expect(await rows.evaluateAll((els) => els.map((el) => el.getAttribute('data-region')))).toEqual([...t.regions]);
  }

  // Each row: the state, the KPI in its unit, and the rail in.
  const receiving = strip.locator('.strip-row[data-region="receiving"]');
  await expect(receiving).toHaveAttribute('data-state', 'attention');
  await expect(receiving.locator('.strip-state')).toHaveText('attention for 36h');
  await expect(receiving.locator('.strip-kpi')).toHaveText('oldest untriaged 5 days');
  await expect(receiving.locator('.strip-verdict')).toHaveText(['oldest 5d > the 3-day triage band']);
  await expect(receiving.locator('.strip-rail')).toHaveText('no rail in — work arrives from outside the map');

  // Unread stays unread: the rail in and the region's own count.
  const marshalling = strip.locator('.strip-row[data-region="marshalling"]');
  await expect(marshalling).toHaveAttribute('data-state', 'troubled');
  await expect(marshalling.locator('.strip-kpi')).toHaveText('no reading');
  await expect(marshalling.locator('.strip-verdict')).toHaveText(['the station registry could not be read']);
  await expect(marshalling.locator('.strip-rail')).toHaveText('from receiving · waiting: no reading · rate: no reading');

  const track = strip.locator('.strip-row[data-region="track"]');
  await expect(track.locator('.strip-kpi')).toHaveText('track kpi 1 thing');
  await expect(track.locator('.strip-rail')).toHaveText('from dock · 3 waiting · 24 /day');
  await expect(track.locator('.strip-rail')).toHaveAttribute('data-state', 'troubled');

  // The garage is fed by two rails, and carries both.
  await expect(strip.locator('.strip-row[data-region="garage"] .strip-rail')).toHaveText([
    'from gates · nothing waiting · 2 /day',
    'from track · nothing waiting · 2 /day',
  ]);

  // A row is a door: the tap selects the region (design e765b3fc, car
  // N1), the strip stays, and the dock's panel opens under it — inside
  // the 16px gutters, with no side scroll.
  await strip.locator('.strip-row[data-region="dock"]').click();
  await expect(page).toHaveURL(/\/it\?at=dock$/);
  await expect(strip).toBeVisible();
  const panel = page.locator('section[data-map-panel]');
  await expect(panel).toHaveAttribute('data-selection', 'dock');
  const box = (await panel.boundingBox())!;
  expect(box.x).toBeGreaterThanOrEqual(16);
  expect(box.x + box.width).toBeLessThanOrEqual(PHONE.width - 16);
  const after = await noSideScroll(page);
  expect(after.scroll).toBeLessThanOrEqual(after.client);
});

test('a borders read that fails leaves every row’s rail unread, never empty', async ({ page }) => {
  await page.setViewportSize(PHONE);
  await mocks(page);
  await page.route(YARD_BORDERS, (r) =>
    r.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify('down') }),
  );
  await page.goto('/it');
  const rails = page.locator('section.strip .strip-rail');
  await expect(rails).toHaveCount(REGIONS.regions.length);
  expect(new Set(await rails.allTextContents())).toEqual(new Set(['rail in: no reading']));
});

test('at desktop width the world is the SVG, and no strip is drawn', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await mocks(page);
  await page.goto('/it');
  await expect(page.locator('section.yard svg')).toHaveCount(1);
  await expect(page.locator('section.strip')).toHaveCount(0);
  // The sidebar still stands beside the page.
  const sidebar = (await page.locator('.shell-sidebar').boundingBox())!;
  expect(sidebar.width).toBe(200);
});
