// /ux/exec — the one read the executive overview used to swallow.
//
// The page is DEFERRED in both crawls (_routes.ts: its summaries need
// object-shaped fixtures), so nothing rendered it at all. Backlog
// 223ebcd6 found its people read on the swallowed-read list 7a7bfc88
// left unfinished: `pResp.ok ? await pResp.json() : []`, and a network
// error went to `// ignore`. The read only names launch owners, so the
// honest degrade is the ids it already fell back to — but said, not
// silent. This spec routes every read the page makes, so it measures
// the page and not the smoke fixtures.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const PATH = '/ux/exec';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const LAUNCH = {
  job_id: 'job-launch-1',
  title: 'Winter release',
  owner_id: 'emp-owner-1',
  launch_date: '2026-10-01',
  launch_channel: 'email',
};

/// Every read the page makes, answered; `people` decides the one under
/// test. The people route is registered after installSmokeMocks, so it
/// takes precedence over the shell's roster fixture.
async function install(page: Page, people: (r: Route) => Promise<void>): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/commerce\/summary$/, (r) => json(r, { error: 'none' }, 404));
  await page.route(/\/api\/jobs\/summary\?status=open$/, (r) => json(r, { counts: {}, total: 0 }));
  await page.route(/\/api\/workflows$/, (r) => json(r, []));
  await page.route(/\/api\/jobs\/launch-calendar\?/, (r) => json(r, { data: [LAUNCH] }));
  await page.route(/\/api\/products$/, (r) => json(r, []));
  await page.route(/\/api\/ledger\/balance-sheet$/, (r) => json(r, { error: 'none' }, 404));
  await page.route(/\/api\/people$/, people);
}

const launches = (page: Page) =>
  page.locator('section.exec-card').filter({ hasText: 'Launches — next 30 days' });

test.describe('/ux/exec — the people read', () => {
  test('answered, the launch owner is a name and nothing says a read failed', async ({ page }) => {
    await install(page, (r) => json(r, [{ id: 'emp-owner-1', name: 'Rhea Okafor' }]));
    await mountPage(page, PATH);

    await expect(launches(page).locator('tbody tr')).toHaveCount(1);
    await expect(launches(page).getByRole('link', { name: 'Rhea Okafor' })).toBeVisible();
    await expect(launches(page).locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('refused, the panel says owner names did not load instead of silently showing ids', async ({ page }) => {
    await install(page, (r) => json(r, { error: 'people down' }, 503));
    await mountPage(page, PATH);

    await expect(launches(page).locator('tbody tr')).toHaveCount(1);
    await expect(launches(page).locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      "Couldn't load owner names — /api/people: HTTP 503. Owners show as ids.",
    );
  });
});
