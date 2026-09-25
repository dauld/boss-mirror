// FULL, READ BY FILL ALONE (design e765b3fc §4a and Q2, decided by David
// 2026-09-25; car F1 on feedback 84cba7e2). "I want to be able to see
// that the Gates are full without reading the small text." A capacity
// region at its bound while its out-route moves arrives as the state
// `full`, and the transit map draws its station as a SOLID disk in the
// --map-full green with a heavier ring, while a clear station stays a
// hollow ring. Full is a good state, so it raises no alarm.
//
// The disk is read as the browser PAINTS it — the computed fill of the
// ring against the computed --map-full token — so a rule that names the
// token but loses the cascade still fails here.

import { expect, test, type Page, type Route } from '@playwright/test';
import { TERRITORIES } from '../../src/it/yard/world';
import { YARD_BORDERS, YARD_REGIONS, installSmokeMocks } from './_smokeMocks';

const FLIGHTS = /\/api\/flights\/mine(\?|$)/;

const json = (r: Route, b: unknown) => r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });

/** The gates and the track full, everything else clear. */
const regions = () => ({
  window_hours: 24,
  now: '2026-09-25T18:00:00Z',
  regions: TERRITORIES.map(({ name }) => {
    const full = name === 'gates' || name === 'track';
    return {
      name,
      count: full ? 3 : 1,
      bound: full ? 3 : null,
      bound_kind: full ? 'capacity' : null,
      unit: 'things',
      state: full ? 'full' : 'clear',
      why: full ? '3 of 3 bays in use — at its capacity, and moving: gates -> dock last crossed 4m ago' : 'fine',
      band: full
        ? { id: 'full', reads: 'its capacity, with its out-route moving', hold_minutes: 0, since: '2026-09-25T17:20:00Z', held_minutes: 40, held: '40m' }
        : null,
      trend: { metric: 'crossings', unit: 'per day', current: 4, previous: 4, samples: 4, previous_samples: 4 },
    };
  }),
});

async function mocks(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(FLIGHTS, (r) => json(r, { flights: ['it-map-transit'] }));
  await page.route(YARD_REGIONS, (r) => json(r, regions()));
  await page.route(YARD_BORDERS, (r) => json(r, { window_hours: 24, now: '2026-09-25T18:00:00Z', borders: [] }));
}

/** The ring's painted fill and stroke width, and the token it should wear. */
const paint = (page: Page, station: string) =>
  page.locator(`[data-station="${station}"] circle.ring`).evaluate((el) => {
    const probe = document.createElementNS('http://www.w3.org/2000/svg', 'circle');
    probe.style.fill = 'var(--map-full)';
    el.parentElement!.appendChild(probe);
    const token = getComputedStyle(probe).fill;
    probe.remove();
    const s = getComputedStyle(el);
    return { fill: s.fill, width: parseFloat(s.strokeWidth), token };
  });

test('a full station is a solid --map-full disk with a heavier ring; a clear one stays hollow', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const transit = page.locator('section.transit[data-transit]');
  await expect(transit).toBeVisible();
  await expect(transit.locator('[data-station="gates"]')).toHaveAttribute('data-state', 'full');
  await expect(transit.locator('[data-station="dock"]')).toHaveAttribute('data-state', 'clear');

  const gates = await paint(page, 'gates');
  const dock = await paint(page, 'dock');
  expect(gates.fill).toBe(gates.token);
  expect(dock.fill).not.toBe(dock.token);
  expect(gates.width).toBeGreaterThan(dock.width);

  // Good is not an alarm: every station is full or clear, so the board is quiet.
  await expect(transit.locator('aside.alarms .alarm')).toHaveCount(0);
  await expect(transit.locator('[data-alarms-clear]')).toBeVisible();
});
