// A signed-in operator whose people row cannot be read is not an
// unrecognized login.
//
// Backlog b4f68a65 (measured 2026-09-26 on origin/main 17b7e526): the
// shell fetched the whole /api/people roster on every load to find the
// viewer, and a failed read left an empty map — so a real, signed-in
// operator rendered as "Signed in as david, but no matching employee in
// the roster" whenever the people service blinked, and nothing on screen
// said a read had failed. The shell now reads the viewer's ONE row, and a
// read that does not answer is its own state, rendered as a failed read
// in the words every page uses for one.

import { expect, test, type Page, type Route } from '@playwright/test';
import { FAILURE_MARKER } from './_routes';

const json = (r: Route, b: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const EMP = {
  id: 'emp-001', name: 'David', email: 'david@x', role: 'platform-admin',
  department: 'it', hire_date: '2020-01-01', status: 'active',
  location: 'HQ', employment_type: 'full-time', skills: [], certifications: [],
};

async function signedIn(page: Page, row: (r: Route) => Promise<void>): Promise<string[]> {
  // Catch-all FIRST: Playwright matches in reverse registration order.
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: 'platform-admin' }));
  await page.route(/\/api\/people\/emp-001$/, row);
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(/\/api\/jobs\/summary(\?|$)/, (r) => json(r, { counts: {}, total: 0 }));
  const roster: string[] = [];
  page.on('request', (req) => {
    const path = new URL(req.url()).pathname;
    if (path === '/api/people') roster.push(path);
  });
  return roster;
}

test('a refused people row says the record could not be loaded — never "no matching employee"', async ({ page }) => {
  await signedIn(page, (r) => json(r, 'people store down', 503));
  await page.goto('/');

  const home = page.locator(`.theme-exec ${FAILURE_MARKER}`);
  await expect(home).toHaveAttribute('role', 'alert');
  await expect(home).toHaveText(
    "Signed in as david, but couldn't load your employee record — /api/people/emp-001: HTTP 503.",
  );
  // The chrome says it too, on every page, instead of rendering nobody.
  await expect(page.locator(`.shell-sidebar-footer ${FAILURE_MARKER}`)).toHaveText(
    "Couldn't load your employee record — /api/people/emp-001: HTTP 503.",
  );
  await expect(page.getByText(/matching employee in the roster/)).toHaveCount(0);
});

test('control: an answered row is the operator, and the shell reads no roster for it', async ({ page }) => {
  const roster = await signedIn(page, (r) => json(r, EMP));
  await page.goto('/');

  await expect(page.locator('.shell-user-name')).toHaveText('David');
  await expect(page.locator('.shell-sidebar-footer .load-failed')).toHaveCount(0);
  expect(roster).toEqual([]);
});
