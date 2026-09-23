// A FAILED READ IS HELD, NOT RETRIED (backlog 06038ed8).
//
// JobsListPage loads the Workflow registry for its Kind filter from a
// mount effect. Its guard — `kinds.length > 0 || kindsLoading` — was
// read INSIDE that effect, so both are effect dependencies: a failed
// read left kinds empty and reset kindsLoading, the effect re-ran, and
// it fetched again. Measured 2026-09-19 while counting the mocked
// suite's unanswered reads: 127 `/api/workflows` requests from ONE
// mount of /ux/jobs, the largest single entry in that run's miss
// summary. A failure is a state the page holds; only an operator
// gesture (focusing the Kind select) asks again.

import { expect, test, type Route } from '@playwright/test';
import { mountPage, settledReads } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

test.describe('the jobs list when the Workflow registry read fails', () => {
  test('reads /api/workflows once and holds the failure', async ({ page }) => {
    await installSmokeMocks(page);

    // Registered after the smoke mocks, so it wins: the registry read
    // fails the way an outage fails it.
    let reads = 0;
    await page.route(/\/api\/workflows$/, (r: Route) => {
      reads += 1;
      return r.fulfill({ status: 503, contentType: 'application/json', body: '{"error":"registry down"}' });
    });

    await mountPage(page, '/ux/jobs');
    // The list itself renders — the registry read is the Kind filter's,
    // not the page's. Wait for that read to ARRIVE, then give the old
    // retry loop a second to climb (it managed 127 reads in one mount).
    // A fixed second from mount let a late first read under gate load
    // read as Received 0 on a correct page (backlog 3571be7f).
    expect(await settledReads(page, () => reads, 1)).toBe(1);
  });
});
