// /ux/people/<id> on a detail read that is not a whole employee row
// (backlog 548a1e8d, found by the /manual page-audit builder, run
// 0577ab10, 2026-09-23). The manual spec follows an employee shortcode
// to /ux/people/emp-001 and checks only the URL; the mocked floor
// answers that detail read with `[]`, and EmployeePage threw
// "TypeError: Cannot read properties of undefined (reading 'length')"
// reading `e.skills.length` off it — in every mocked run's console,
// under a passing spec. The same throw is what a live row missing a
// list would paint: a blank page and nothing for an operator to read.
//
// The fix is the read's, not the template's: a body the page cannot
// render is a FAILED read, said on the page's failure line and naming
// the field it lacked. An optional chain at the `.length` would have
// painted "no skills" over a record that never said so.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { FAILURE_MARKER } from './_routes';
import { installSmokeMocks } from './_smokeMocks';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const DETAIL = /\/api\/people\/emp-001$/;

/// A whole row, as boss-people serializes one: both lists always present.
const ROW = {
  id: 'emp-001', name: 'Demo CEO', email: 'ceo@demo', role: 'ceo',
  department: 'exec', skill_level: null, skills: ['brewing'], hire_date: '2020-01-01',
  location: 'HQ', manager_id: null, employment_type: 'full-time', status: 'active',
  certifications: [],
};

/// Mount the employee page on `body`, collecting every uncaught page error.
async function openEmployee(page: Page, body: unknown): Promise<string[]> {
  const errs: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));
  await installSmokeMocks(page);
  await page.route(DETAIL, (r) => json(r, body));
  await mountPage(page, '/ux/people/emp-001');
  return errs;
}

test.describe('/ux/people/<id> says a partial row failed rather than throwing', () => {
  test('control: a whole row renders its name and skills, with no failure line', async ({ page }) => {
    const errs = await openEmployee(page, ROW);
    await expect(page.locator('h1.detail-title')).toHaveText('Demo CEO');
    await expect(page.locator('.chips .chip')).toHaveText(['brewing']);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    expect(errs).toEqual([]);
  });

  for (const field of ['skills', 'certifications'] as const) {
    test(`a row that omits ${field} names it on the failure line and throws nothing`, async ({ page }) => {
      const { [field]: _omitted, ...partial } = ROW;
      const errs = await openEmployee(page, partial);
      const failed = page.locator(FAILURE_MARKER);
      await expect(failed).toHaveText(
        `Couldn't load this employee — /api/people/emp-001: the record carries no ${field} list`,
      );
      await expect(failed).toHaveAttribute('role', 'alert');
      expect(errs).toEqual([]);
    });
  }

  test('the mocked floor’s [] is not an employee, and is said so', async ({ page }) => {
    const errs = await openEmployee(page, []);
    await expect(page.locator(FAILURE_MARKER)).toHaveText(
      "Couldn't load this employee — /api/people/emp-001: the answer is not an employee record",
    );
    await expect(page.getByText('Employee not found')).toHaveCount(0);
    expect(errs).toEqual([]);
  });
});
