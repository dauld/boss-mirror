// /watchlist's Tier buttons come from the (account, tier) Classes, plus
// No tier when an account has none (backlog 1be37454; page audit
// 08b0c4f8 GAP 11, 2026-09-23). The page hard-coded Platinum / Gold /
// Silver, so a tier a tenant adds never appeared as a filter and the
// untiered account (acct-anonymous-sponsor, live) was reachable only
// under All. The pure bucketing is pinned in src/accounts/tiers.test.ts;
// this spec pins the page wiring with a classes read that carries a
// FOURTH tier the old trio could not know about.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

/// The three seeded tier Classes (01-registries.sql) and a tenant's
/// fourth, as `GET /api/classes?subject_kind=account` serves them.
const TIER_CLASSES = [
  ['platinum', 'Platinum', 30], ['gold', 'Gold', 31], ['silver', 'Silver', 32], ['bronze', 'Bronze', 33],
].map(([code, display_name, sort_order]) => ({
  subject_kind: 'account', code, display_name, parent_code: null,
  member_attribute: 'tier', metadata: {}, sort_order, retired_at: null,
}));

function account(id: string, tier: string | null): Record<string, unknown> {
  return {
    id, name: id, director: null, city: null, state: null, tier,
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
  account('acct-gold', 'gold'),
  account('acct-bronze', 'bronze'),
  account('acct-sponsor', null),
];

async function openWatchlist(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/people\/accounts\/risk-scores/, (r) =>
    json(r, { accounts: ACCOUNTS.map((a, i) => score(String(a.id), 60 - i * 10)) }));
  await page.route(/\/api\/people\/accounts\?/, (r) =>
    json(r, { data: ACCOUNTS, total: ACCOUNTS.length, limit: 1000, offset: 0 }));
  await page.route(/\/api\/classes\?subject_kind=account/, (r) => json(r, [
    { subject_kind: 'account', code: 'call', display_name: 'Call', parent_code: null, member_attribute: 'note-kind', metadata: {}, sort_order: 1, retired_at: null },
    ...TIER_CLASSES,
  ]));
  await mountPage(page, '/watchlist');
}

const accountNames = (page: Page) => page.locator('table.risk-table tbody td:first-child');

test.describe('/watchlist Tier buttons come from the tier Classes', () => {
  test('every tier Class, the fourth included, and No tier have a counted button', async ({ page }) => {
    await openWatchlist(page);
    const tiers = page.locator('.catalog-filters').filter({ hasText: 'Tier' });
    for (const name of ['Platinum (0)', 'Gold (1)', 'Silver (0)', 'Bronze (1)', 'No tier (1)']) {
      await expect(page.getByRole('button', { name, exact: true })).toBeVisible();
    }
    // A note-kind Class is an account Class too, and is not a tier.
    await expect(tiers.getByRole('button', { name: /^Call/ })).toHaveCount(0);
  });

  test('Bronze and No tier each show exactly their rows', async ({ page }) => {
    await openWatchlist(page);
    await page.getByRole('button', { name: 'Bronze (1)', exact: true }).click();
    await expect(accountNames(page)).toHaveText(['acct-bronze']);
    await page.getByRole('button', { name: 'No tier (1)', exact: true }).click();
    await expect(accountNames(page)).toHaveText(['acct-sponsor']);
  });
});
