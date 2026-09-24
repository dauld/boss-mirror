// /ux/service/schedule — naming the techs on the week grid.
//
// Backlog 1e73bd93. The page read the WHOLE employee roster to name the
// few techs on the grid, and a refusal or a network error was dropped
// (`if (r.ok)` with no else, `catch { // ignore }`), so the techs
// silently became raw ids. It now reads one row per tech shown through
// the shared reader (src/data/ownerNames.ts) and says so when a name
// cannot load. This spec routes every read the page makes, so it
// measures the page and not the smoke fixtures.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const PATH = '/ux/service/schedule';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

/// Every read the page makes, answered; `tech` decides the one under
/// test. The ROSTER names the same tech differently, so a page that
/// still reads the whole roster to name its techs shows the wrong name
/// and fails the first test rather than passing it by accident.
async function install(page: Page, tech: (r: Route) => Promise<void>): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/scheduling\/week-grid\?/, (r) =>
    json(r, { from: '2026-09-21T00:00:00Z', to: '2026-09-28T00:00:00Z', rows: [{ employee_id: 'emp-tech-1', blocks: [] }] }),
  );
  await page.route(/\/api\/people$/, (r) => json(r, [{ id: 'emp-tech-1', name: 'Roster Copy' }]));
  await page.route(/\/api\/people\/emp-tech-1$/, tech);
}

const techCell = (page: Page) => page.locator('tbody tr').first().locator('td').first();

test.describe('/ux/service/schedule — naming the techs shown', () => {
  test('answered, each tech is named from its own row and nothing says a read failed', async ({ page }) => {
    await install(page, (r) => json(r, { id: 'emp-tech-1', name: 'Rhea Okafor' }));
    await mountPage(page, PATH);

    await expect(techCell(page).getByRole('link', { name: 'Rhea Okafor' })).toBeVisible();
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('refused, the grid keeps the id and the page says the names did not load', async ({ page }) => {
    await install(page, (r) => json(r, { error: 'people down' }, 503));
    await mountPage(page, PATH);

    await expect(techCell(page).getByRole('link', { name: 'emp-tech-1' })).toBeVisible();
    await expect(page.locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      "Couldn't load tech names — /api/people/emp-tech-1: HTTP 503. Techs show as ids.",
    );
  });
});
