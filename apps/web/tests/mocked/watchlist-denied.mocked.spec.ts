// /watchlist says a refusal in its own words, distinct from an empty
// watchlist (backlog 3f0cdca8; page audit 08b0c4f8 GAP 5, 2026-09-23).
//
// GET /api/people/accounts/risk-scores answered a role without broad
// account access `200 {accounts: []}`, and the page painted "No accounts
// match those filters." — it could not tell "you may not see this" from
// "nothing is at risk". The server now refuses 403 (pinned in
// crates/modules/boss-accounts/tests/risk_scores_denial.rs); this spec
// pins BOTH legs of the page: the 403 is named as a refusal and never as
// an empty list, and a true empty 200 is still the empty state and never
// a refusal.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const REFUSAL = 'Not shown to your role';
const EMPTY = 'No accounts match those filters.';

async function openWatchlist(page: Page, riskScores: (r: Route) => Promise<void>): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/people\/accounts\/risk-scores/, riskScores);
  await page.route(/\/api\/people\/accounts\?/, (r) =>
    r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ data: [], total: 0, limit: 1000, offset: 0 }),
    }));
  await mountPage(page, '/watchlist');
}

test.describe('/watchlist tells a refusal from an empty watchlist', () => {
  test('a 403 is named as a refusal, not painted as nothing at risk', async ({ page }) => {
    await openWatchlist(page, (r) =>
      r.fulfill({
        status: 403,
        contentType: 'text/plain',
        body: 'account risk scores are shown only to roles with broad account access',
      }));
    await expect(page.locator('h1').first()).toContainText(REFUSAL);
    await expect(page.getByText(/shown\s+only to roles with broad account access/)).toBeVisible();
    await expect(page.getByText(EMPTY)).toHaveCount(0);
    await expect(page.getByText(/accounts scored/)).toHaveCount(0);
  });

  test('an empty 200 is the empty state, not a refusal', async ({ page }) => {
    await openWatchlist(page, (r) =>
      r.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ accounts: [], total_scored: 0 }),
      }));
    await expect(page.getByText(EMPTY)).toBeVisible();
    await expect(page.getByText(/0 accounts scored/)).toBeVisible();
    await expect(page.getByText(REFUSAL)).toHaveCount(0);
  });
});
