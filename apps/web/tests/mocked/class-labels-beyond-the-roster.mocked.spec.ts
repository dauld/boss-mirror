// Every surface that prints an employee's department or role labels it
// from the Class registry's display_name (backlog 8677728c, following
// 8a331c9b). 8a331c9b moved the /ux/people roster onto classLabel;
// five more surfaces still title-cased the code — the Hierarchy view's
// role line, the employee page's eyebrow, tagline, reporting chain and
// team table, HR's headcount table and QA's staffing table — so the
// live `operations` department printed Operations where its Class says
// Operations / IT. people-class-labels pins the roster; this spec pins
// the rest, and src/people/no-humanized-class-labels.test.ts refuses
// the next .svelte file that reaches for humanizeClassCode directly.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

function emp(
  id: string,
  role: string,
  department: string,
  manager_id: string | null,
): Record<string, unknown> {
  return {
    id, name: id, email: `${id}@a`, role, department, manager_id,
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

/// Each display_name differs from what humanizeClassCode makes of its
/// code (Operations, Platform Admin, Lab Tech), so a surface that still
/// humanizes prints the wrong text rather than a coincidentally right one.
const CLASSES = [
  klass('department', 'operations', 'Operations / IT', 80),
  klass('role', 'platform-admin', 'Platform admin', 3),
  klass('role', 'lab-tech', 'Laboratory technician', 20),
  klass('status', 'active', 'Active', 10),
];

/// A manager and one direct report, both in `operations`: the report's
/// page has a reporting chain, the manager's has a team table.
const ROSTER = [
  emp('emp-lead', 'platform-admin', 'operations', null),
  emp('emp-lab', 'lab-tech', 'operations', 'emp-lead'),
];

async function install(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/people$/, (r) => json(r, ROSTER));
  await page.route(/\/api\/people\/[^/?]+$/, (r) => {
    const id = decodeURIComponent(new URL(r.request().url()).pathname.split('/').pop() ?? '');
    const row = ROSTER.find((e) => e.id === id);
    return row ? json(r, row) : json(r, 'not found', 404);
  });
  await page.route(/\/api\/classes(\?|$)/, (r) => json(r, CLASSES));
}

test.describe('department and role labels come from the Class registry beyond the roster', () => {
  test("the Hierarchy view's role line prints the registry's display_name", async ({ page }) => {
    await install(page);
    await mountPage(page, '/ux/people');
    await page.getByRole('button', { name: 'Hierarchy', exact: true }).click();
    await expect(page.locator('.org-role')).toHaveText(['Platform admin', 'Laboratory technician']);
  });

  test("the employee page's eyebrow, tagline and reporting chain print display_names", async ({ page }) => {
    await install(page);
    await mountPage(page, '/ux/people/emp-lab');
    await expect(page.locator('.detail-eyebrow')).toContainText('Operations / IT');
    await expect(page.locator('.detail-tagline')).toHaveText('Laboratory technician · emp-lab@a');
    await expect(page.locator('ol.checklist li')).toContainText('Platform admin');
  });

  test("the employee page's team table prints each report's role display_name", async ({ page }) => {
    await install(page);
    await mountPage(page, '/ux/people/emp-lead');
    await expect(page.locator('table.data-table tbody td.prose-cell')).toHaveText(['Laboratory technician']);
  });

  test("HR's headcount table prints the department's display_name", async ({ page }) => {
    await install(page);
    await mountPage(page, '/ux/hr');
    await page.getByRole('tab', { name: 'Headcount', exact: true }).click();
    await expect(page.locator('table.data-table tbody tr').first().locator('td').first())
      .toHaveText('Operations / IT');
  });

  test("QA's staffing table prints the role's display_name", async ({ page }) => {
    await install(page);
    await mountPage(page, '/ux/qa');
    await page.getByRole('tab', { name: 'Batch QC', exact: true }).click();
    await expect(
      page.locator('table.data-table tbody tr').filter({ hasText: 'emp-lab' }).locator('td').nth(1),
    ).toHaveText('Laboratory technician');
  });
});
