// /ux/finance — the tab, and the entry or fact a link opened, live in
// the URL (backlog 2ab44d55, page audit 3f964c57 gap 7, measured
// against origin/main 642c0171).
//
// NewJournalEntryPage navigates to /ux/finance?entry=<id> after a post,
// and entity-href's ledger-entry and fact kinds target ?entry= and
// ?fact=. The page read neither, so the operator who had just posted an
// entry landed on Overview and never saw it; and the active tab lived
// in component state only, so back and reload returned to Overview.
// The pure read and write are pinned in src/finance/financeQuery.test.ts;
// this pins the page wiring end to end.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const PATH = '/ux/finance';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const ledgerDown = (r: Route): Promise<void> =>
  r.fulfill({ status: 500, contentType: 'text/plain', body: 'ledger down' });

const SUMMARY = {
  revenue_ttm: [], total_revenue_ttm_cents: 0, total_cogs_ttm_cents: 0,
  total_gross_margin_ttm_cents: 0, ar_aging: [], total_outstanding_cents: 0,
  total_invoice_count: 0, revenue_by_month: [], currency: 'USD',
};

const TRIAL_BALANCE = {
  as_of: '2026-09-23',
  rows: [{
    account_code: '1000', account_name: 'Cash', kind: 'asset', normal_side: 'debit',
    debit_total_cents: 125_000, credit_total_cents: 0, balance_cents: 125_000, currency: 'USD',
  }],
  total_debits_cents: 125_000, total_credits_cents: 125_000, balanced: true, currency: 'USD',
};

const ENTRY_DETAIL = {
  id: 'ent-0001', fact_id: 'fact-0001', posted_on: '2026-09-02', memo: 'Opening cash',
  rule_version: 1, fact_kind: 'manual', fact_payload: {}, fact_source_table: null,
  fact_source_id: null,
  lines: [
    { account_code: '1000', account_name: 'Cash', debit_cents: 125_000, credit_cents: 0,
      currency: 'USD', memo: null, sort_order: 0 },
    { account_code: '3000', account_name: 'Owner equity', debit_cents: 0, credit_cents: 125_000,
      currency: 'USD', memo: null, sort_order: 1 },
  ],
};

const ENTRY_SUMMARY = {
  id: 'ent-0001', fact_id: 'fact-0001', posted_on: '2026-09-02', memo: 'Opening cash',
  rule_version: 1, fact_kind: 'manual', fact_source_table: null, fact_source_id: null,
};

/// Every ledger read the test names is served; any other is refused, so
/// nothing renders from a floor it did not ask for. `factEntries` is the
/// answer to GET /api/ledger/entries?fact_id=…; the requests it saw are
/// returned so a test can say what the page asked.
async function install(
  page: Page,
  opts: Readonly<{ factEntries?: ((r: Route) => Promise<void>) | unknown }> = {},
): Promise<string[]> {
  await installSmokeMocks(page);
  await page.route(/\/api\/commerce\/invoices/, (r) =>
    json(r, { data: [], total: 0, limit: 1000, offset: 0 }));
  await page.route(/\/api\/commerce\/summary$/, (r) => json(r, SUMMARY));
  await page.route(/\/api\/ledger\//, ledgerDown);
  await page.route(/\/api\/ledger\/trial-balance/, (r) => json(r, TRIAL_BALANCE));
  await page.route(/\/api\/ledger\/periods$/, (r) => json(r, []));
  await page.route(/\/api\/ledger\/entries\/ent-0001$/, (r) => json(r, ENTRY_DETAIL));
  const factReads: string[] = [];
  await page.route(/\/api\/ledger\/entries\?fact_id=/, (r) => {
    factReads.push(r.request().url());
    const answer = opts.factEntries ?? [ENTRY_SUMMARY];
    return typeof answer === 'function' ? (answer as (r: Route) => Promise<void>)(r) : json(r, answer);
  });
  return factReads;
}

const tab = (page: Page, name: string) => page.getByRole('tab', { name, exact: true });
const linked = (page: Page) => page.locator('.tb-linked-entry');

test.describe('/ux/finance — a link to an entry or a fact opens it', () => {
  test('?entry= opens the Trial Balance on that entry, and mounting rewrites nothing', async ({ page }) => {
    await install(page);
    await mountPage(page, `${PATH}?entry=ent-0001`);

    await expect(tab(page, 'Trial Balance')).toHaveAttribute('aria-selected', 'true');
    await expect(linked(page).locator('.tb-entry-detail')).toContainText('Entry ent-0001');
    await expect(linked(page).locator('.tb-entry-lines tbody tr')).toHaveCount(2);
    await expect(linked(page)).toContainText('Owner equity');
    expect(new URL(page.url()).search).toBe('?entry=ent-0001');
  });

  test('?fact= resolves the fact to the entry it posted and opens that', async ({ page }) => {
    const factReads = await install(page);
    await mountPage(page, `${PATH}?fact=fact-0001`);

    await expect(tab(page, 'Trial Balance')).toHaveAttribute('aria-selected', 'true');
    await expect(linked(page).locator('.tb-entry-detail')).toContainText('Entry ent-0001');
    expect(factReads.map((u) => new URL(u).search)).toEqual(['?fact_id=fact-0001&limit=1']);
    expect(new URL(page.url()).search).toBe('?fact=fact-0001');
  });

  test('a fact that posted no entry says so, and a failed fact read is a failure, not that', async ({ page }) => {
    await install(page, { factEntries: [] });
    await mountPage(page, `${PATH}?fact=fact-0002`);
    await expect(linked(page)).toContainText('No journal entry for fact fact-0002.');
    await expect(linked(page).locator(FAILURE_MARKER)).toHaveCount(0);

    await install(page, { factEntries: ledgerDown });
    await page.reload();
    const failed = linked(page).locator(FAILURE_MARKER);
    await expect(failed).toContainText('HTTP 500');
    await expect(failed).toContainText('ledger down');
    await expect(linked(page)).not.toContainText('No journal entry');
  });

  test('closing the entry keeps the Trial Balance, and the URL says so', async ({ page }) => {
    await install(page);
    await mountPage(page, `${PATH}?entry=ent-0001`);
    await expect(linked(page).locator('.tb-entry-detail')).toBeVisible();

    await linked(page).getByRole('button', { name: 'Close' }).click();
    await expect(linked(page)).toHaveCount(0);
    await expect(tab(page, 'Trial Balance')).toHaveAttribute('aria-selected', 'true');
    await expect.poll(() => new URL(page.url()).search).toBe('?tab=trial-balance');
  });

  test('leaving the Trial Balance drops the entry from the URL', async ({ page }) => {
    await install(page);
    await mountPage(page, `${PATH}?entry=ent-0001`);
    await expect(linked(page).locator('.tb-entry-detail')).toBeVisible();

    await tab(page, 'Invoices').click();
    await expect.poll(() => new URL(page.url()).search).toBe('?tab=invoices');
    await tab(page, 'Trial Balance').click();
    await expect(linked(page)).toHaveCount(0);
  });
});

test.describe('/ux/finance — the tab survives reload and back', () => {
  test('a chosen tab rides in the URL without a history entry, and survives a reload', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH);
    await expect(tab(page, 'Overview')).toHaveAttribute('aria-selected', 'true');
    const depth = await page.evaluate(() => window.history.length);

    await tab(page, 'Cash flow').click();
    await expect.poll(() => new URL(page.url()).search).toBe('?tab=cash-flow');
    expect(await page.evaluate(() => window.history.length)).toBe(depth);

    await page.reload();
    await expect(tab(page, 'Cash flow')).toHaveAttribute('aria-selected', 'true');

    await tab(page, 'Overview').click();
    await expect.poll(() => new URL(page.url()).search).toBe('');
  });

  test('back from a page the finance page linked to returns to the tab it left', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH);
    await tab(page, 'Invoices').click();
    await expect.poll(() => new URL(page.url()).search).toBe('?tab=invoices');

    await page.getByRole('link', { name: '+ New journal entry' }).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(`${PATH}/journal-entries/new`);

    await page.goBack();
    await expect.poll(() => new URL(page.url()).search).toBe('?tab=invoices');
    await expect(tab(page, 'Invoices')).toHaveAttribute('aria-selected', 'true');
  });

  test('navigating to a finance link while on the page shows what the link names', async ({ page }) => {
    await install(page);
    await mountPage(page, `${PATH}?tab=invoices`);
    await expect(tab(page, 'Invoices')).toHaveAttribute('aria-selected', 'true');

    // The app's own navigate(): pushState, then popstate — what the
    // sidebar's Finance link and an EntityLink to an entry both do.
    await page.evaluate((to) => {
      window.history.pushState({}, '', to);
      window.dispatchEvent(new PopStateEvent('popstate'));
    }, `${PATH}?entry=ent-0001`);
    await expect(tab(page, 'Trial Balance')).toHaveAttribute('aria-selected', 'true');
    await expect(linked(page).locator('.tb-entry-detail')).toContainText('Entry ent-0001');

    await page.evaluate((to) => {
      window.history.pushState({}, '', to);
      window.dispatchEvent(new PopStateEvent('popstate'));
    }, PATH);
    await expect(tab(page, 'Overview')).toHaveAttribute('aria-selected', 'true');
    expect(new URL(page.url()).search).toBe('');
  });
});
