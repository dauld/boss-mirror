// /ux/people rows wore data-table-row-link — pointer, hover wash, focus
// ring — while only the BOSS ID cell was a link (backlog 96490f23, page
// audit 0c0265a3 GAP 8). The fix is the one mechanism every clickable row
// now uses, the web-kit rowLink action (decided on 2361ac45, 2026-09-24):
// the whole row opens the employee, from the mouse and the keyboard, and
// the ID link inside it still navigates exactly once.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { DEPARTMENT_CLASSES, installSmokeMocks } from './_smokeMocks';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

function emp(id: string, name: string): Record<string, unknown> {
  return {
    id, name, email: `${id}@a`, role: 'ceo', department: 'it',
    hire_date: '2023-01-01', status: 'active', location: 'HQ',
    employment_type: 'full-time', skills: [], certifications: [],
  };
}

async function openRoster(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/people$/, (r) =>
    json(r, [emp('emp-001', 'Ada Brewer'), emp('emp-002', 'Bo Cellar')]));
  await page.route(/\/api\/classes(\?|$)/, (r) => json(r, [
    { subject_kind: 'employee', code: 'ceo', display_name: 'CEO', parent_code: null, member_attribute: 'role', metadata: {}, sort_order: 1, retired_at: null },
    { subject_kind: 'employee', code: 'active', display_name: 'Active', parent_code: null, member_attribute: 'status', metadata: {}, sort_order: 10, retired_at: null },
    ...DEPARTMENT_CLASSES,
  ]));
  await mountPage(page, '/ux/people');
}

const adaRow = (page: Page) =>
  page.locator('table.data-table tbody tr', {
    has: page.getByRole('link', { name: 'emp-001', exact: true }),
  });

const path = (page: Page): string => new URL(page.url()).pathname;

test.describe('/ux/people — a row is a link', () => {
  test('a click on the row outside the ID cell opens that employee once', async ({ page }) => {
    await openRoster(page);
    const row = adaRow(page);
    await expect(row).toHaveClass(/data-table-row-link/);
    await expect(row).toHaveAttribute('role', 'link');
    await expect(row).toHaveAttribute('aria-label', 'Ada Brewer (emp-001)');
    const depth = await page.evaluate(() => window.history.length);
    await row.locator('td').nth(1).click();
    await expect.poll(() => path(page)).toBe('/ux/people/emp-001');
    expect(await page.evaluate(() => window.history.length)).toBe(depth + 1);
  });

  test('Enter on a focused row opens that employee', async ({ page }) => {
    await openRoster(page);
    await adaRow(page).focus();
    await page.keyboard.press('Enter');
    await expect.poll(() => path(page)).toBe('/ux/people/emp-001');
  });

  test('the ID link inside the row still navigates exactly once', async ({ page }) => {
    await openRoster(page);
    const depth = await page.evaluate(() => window.history.length);
    await page.getByRole('link', { name: 'emp-001', exact: true }).click();
    await expect.poll(() => path(page)).toBe('/ux/people/emp-001');
    expect(await page.evaluate(() => window.history.length)).toBe(depth + 1);
  });
});
