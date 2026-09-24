// /ux/exec — the launch panel is retired, and so is the read that fed it.
//
// The page is DEFERRED in both crawls (_routes.ts: its summaries need
// object-shaped fixtures), so this spec is the only thing that renders
// it. It pinned the people read (backlog 223ebcd6) — a read that only
// named the owners of the "Launches — next 30 days" panel. That panel
// read GET /api/jobs/launch-calendar, which nothing on the live
// instance or in the example tenant we keep produced (design 2ea444f5: 0 of 63
// active Workflow rows declared `launch_date`), so on an instance with
// the exec module on it always said "No marketing motions launching in
// the next 30 days." It retired with the second example tenant
// (backlog a8991c86), and the people read went with it. This spec now
// pins that the page renders its remaining cards without asking for
// either.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const PATH = '/ux/exec';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

/// Every read the page makes, answered, and every request it sends,
/// recorded.
async function install(page: Page): Promise<string[]> {
  const asked: string[] = [];
  page.on('request', (req) => asked.push(new URL(req.url()).pathname));
  await installSmokeMocks(page);
  await page.route(/\/api\/commerce\/summary$/, (r) => json(r, { error: 'none' }, 404));
  await page.route(/\/api\/jobs\/summary\?status=open$/, (r) => json(r, { counts: {}, total: 0 }));
  await page.route(/\/api\/workflows$/, (r) => json(r, []));
  await page.route(/\/api\/products$/, (r) => json(r, []));
  await page.route(/\/api\/ledger\/balance-sheet$/, (r) => json(r, { error: 'none' }, 404));
  return asked;
}

test.describe('/ux/exec — the launch panel is retired', () => {
  test('the page renders its cards and never asks for the launch calendar', async ({ page }) => {
    const asked = await install(page);
    await mountPage(page, PATH);

    await expect(
      page.locator('section.exec-card').filter({ hasText: 'Finished goods on hand' }),
    ).toBeVisible();
    await expect(page.locator('section.exec-card').filter({ hasText: 'Launches' })).toHaveCount(0);
    await expect(page.getByRole('link', { name: /Open full calendar/ })).toHaveCount(0);
    expect(asked.filter((p) => p === '/api/jobs/launch-calendar')).toEqual([]);
  });
});
