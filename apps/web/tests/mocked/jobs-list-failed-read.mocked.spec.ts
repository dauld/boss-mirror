// A FAILED LIST READ IS SAID ON THE SHARED MARKER, AND THE HEADER COUNTS
// NOTHING (backlog e98cabd0, swept by c3e4edcc).
//
// JobsListPage is three routes — /ux/jobs, /ux/service and /ux/sales —
// and when its one list read failed it painted "Couldn't load jobs: …"
// as `p.empty`, the class of "No jobs match.", under a header that still
// read "0 open": the failure in words, an empty queue in the header, and
// no FAILURE_MARKER, so the outage crawl held all three routes in
// SILENT. The line wears the marker now and the header says the count
// is unknown. The outage crawl pins the three routes; this pins the
// words, the header and the healthy control.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { FAILURE_MARKER } from './_routes';
import { installSmokeMocks } from './_smokeMocks';

const LIST = /\/api\/jobs\?/;
const subtitle = (page: Page) => page.locator('header.exec-header p');

test.describe('the jobs list when its list read fails', () => {
  for (const path of ['/ux/jobs', '/ux/service', '/ux/sales']) {
    test(`${path}: the failure line is the shared marker, and the header states no count`, async ({ page }) => {
      await installSmokeMocks(page);
      // Registered after the smoke mocks, so it wins for the list read.
      await page.route(LIST, (r: Route) =>
        r.fulfill({ status: 503, contentType: 'application/json', body: '{"error":"jobs down"}' }),
      );
      await mountPage(page, path);

      await expect(page.locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText("Couldn't load jobs: HTTP 503");
      await expect(page.getByText('No jobs match.')).toHaveCount(0);
      await expect(subtitle(page)).toHaveText('Job count unknown — the read failed');
      await expect(subtitle(page)).not.toContainText(/\d/);
    });
  }

  test('the control: an empty answer counts its zero and paints no marker', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(LIST, (r: Route) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ data: [], total: 0 }) }),
    );
    await mountPage(page, '/ux/jobs');

    await expect(page.getByText('No jobs match.')).toBeVisible();
    await expect(subtitle(page)).toHaveText('0 open');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });
});
