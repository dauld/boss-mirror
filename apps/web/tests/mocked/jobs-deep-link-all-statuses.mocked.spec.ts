// A DEEP LINK CAN ASK FOR EVERY STATUS (backlog 03e198e5).
//
// EmployeePage links "View this employee's owned jobs" to
// `/ux/jobs?owner_id=…&status=` — the empty status meaning all, the
// same filter the page's own All button sets. The router read the
// parameter with a truthiness check, so the empty value was dropped
// and the page fell back to its default of open: the link showed open
// jobs only, and nothing tested a deep link asking for all statuses
// (page-audit 473f4f92, GAP 7). An ABSENT status still means open;
// the control below holds that half.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

/// Mount the jobs list at `search` and return the first list read's
/// query parameters — the request the page made for what it shows.
async function firstListRead(page: Page, search: string): Promise<URLSearchParams> {
  await installSmokeMocks(page);
  const reads: URLSearchParams[] = [];
  // Registered after the smoke mocks, so it wins for the list read.
  await page.route(/\/api\/jobs\?/, (r: Route) => {
    const params = new URL(r.request().url()).searchParams;
    if (params.get('owner_id') === 'emp-1') reads.push(params);
    return r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ data: [], total: 0 }),
    });
  });
  await mountPage(page, `/ux/jobs${search}`);
  await expect.poll(() => reads.length).toBeGreaterThanOrEqual(1);
  return reads[0]!;
}

test.describe('the jobs list opened from a deep link', () => {
  test('status= asks for every status and marks All active', async ({ page }) => {
    const params = await firstListRead(page, '?owner_id=emp-1&status=');
    expect(params.has('status')).toBe(false);
    await expect(page.locator('.filter-button-active')).toHaveText('All');
  });

  test('no status still defaults to open', async ({ page }) => {
    const params = await firstListRead(page, '?owner_id=emp-1');
    expect(params.get('status')).toBe('open');
    await expect(page.locator('.filter-button-active')).toHaveText('Open');
  });
});
