// /ux/people's Status buttons are built from the (employee, status)
// Classes, plus Unknown when a row has no status (backlog 01c268d6;
// page audit 0c0265a3 GAP 7, 2026-09-23). The hand-written group was
// Active / On leave / All while `terminated` is one of the three live
// status Classes, so a terminated row — and a null-status row, which
// renders as "unknown" — was reachable only under All and no button
// counted it. The pure bucketing is pinned in src/people/utils.test.ts;
// this spec pins the page wiring: the registry's buttons render,
// counted, and each one shows exactly its rows.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { DEPARTMENT_CLASSES, installSmokeMocks } from './_smokeMocks';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

function emp(id: string, status: string | null): Record<string, unknown> {
  return {
    id, name: id, email: `${id}@a`, role: 'ceo', department: 'it',
    hire_date: '2023-01-01', status, location: 'HQ',
    employment_type: 'full-time', skills: [], certifications: [],
  };
}

/// The three live status Classes, as 01-registries.sql seeds them.
const STATUS_CLASSES = [
  ['active', 'Active', 10], ['on-leave', 'On Leave', 20], ['terminated', 'Terminated', 30],
].map(([code, display_name, sort_order]) => ({
  subject_kind: 'employee', code, display_name, parent_code: null,
  member_attribute: 'status', metadata: {}, sort_order, retired_at: null,
}));

async function openRoster(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/people$/, (r) =>
    json(r, [emp('emp-001', 'active'), emp('emp-term', 'terminated'), emp('emp-null', null)]));
  await page.route(/\/api\/classes(\?|$)/, (r) => json(r, [
    { subject_kind: 'employee', code: 'ceo', display_name: 'CEO', parent_code: null, member_attribute: 'role', metadata: {}, sort_order: 1, retired_at: null },
    ...DEPARTMENT_CLASSES,
    ...STATUS_CLASSES,
  ]));
  await mountPage(page, '/ux/people');
}

const rosterIds = (page: Page) => page.locator('table.data-table tbody td.mono');

test.describe('/ux/people Status buttons come from the status Classes', () => {
  test('every status Class and the unknown rows have a counted button', async ({ page }) => {
    await openRoster(page);
    for (const name of ['Active (1)', 'On Leave (0)', 'Terminated (1)', 'Unknown (1)', 'All (3)']) {
      await expect(page.getByRole('button', { name, exact: true })).toBeVisible();
    }
  });

  test('Terminated and Unknown each show exactly their rows', async ({ page }) => {
    await openRoster(page);
    await page.getByRole('button', { name: 'Terminated (1)', exact: true }).click();
    await expect(rosterIds(page)).toHaveText(['emp-term']);
    await page.getByRole('button', { name: 'Unknown (1)', exact: true }).click();
    await expect(rosterIds(page)).toHaveText(['emp-null']);
  });
});
