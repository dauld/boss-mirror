// /ux/people labels departments and roles from the Class registry's
// display_name (backlog 8a331c9b; page audit 0c0265a3 GAP 10,
// 2026-09-23). PeopleList never read the registry: every department and
// role went through humanizeClassCode, so the live `operations`
// department printed Operations where its Class says Operations / IT,
// and `platform-admin` printed Platform Admin for Platform admin. The
// lookup itself is pinned in src/people/types.test.ts; this spec pins
// the page wiring — the Role and Department cells, and the Department
// buttons, read the registry, and a code it lacks still humanizes.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

function emp(id: string, role: string, department: string): Record<string, unknown> {
  return {
    id, name: id, email: `${id}@a`, role, department,
    hire_date: '2023-01-01', status: 'active', location: 'HQ',
    employment_type: 'full-time', skills: [], certifications: [],
  };
}

function klass(member_attribute: string, code: string, display_name: string, sort_order: number) {
  return {
    subject_kind: 'employee', code, display_name, parent_code: null,
    member_attribute, metadata: {}, sort_order, retired_at: null,
  };
}

/// The live Classes the two live employees name (registry read
/// 2026-09-24), plus the Active status so the default Status button
/// admits them.
const CLASSES = [
  klass('department', 'operations', 'Operations / IT', 80),
  klass('role', 'platform-admin', 'Platform admin', 3),
  klass('status', 'active', 'Active', 10),
];

async function openRoster(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/people$/, (r) =>
    json(r, [
      emp('emp-david', 'platform-admin', 'operations'),
      // Neither code has a Class: the humanized code is the fallback.
      emp('emp-other', 'head-of-sales', 'field-ops'),
    ]));
  await page.route(/\/api\/classes(\?|$)/, (r) => json(r, CLASSES));
  await mountPage(page, '/ux/people');
}

const departments = (page: Page) =>
  page.locator('.filter-group').filter({
    has: page.locator('.filter-label', { hasText: /^Department$/ }),
  });
/// A roster row's Role and Department cells (the 3rd and 4th columns).
const roleAndDept = (page: Page, id: string) =>
  page
    .locator('table.data-table tbody tr')
    .filter({ has: page.locator('td.mono', { hasText: id }) })
    .locator('td:nth-child(3), td:nth-child(4)');

test.describe('/ux/people labels departments and roles from the Class registry', () => {
  test("the Role and Department cells print the registry's display_name", async ({ page }) => {
    await openRoster(page);
    await expect(roleAndDept(page, 'emp-david')).toHaveText(['Platform admin', 'Operations / IT']);
  });

  test('a code the registry lacks still prints its humanized code', async ({ page }) => {
    await openRoster(page);
    await expect(roleAndDept(page, 'emp-other')).toHaveText(['Head of Sales', 'Field Ops']);
  });

  test("the Department buttons print the registry's display_name", async ({ page }) => {
    await openRoster(page);
    await expect(departments(page).getByRole('button', { name: 'Operations / IT (1)', exact: true }))
      .toBeVisible();
    await expect(departments(page).getByRole('button', { name: 'Field Ops (1)', exact: true }))
      .toBeVisible();
  });
});
