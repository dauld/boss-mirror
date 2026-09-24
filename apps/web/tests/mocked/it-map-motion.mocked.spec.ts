// THE IT MAP MOVES — behind its flight (design 31bade8f, car M2 on
// backlog d220022f; flights design c4c2a607). The arithmetic is pinned
// in world-motion.test.ts; this pins the page: with the flight OFF the
// world map is the map it was (no canvas, no motion bar, the old
// dashes); ON, one canvas lies over the SVG, the compression and what a
// token is are stated, the server's stillness reaches the rails and a
// held rail says for how long, reduced motion draws static density and
// runs no loop, the map greys past three missed reads, and a region
// crossing INTO troubled rings once. It reads meters the layer keeps on
// its canvas — tokens in flight, draw cost, rings rung — and never
// snapshots a pixel.

import { expect, test, type Page, type Route } from '@playwright/test';
import { BORDERS, TERRITORIES } from '../../src/it/yard/world';
import { FRAME_BUDGET_MS } from '../../src/it/yard/world-motion';
import { YARD_BORDERS, YARD_REGIONS, installSmokeMocks } from './_smokeMocks';

const FLIGHTS = /\/api\/flights\/mine(\?|$)/;
const NOW = '2026-09-24T12:00:00Z';

const json = (r: Route, b: unknown) => r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });

const regions = (gates: 'clear' | 'troubled' = 'clear') => ({
  window_hours: 24,
  now: NOW,
  regions: TERRITORIES.map(({ name }) => ({
    name, count: 1, state: name === 'gates' ? gates : 'clear', why: name === 'gates' && gates === 'troubled' ? 'a bay holds a corpse' : 'fine',
    trend: { metric: 'crossings', unit: 'per day', current: 4, previous: 4, samples: 4, previous_samples: 4 },
  })),
});

/** Every rail busy and flowing, with three the server judged otherwise:
 *  dock→track held since 08:48 (quiet past four mean gaps), gates→dock
 *  troubled, receiving→marshalling unread. */
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
    if (key === 'gates→dock') return { ...base, state: 'troubled', why: 'a bay holds a corpse' };
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

test('flight OFF: the world map is the map it was — no canvas, no motion bar, the old dashes', async ({ page }) => {
  await mocks(page, []);
  await page.goto('/it');
  const svg = page.locator('section.yard svg');
  await expect(svg.locator('.territory')).toHaveCount(TERRITORIES.length);
  await expect(page.locator('canvas.motion-layer')).toHaveCount(0);
  await expect(page.locator('[data-motion-bar]')).toHaveCount(0);
  await expect(page.locator('section.yard.motion')).toHaveCount(0);
  // The rails carry no motion reading and the rate is today's `N/d`.
  await expect(svg.locator('.rail[data-still]')).toHaveCount(0);
  await expect(svg.locator('[data-crossing="dock→track"] .rate')).toHaveText('6/d');
  await expect(svg.locator('.held')).toHaveCount(0);
  await expect(svg.locator('.traffic[data-traffic="shop-floor→gates"]')).toBeVisible();
});

test('flight ON: one canvas over the world, the compression stated, the server\'s stillness on the rails', async ({ page }) => {
  await mocks(page, ['it-map-motion']);
  await page.goto('/it');
  const canvas = page.locator('section.yard .stage canvas.motion-layer');
  await expect(canvas).toHaveCount(1);
  await expect(canvas).toHaveAttribute('data-motion', 'moving');
  // Stated, always (decision 2): ×600 by default, and what a token is.
  const bar = page.locator('[data-motion-bar]');
  await expect(bar).toContainText('×600 — 1 real hour = 6 s on screen');
  await expect(bar).toContainText("tokens replay each rail's measured 24h rate; they are not individual events");
  await expect(bar.getByRole('button', { name: '×600' })).toHaveAttribute('aria-pressed', 'true');

  // The server's judgement, read — never derived here.
  const svg = page.locator('section.yard svg');
  await expect(svg.locator('.rail[data-rail="dock→track"]')).toHaveAttribute('data-still', 'held');
  await expect(svg.locator('.rail[data-rail="gates→dock"]')).toHaveAttribute('data-still', 'troubled');
  await expect(svg.locator('.rail[data-rail="receiving→marshalling"]')).toHaveAttribute('data-still', 'unknown');
  await expect(svg.locator('.rail[data-rail="shop-floor→gates"]')).toHaveAttribute('data-still', 'moving');
  // A held rail says for how long, from the server's own held_since.
  await expect(svg.locator('[data-held="dock→track"]')).toContainText('held 3h 12m');
  await expect(svg.locator('[data-held]')).toHaveCount(1);
  // A busy rail at ×600: 480/day is 3.3 a second — one token each.
  await expect(svg.locator('[data-crossing="shop-floor→gates"] .rate')).toHaveText('480/d');
  // Decoration is gone: the dashes are hidden, only unknown's grey dotting stays.
  await expect(svg.locator('.traffic[data-traffic="shop-floor→gates"]')).toBeHidden();

  // Tokens are in flight, and the draw fits its budget.
  await expect.poll(async () => Number(await canvas.getAttribute('data-tokens'))).toBeGreaterThan(50);
  await expect.poll(async () => Number(await canvas.getAttribute('data-frame-ms') ?? 'NaN')).toBeLessThan(FRAME_BUDGET_MS);

  // ×3600 aggregates past 5 a second, and says so on the rail.
  await bar.getByRole('button', { name: '×3600' }).click();
  await expect(bar).toContainText('×3600 — 1 real hour = 1 s on screen');
  await expect(svg.locator('[data-crossing="shop-floor→gates"] .rate')).toHaveText('480/d ●=4');
  // Pause stops the loop.
  await bar.getByRole('button', { name: 'pause' }).click();
  await expect(canvas).toHaveAttribute('data-motion', 'paused');
  await expect(bar).toContainText('paused — nothing travels');

  // The crossing panel says whom the pile waits on and the rule it was judged by.
  await svg.locator('[data-crossing="dock→track"]').click();
  await expect(page.locator('[data-by-class]')).toContainText('3 in line for a machine · 2 on a person or the world · 1 cannot tell · 1 stuck');
  await expect(page.locator('[data-panel="dock→track"]')).toContainText('no — quiet 3h 12m > 4 mean gaps');
});

test('reduced motion: static density, no loop, and the stall still reads', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await mocks(page, ['it-map-motion']);
  await page.goto('/it');
  const canvas = page.locator('canvas.motion-layer');
  await expect(canvas).toHaveAttribute('data-motion', 'reduced');
  await expect(page.locator('[data-motion-bar] input[type="checkbox"]')).toBeChecked();
  await expect(page.locator('[data-motion-bar]')).toContainText('drawn as static density');
  await expect.poll(async () => canvas.getAttribute('data-tokens')).toBe('0');
  // Stillness has a word and a colour, not only motion (decision 9).
  await expect(page.locator('[data-held="dock→track"]')).toContainText('held 3h');
  // The page's own toggle turns motion back on.
  await page.locator('[data-motion-bar] input[type="checkbox"]').uncheck();
  await expect(canvas).toHaveAttribute('data-motion', 'moving');
});

test('past three missed reads the map greys and stops', async ({ page }) => {
  await page.clock.install({ time: new Date(NOW) });
  await installSmokeMocks(page);
  await page.route(FLIGHTS, (r) => json(r, { flights: ['it-map-motion'] }));
  await page.route(YARD_REGIONS, (r) => json(r, regions()));
  let reads = 0;
  // The first read answers; every later one never does.
  await page.route(YARD_BORDERS, (r) => (reads++ === 0 ? json(r, borders()) : undefined));
  await page.goto('/it');
  const canvas = page.locator('canvas.motion-layer');
  await expect(canvas).toHaveAttribute('data-motion', 'moving');
  await expect(page.locator('.motion-stale')).toHaveCount(0);
  // Paused, so the fake clock does not have to run 45 s of frames;
  // stale outranks paused.
  await page.getByRole('button', { name: 'pause' }).click();
  await page.clock.runFor(45_000);
  await expect(canvas).toHaveAttribute('data-motion', 'stale');
  await expect(page.locator('.stage.stale')).toHaveCount(1);
  await expect(page.locator('.motion-stale')).toContainText('not read for');
});

for (const reduced of [false, true]) {
  test(`a region crossing INTO troubled rings ${reduced ? 'never under reduced motion' : 'once, never again'}`, async ({ page }) => {
    if (reduced) await page.emulateMedia({ reducedMotion: 'reduce' });
    await ringsFor(page, reduced ? 0 : 1);
  });
}

async function ringsFor(page: Page, expected: number): Promise<void> {
  await page.clock.install({ time: new Date(NOW) });
  await installSmokeMocks(page);
  await page.route(FLIGHTS, (r) => json(r, { flights: ['it-map-motion'] }));
  let reads = 0;
  await page.route(YARD_REGIONS, (r) => json(r, regions(reads++ === 0 ? 'clear' : 'troubled')));
  await page.route(YARD_BORDERS, (r) => json(r, borders()));
  await page.goto('/it');
  const canvas = page.locator('canvas.motion-layer');
  await expect(canvas).toHaveAttribute('data-motion', expected === 0 ? 'reduced' : 'moving');
  // Tokens paused (a ring still draws): the fake clock need not run
  // twenty seconds of token frames.
  await page.getByRole('button', { name: 'pause' }).click();
  await page.clock.runFor(1_000);
  await expect(canvas).toHaveAttribute('data-rings', '0');
  // The next read finds gates troubled: one ring. A third finds it still troubled: none more.
  await page.clock.runFor(10_000);
  await expect(page.locator('.territory[data-region="gates"]')).toHaveAttribute('data-state', 'troubled');
  await page.clock.runFor(1_000);
  await expect(canvas).toHaveAttribute('data-rings', String(expected));
  await page.clock.runFor(11_000);
  await expect(canvas).toHaveAttribute('data-rings', String(expected));
}
