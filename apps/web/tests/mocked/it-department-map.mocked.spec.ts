// THE DEPARTMENT MAP — design e765b3fc, car N1 (decided by David
// 2026-09-25, on feedback 84cba7e2).
//
// "This is all going towards a design where the operating map sits at
// the top always and clicking stations or lines pulls up detail below.
// We can also probably remove the left nav bar items associated with
// navigating to different areas on the map and consolidate to maybe just
// Department Map?" (added_2026_09_25_david_3). This pins the car's two
// claims, as its plan names them:
//
//  - the IT sidebar is exactly seven rows, the first "Department Map"
//    — Receiving Yard, Marshalling Yard, Train Yard and Crew Board gone;
//  - `/it?at=gates` opens the gates' panel BELOW the map, and the map
//    stays on top: a click on a station selects it without tearing the
//    map down, where it used to swap the map for the region's own.
//
// The panel is a SHELL on this car: the station's name, its one number
// and its state, and the door to the floor page that still holds its
// detail. Car N2 fills it; car N3 retires the floor pages.

import { expect, test, type Page, type Route } from '@playwright/test';
import { TERRITORIES } from '../../src/it/yard/world';
import { DEPARTMENT_CLASSES, YARD_BORDERS, YARD_REGIONS, installSmokeMocks } from './_smokeMocks';

const json = (r: Route, b: unknown): Promise<void> =>
  r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });

const trend = { metric: 'gate duration', unit: 'minutes', current: 11, previous: 9, samples: 5, previous_samples: 4 };

/** One row per territory, clear and empty — except the gates, full at
 *  3 of 3 bays, and the dock with six cars parked. */
const REGIONS = {
  window_hours: 24,
  now: '2026-09-25T17:21:00Z',
  regions: TERRITORIES.map(({ name }) =>
    name === 'gates'
      ? { name, count: 3, bound: 3, bound_kind: 'capacity', unit: 'bays in use', state: 'clear', why: '3 of 3 bays in use', trend }
      : name === 'dock'
        ? { name, count: 6, unit: 'cars parked', state: 'clear', why: '6 cars parked', trend }
        : { name, count: 0, state: 'clear', why: 'nothing here', trend },
  ),
};

async function mocks(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(YARD_REGIONS, (r) => json(r, REGIONS));
  await page.route(YARD_BORDERS, (r) => json(r, { window_hours: 24, now: REGIONS.now, borders: [] }));
}

const MAP = 'section.yard svg';
const PANEL = 'section[data-map-panel]';

/** Signed in as the live roster's operator: platform-admin, whose Class
 *  row declares no `surfaces`, so every IT row is his to see. The
 *  sidebar paints no group until the session resolves a role. */
async function signIn(page: Page): Promise<void> {
  const emp = {
    id: 'emp-david', name: 'David', email: 'd@a', role: 'platform-admin', department: 'it',
    hire_date: '2023-01-01', status: 'active', location: 'HQ', employment_type: 'full-time',
    skills: [], certifications: [],
  };
  const roleRow = {
    subject_kind: 'employee', code: 'platform-admin', display_name: 'platform-admin', parent_code: null,
    member_attribute: 'role', metadata: {}, sort_order: 1, retired_at: null,
  };
  await page.route(/\/api\/people$/, (r) => json(r, [emp]));
  await page.route(/\/api\/session$/, (r) => json(r, { username: 'david', employee_id: emp.id, role: emp.role }));
  await page.route(/\/api\/classes(\?|$)/, (r) => json(r, [roleRow, ...DEPARTMENT_CLASSES]));
}

test('the IT sidebar is seven rows, and the first is the Department Map — the landing', async ({ page }) => {
  await mocks(page);
  await signIn(page);
  await page.goto('/it');
  const rows = page.locator('.shell-sidebar .shell-nav-group a.shell-nav-item');
  await expect(rows).toHaveText([
    'Department Map',
    'Operate',
    'Registry',
    'Design',
    'Codebase',
    'Estate',
    'Knowledge Base',
  ]);
  await expect(rows.first()).toHaveAttribute('href', '/it');
  // The landing lights its own row.
  await expect(rows.first()).toHaveClass(/shell-nav-item-active/);
  // And a selection lights the same row: it is the same page.
  await page.goto('/it?at=gates');
  await expect(page.locator('.shell-sidebar a.shell-nav-item-active')).toHaveText('Department Map');
  // The page is named what the row is named.
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Department Map');
});

test('?at=gates opens the gates panel below the map, with its one number and its state', async ({ page }) => {
  await mocks(page);
  await page.goto('/it?at=gates');
  const panel = page.locator(PANEL);
  await expect(panel).toHaveCount(1);
  await expect(panel).toHaveAttribute('data-selection', 'gates');
  // The panel's own title — the floor's cards below it carry headings too (car N2).
  await expect(panel.locator('.panel-title')).toHaveText('Gates');
  await expect(panel.locator('[data-figure]')).toHaveText('3 / 3 bays in use');
  await expect(panel).toHaveAttribute('data-state', 'clear');
  await expect(panel.locator('.panel-state')).toHaveText('clear');
  // BELOW the map, which is still drawn whole on top.
  await expect(page.locator(`${MAP} .territory`)).toHaveCount(TERRITORIES.length);
  const mapBox = (await page.locator(MAP).boundingBox())!;
  const panelBox = (await panel.boundingBox())!;
  expect(panelBox.y).toBeGreaterThanOrEqual(mapBox.y + mapBox.height);
  // The floor page still holds the detail on this car; the panel is its door.
  await expect(panel.locator('a[data-floor]')).toHaveAttribute('href', '/it/yard/gates');
});

test('a click on a station selects it and keeps the map on top; close deselects', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  await expect(page.locator(PANEL)).toHaveCount(0);
  const node = await page.locator(MAP).elementHandle();
  expect(node).not.toBeNull();

  await page.locator(`${MAP} .territory[data-region="dock"]`).click();
  await expect(page).toHaveURL(/\/it\?at=dock$/);
  await expect(page.locator(PANEL)).toHaveAttribute('data-selection', 'dock');
  await expect(page.locator(`${PANEL} [data-figure]`)).toHaveText('6 cars parked');
  // The SAME map node: selected on it, never swapped away from it.
  expect(await node!.evaluate((el) => el.isConnected)).toBe(true);

  // A second station moves the selection without a new map either.
  await page.locator(`${MAP} .territory[data-region="gates"]`).click();
  await expect(page).toHaveURL(/\/it\?at=gates$/);
  await expect(page.locator(PANEL)).toHaveAttribute('data-selection', 'gates');
  expect(await node!.evaluate((el) => el.isConnected)).toBe(true);

  // Back returns to the previous selection — it is on the history.
  await page.goBack();
  await expect(page).toHaveURL(/\/it\?at=dock$/);
  await expect(page.locator(PANEL)).toHaveAttribute('data-selection', 'dock');

  await page.locator(`${PANEL} a[data-close]`).click();
  await expect(page).toHaveURL(/\/it$/);
  await expect(page.locator(PANEL)).toHaveCount(0);
  expect(await node!.evaluate((el) => el.isConnected)).toBe(true);
});

test('a selection the map does not carry is said, never an empty panel', async ({ page }) => {
  await mocks(page);
  await page.goto('/it?at=gatez');
  const panel = page.locator(PANEL);
  await expect(panel).toHaveAttribute('data-selection', 'gatez');
  await expect(panel).toContainText('Nothing on this map is named “gatez”');
  await expect(panel.locator('a[data-floor]')).toHaveCount(0);
});
