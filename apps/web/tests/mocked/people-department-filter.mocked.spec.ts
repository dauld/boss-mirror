// /ux/people's Department buttons follow the Status selection (backlog
// 1410f145; page audit 0c0265a3 GAP 6, 2026-09-23). They were built
// from the active rows whatever Status said, so under On leave or All
// the counts contradicted the table, a department whose people were
// all on leave had no button, and a row with no department could not
// be selected by department. The pure bucketing is pinned in
// src/people/utils.test.ts; this spec pins the page wiring.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { DEPARTMENT_CLASSES, installSmokeMocks } from './_smokeMocks';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

function emp(id: string, status: string, department: string | null): Record<string, unknown> {
  return {
    id, name: id, email: `${id}@a`, role: 'ceo', department,
    hire_date: '2023-01-01', status, location: 'HQ',
    employment_type: 'full-time', skills: [], certifications: [],
  };
}

const STATUS_CLASSES = [
  ['active', 'Active', 10], ['on-leave', 'On Leave', 20], ['terminated', 'Terminated', 30],
].map(([code, display_name, sort_order]) => ({
  subject_kind: 'employee', code, display_name, parent_code: null,
  member_attribute: 'status', metadata: {}, sort_order, retired_at: null,
}));

async function openRoster(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/people$/, (r) =>
    json(r, [
      emp('emp-it', 'active', 'it'),
      emp('emp-nodept', 'active', null),
      emp('emp-ops-1', 'on-leave', 'ops'),
      emp('emp-ops-2', 'on-leave', 'ops'),
    ]));
  await page.route(/\/api\/classes(\?|$)/, (r) => json(r, [
    { subject_kind: 'employee', code: 'ceo', display_name: 'CEO', parent_code: null, member_attribute: 'role', metadata: {}, sort_order: 1, retired_at: null },
    ...DEPARTMENT_CLASSES,
    ...STATUS_CLASSES,
  ]));
  await mountPage(page, '/ux/people');
}

const departments = (page: Page) =>
  page.locator('.filter-group').filter({
    has: page.locator('.filter-label', { hasText: /^Department$/ }),
  });
const rosterIds = (page: Page) => page.locator('table.data-table tbody td.mono');

test.describe('/ux/people Department buttons follow the Status selection', () => {
  test('under On Leave, a department of on-leave people has a button counting them', async ({ page }) => {
    await openRoster(page);
    await page.getByRole('button', { name: 'On Leave (2)', exact: true }).click();
    await expect(departments(page).getByRole('button')).toHaveText(['All (2)', 'Ops (2)']);
  });

  test('a row with no department is selectable by department', async ({ page }) => {
    await openRoster(page);
    await expect(departments(page).getByRole('button')).toHaveText(['All (2)', 'IT (1)', 'No department (1)']);
    await departments(page).getByRole('button', { name: 'No department (1)', exact: true }).click();
    await expect(rosterIds(page)).toHaveText(['emp-nodept']);
  });
});
