// /ux/support's Account Health tab, for an account with no tier
// (backlog ae7d1ce4, gap 8 of the /ux/support page audit 9876ef0d).
//
// The page typed an account's tier as 'platinum' | 'gold' | 'silver'
// while the one live account row answered "tier": null. That null is
// legal, not a bad row: boss-accounts carries `tier: Option<String>`,
// and 22-accounts.sql says "NULL stays legal (untiered until
// classified)". So the fix is the type, not the API. With the null
// hidden by the type, the Tier cell rendered as an empty string, and
// nothing a reader could see told an unclassified account apart from
// a broken cell. The accounts list already renders "untiered" through
// TierChip, and this spec holds the support page to the same thing.
//
// The sort leg pins the placement the packet asked to be CHOSEN
// rather than accidental: web-kit's compareSortValues puts no-data
// before every value ascending, so an untiered account leads the Tier
// column ascending and trails it descending.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const json = (r: Route, body: unknown): Promise<void> =>
  r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });

/// The live shape: identity-first, tier null until classified.
const ACCOUNTS = [
  { id: 'acct-gold', name: 'Gilded Taproom', tier: 'gold' },
  { id: 'acct-none', name: 'Newly Opened Bar', tier: null },
  { id: 'acct-silver', name: 'Silver Pub', tier: 'silver' },
  // Not enriched at all: the link falls back to the id, EntityLink's
  // convention, and the tie-break no longer throws on the null name.
  { id: 'acct-bare', name: null, tier: 'platinum' },
];

async function installReads(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/jobs\?department=support&limit=5000$/, (r) =>
    json(r, { data: [], total: 0, limit: 5000, offset: 0 }));
  await page.route(/\/api\/people\/accounts\?limit=1000$/, (r) =>
    json(r, { data: ACCOUNTS, total: ACCOUNTS.length, limit: 1000, offset: 0 }));
  await page.route(/\/api\/assets\?limit=1000$/, (r) =>
    json(r, { data: [], total: 0, limit: 1000, offset: 0 }));
}

/// [account, tier] per visible row, top to bottom. textContent, not
/// innerText: the chip capitalises by CSS, and the word is what we pin.
/// A tier's word is its (account, tier) Class label since d2c9e79f —
/// humanised from the code here, as the shell mocks carry no tier
/// Classes. The sort is still by code.
async function accountTierRows(page: Page): Promise<string[][]> {
  return page.locator('table.data-table tbody tr').evaluateAll((rows) =>
    rows.map((row) => [0, 1].map((i) => (row.children[i]?.textContent ?? '').trim())));
}

test('an account with no tier reads "untiered", not an empty cell', async ({ page }) => {
  await installReads(page);
  await mountPage(page, '/ux/support');
  await page.getByRole('tab', { name: 'Account Health' }).click();

  const rows = await accountTierRows(page);
  expect(rows).toContainEqual(['Newly Opened Bar', 'untiered']);
  expect(rows).toContainEqual(['Gilded Taproom', 'Gold']);
  expect(rows).toContainEqual(['acct-bare', 'Platinum']);
  for (const [, tier] of rows) expect(tier).not.toBe('');
});

test('the Tier sort places untiered accounts first ascending and last descending', async ({ page }) => {
  await installReads(page);
  await mountPage(page, '/ux/support');
  await page.getByRole('tab', { name: 'Account Health' }).click();

  const tierHeader = page.getByRole('columnheader', { name: /Tier/ });
  await tierHeader.click();
  await expect(tierHeader).toContainText('↑');
  expect((await accountTierRows(page)).map(([, t]) => t)).toEqual(['untiered', 'Gold', 'Platinum', 'Silver']);

  await tierHeader.click();
  await expect(tierHeader).toContainText('↓');
  expect((await accountTierRows(page)).map(([, t]) => t)).toEqual(['Silver', 'Platinum', 'Gold', 'untiered']);
});
