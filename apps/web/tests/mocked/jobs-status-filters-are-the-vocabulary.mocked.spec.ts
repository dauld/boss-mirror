// A STATUS FILTER OFFERS ONLY A STATUS A JOB CAN HOLD (backlog 3c3dc8f3).
//
// /ux/jobs offered Blocked and Pending sign-off as status buttons and
// as options of its Status select. No Job ever held either — 0 of
// 16,963 on 2026-09-24, and 0 of 97,909 `jobs.job.*` audit events — so
// each could only end at "No jobs match.", which reads as a filter
// result but was the vocabulary (page-audit 473f4f92, GAP 9). Both
// statuses are retired from `JobStatus`, and the API now refuses them;
// this holds the page to the same four words. /ux/service and
// /ux/sales mount the same page, so they are held by it too.

import { expect, test, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

test('the jobs list offers no status filter that no Job can hold', async ({ page }) => {
  await installSmokeMocks(page);
  // Registered after the smoke mocks, so it wins for the list read.
  await page.route(/\/api\/jobs\?/, (r: Route) =>
    r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ data: [], total: 0 }),
    }),
  );
  await mountPage(page, '/ux/jobs');

  const buttons = page.locator('.catalog-filters .filter-button');
  await expect(buttons).toHaveText(['Open', 'Closed', 'All']);

  const options = page.locator('label.job-filter:has(> span:text-is("Status")) option');
  await expect(options).toHaveText(['Open', 'Closed', 'All']);
});
