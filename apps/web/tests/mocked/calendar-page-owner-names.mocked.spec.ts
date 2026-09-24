// /ux/calendar — naming the launch owners.
//
// Backlog 0268a829 (page audit 0ceeffa6, GAP 8): the calendar read the
// WHOLE roster to name the owners of the rows it shows, and a refusal
// or a network error was dropped (`if (r.ok)` with no else, `catch {
// // ignore }`), so the owners silently became raw ids. The fix reads
// one row per owner shown (`/api/people/{id}`) and says so when a name
// cannot load, in the words the exec page uses for the same owners.
// This spec routes every read the page makes, so it measures the page
// and not the smoke fixtures.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const PATH = '/ux/calendar';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const LAUNCH = {
  job_id: 'job-launch-1',
  title: 'Winter release',
  owner_id: 'emp-owner-1',
  subject_id: null,
  status: 'open',
  current_tier: 1,
  launch_date: '2026-10-01',
  launch_channel: 'email',
};

/// Every read the page makes, answered; `owner` decides the one under
/// test. The ROSTER names the same owner differently, so a page that
/// still reads the whole roster to name its owners shows the wrong name
/// and fails the first test rather than passing it by accident.
async function install(page: Page, owner: (r: Route) => Promise<void>): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/jobs\/launch-calendar\?/, (r) => json(r, { data: [LAUNCH] }));
  await page.route(/\/api\/people$/, (r) =>
    json(r, [{ id: 'emp-owner-1', name: 'Roster Copy' }]),
  );
  await page.route(/\/api\/people\/emp-owner-1$/, owner);
}

const row = (page: Page) => page.locator('li').filter({ hasText: 'Winter release' });

test.describe('/ux/calendar — naming the owners shown', () => {
  test('answered, the owner is named from its own row and nothing says a read failed', async ({ page }) => {
    await install(page, (r) => json(r, { id: 'emp-owner-1', name: 'Rhea Okafor' }));
    await mountPage(page, PATH);

    await expect(row(page)).toHaveCount(1);
    await expect(row(page).getByRole('link', { name: 'Rhea Okafor' })).toBeVisible();
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('refused, the page says owner names did not load instead of silently showing ids', async ({ page }) => {
    await install(page, (r) => json(r, { error: 'people down' }, 503));
    await mountPage(page, PATH);

    await expect(row(page)).toHaveCount(1);
    await expect(row(page).getByRole('link', { name: 'emp-owner-1' })).toBeVisible();
    await expect(page.locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      "Couldn't load owner names — /api/people/emp-owner-1: HTTP 503. Owners show as ids.",
    );
  });
});
