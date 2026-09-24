// /watchlist reads the accounts directory for exactly two things — each
// scored row's tier and its city — and until backlog 3122f14a it dropped
// a failed read of it on the floor ("names only"; page audit 08b0c4f8,
// GAP 7, 2026-09-23). With the directory dark every row had an unknown
// tier, so any Tier button but All emptied the table under "No accounts
// match those filters.", and a city search missed in silence. A capped
// read did the same to every scored account past the cap.
//
// These pin what the page owes instead: the failure is said, named by
// the read that failed; the Tier filter offers no choice it cannot
// answer; the search stops offering the city; and a capped read says
// how much of the directory it covers. The risk scores are the page's
// primary dataset and still render — a second read failing is not the
// page failing.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const TIER_CLASSES = [
  ['platinum', 'Platinum', 30], ['gold', 'Gold', 31], ['silver', 'Silver', 32],
].map(([code, display_name, sort_order]) => ({
  subject_kind: 'account', code, display_name, parent_code: null,
  member_attribute: 'tier', metadata: {}, sort_order, retired_at: null,
}));

function account(id: string, tier: string | null, city: string): Record<string, unknown> {
  return {
    id, name: id, director: null, city, state: null, tier,
    customer_since: null, territory_rep_id: null,
  };
}

function score(id: string, n: number): Record<string, unknown> {
  return {
    account_id: id, account_name: id, score: n, top_factor: 'no recent invoice',
    factors: { days_since_last_invoice: null, open_ticket_count: 0, has_active_contract: false, days_since_last_note: null },
  };
}

const ACCOUNTS = [
  account('acct-gold', 'gold', 'Portland'),
  account('acct-silver', 'silver', 'Bend'),
  account('acct-sponsor', null, 'Salem'),
];

async function openWatchlist(page: Page, directory: (r: Route) => Promise<void>): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/people\/accounts\/risk-scores/, (r) =>
    json(r, { accounts: ACCOUNTS.map((a, i) => score(String(a.id), 60 - i * 10)) }));
  await page.route(/\/api\/people\/accounts\?/, directory);
  await page.route(/\/api\/classes\?subject_kind=account/, (r) => json(r, TIER_CLASSES));
  await mountPage(page, '/watchlist');
}

const accountNames = (page: Page) => page.locator('table.risk-table tbody td:first-child');
const tierGroup = (page: Page) =>
  page.locator('.catalog-filters .filter-group').filter({ hasText: 'Tier' });

test.describe('/watchlist when the accounts directory read fails', () => {
  test('says so, keeps every scored row, and offers no tier it cannot answer', async ({ page }) => {
    await openWatchlist(page, (r) => json(r, { error: 'boom' }, 500));

    const alert = page.locator('.load-failed[role="alert"]');
    await expect(alert).toBeVisible();
    await expect(alert).toContainText('accounts directory');
    await expect(alert).toContainText('HTTP 500');

    // The scores are the page's own read and it answered.
    await expect(accountNames(page)).toHaveText(['acct-gold', 'acct-silver', 'acct-sponsor']);

    // No Tier button beside All: each one would empty the table.
    await expect(tierGroup(page).getByRole('button')).toHaveText(['All']);
    await expect(tierGroup(page)).toContainText('unknown');

    // The search no longer offers a city it has no copy of.
    const search = page.locator('.catalog-filters input');
    await expect(search).not.toHaveAttribute('placeholder', /city/i);
  });
});

test.describe('/watchlist when the accounts directory read is capped', () => {
  test('says how much of the directory the tier and city cover', async ({ page }) => {
    await openWatchlist(page, (r) =>
      json(r, { data: ACCOUNTS, total: 2000, limit: 1000, offset: 0 }));

    await expect(page.locator('.overflow-banner')).toContainText('Showing 3 of 2,000');
    // The read answered, so its tiers are still filters.
    await page.getByRole('button', { name: 'Gold (1)', exact: true }).click();
    await expect(accountNames(page)).toHaveText(['acct-gold']);
  });

  test('a complete read carries no banner and the city search works', async ({ page }) => {
    await openWatchlist(page, (r) =>
      json(r, { data: ACCOUNTS, total: ACCOUNTS.length, limit: 1000, offset: 0 }));

    await expect(accountNames(page)).toHaveCount(3);
    await expect(page.locator('.overflow-banner')).toHaveCount(0);
    await expect(page.locator('.load-failed')).toHaveCount(0);
    await page.locator('.catalog-filters input').fill('bend');
    await expect(accountNames(page)).toHaveText(['acct-silver']);
  });
});
