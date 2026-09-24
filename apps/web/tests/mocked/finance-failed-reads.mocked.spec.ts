// /ux/finance — every read's failure line carries the failure marker,
// and no failed read wears an empty state (backlog 1a2b67c9, page
// audit 3f964c57 gap 12, measured against origin/main 642c0171).
//
// Two defects, one class. loadPeriods and loadEntriesForAccount folded
// a failed read into [], so a ledger outage painted "No periods yet."
// and "No entries for this account." — words that belong to a read
// that WORKED. And ten lines that were honest in prose ("Ledger
// unavailable.", "AP aging unavailable.", …) carried only `class=empty`,
// the same class as a truly empty list, so nothing — not the outage
// crawl, not a spec — could tell them from an empty. Only 2 of the
// page's 15 reads carried FAILURE_MARKER.
//
// This spec is deliberately narrow: the failure lines and their two
// empty twins. The page's controls, tabs and writes are e0732f75's
// spec (the page audit's test step), which also takes the route off
// the crawls' DEFERRED list.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const PATH = '/ux/finance';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

/// The ledger's refusal when its database is gone, as text: what the
/// failure line must carry is the status AND this reason.
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

const ENTRY = {
  id: 'ent-0001', fact_id: 'fact-0001', posted_on: '2026-09-02', memo: 'Opening cash',
  rule_version: 1, fact_kind: 'manual', fact_source_table: null, fact_source_id: null,
};

type Reads = Readonly<{
  summary?: unknown;
  apAging?: unknown;
  trialBalance?: unknown;
  periods?: unknown;
  entries?: unknown;
  entryDetail?: unknown;
}>;

/// The shell, then the page's reads. Every /api/ledger/ read not named
/// is refused, so a statement the test does not care about fails
/// rather than rendering the floor's `[]` as a statement. A read given
/// a function is routed to it; anything else is its 200 body.
async function install(page: Page, reads: Reads): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/commerce\/invoices/, (r) =>
    json(r, { data: [], total: 0, limit: 1000, offset: 0 }));
  await page.route(/\/api\/ledger\//, ledgerDown);
  const serve = (re: RegExp, body: unknown) =>
    body === undefined
      ? Promise.resolve()
      : page.route(re, (r) =>
          typeof body === 'function' ? (body as (r: Route) => Promise<void>)(r) : json(r, body));
  await serve(/\/api\/commerce\/summary$/, reads.summary ?? ledgerDown);
  await serve(/\/api\/inventory\/ap-aging$/, reads.apAging);
  await serve(/\/api\/ledger\/trial-balance/, reads.trialBalance);
  await serve(/\/api\/ledger\/periods$/, reads.periods);
  await serve(/\/api\/ledger\/entries\?/, reads.entries);
  await serve(/\/api\/ledger\/entries\/ent-0001$/, reads.entryDetail);
}

const failures = (page: Page) => page.locator(FAILURE_MARKER);

async function openTab(page: Page, name: string): Promise<void> {
  await page.getByRole('tab', { name, exact: true }).click();
}

test('every failed statement read paints the failure marker, as an alert', async ({ page }) => {
  await install(page, {});
  await mountPage(page, PATH);

  // Overview: the commerce summary read.
  await expect(failures(page).filter({ hasText: 'Finance summary unavailable.' })).toBeVisible();

  await openTab(page, 'Income statement');
  await expect(failures(page).filter({ hasText: 'Ledger unavailable.' })).toBeVisible();

  await openTab(page, 'Balance sheet');
  await expect(failures(page).filter({ hasText: 'Ledger unavailable.' })).toBeVisible();
  await expect(failures(page).filter({ hasText: 'Runoff projection unavailable.' })).toBeVisible();

  await openTab(page, 'Cash flow');
  await expect(failures(page).filter({ hasText: 'Ledger unavailable.' })).toBeVisible();
  await openTab(page, 'Direct');
  await expect(failures(page).filter({ hasText: 'Ledger unavailable.' })).toBeVisible();

  await openTab(page, 'Tax liability');
  await expect(failures(page).filter({ hasText: 'Tax liability unavailable.' })).toBeVisible();

  // Each is an alert, so assistive tech announces the outage as one.
  await expect(failures(page).first()).toHaveAttribute('role', 'alert');
});

test('a failed periods read says so, with the reason — never "No periods yet."', async ({ page }) => {
  await install(page, {});
  await mountPage(page, PATH);
  await openTab(page, 'Trial Balance');

  await expect(failures(page).filter({ hasText: 'Ledger unavailable.' })).toBeVisible();
  const periods = failures(page).filter({ hasText: 'periods' });
  await expect(periods).toBeVisible();
  await expect(periods).toContainText('HTTP 500');
  await expect(periods).toContainText('ledger down');
  await expect(periods).toHaveAttribute('role', 'alert');
  await expect(page.getByText('No periods yet.')).toHaveCount(0);
});

test('a failed AP aging read paints the failure marker', async ({ page }) => {
  await install(page, { summary: SUMMARY, apAging: (r: Route) =>
    r.fulfill({ status: 502, contentType: 'text/plain', body: 'inventory upstream unavailable' }) });
  await mountPage(page, PATH);

  await expect(failures(page).filter({ hasText: 'AP aging unavailable.' })).toBeVisible();
  await expect(page.getByText('Finance summary unavailable.')).toHaveCount(0);
});

test('a failed entries read says so, with the reason — never "No entries for this account."', async ({ page }) => {
  await install(page, { trialBalance: TRIAL_BALANCE, periods: [], entries: ledgerDown });
  await mountPage(page, PATH);
  await openTab(page, 'Trial Balance');
  await page.locator('tr.tb-row', { hasText: 'Cash' }).click();

  const entries = failures(page).filter({ hasText: 'entries' });
  await expect(entries).toBeVisible();
  await expect(entries).toContainText('1000');
  await expect(entries).toContainText('HTTP 500');
  await expect(entries).toContainText('ledger down');
  await expect(page.getByText('No entries for this account.')).toHaveCount(0);
  // The periods read worked and is truly empty: exactly one failure line.
  await expect(page.getByText('No periods yet.')).toBeVisible();
  await expect(failures(page)).toHaveCount(1);
});

test('a failed entry detail read paints the failure marker', async ({ page }) => {
  await install(page, {
    trialBalance: TRIAL_BALANCE, periods: [], entries: [ENTRY], entryDetail: ledgerDown,
  });
  await mountPage(page, PATH);
  await openTab(page, 'Trial Balance');
  await page.locator('tr.tb-row', { hasText: 'Cash' }).click();
  await page.locator('tr.tb-row', { hasText: 'Opening cash' }).click();

  await expect(failures(page).filter({ hasText: 'Entry unavailable.' })).toBeVisible();
});

test('a truly empty ledger still reads as empty, not as a failure', async ({ page }) => {
  await install(page, { trialBalance: TRIAL_BALANCE, periods: [], entries: [] });
  await mountPage(page, PATH);
  await openTab(page, 'Trial Balance');
  await page.locator('tr.tb-row', { hasText: 'Cash' }).click();

  await expect(page.getByText('No periods yet.')).toBeVisible();
  await expect(page.getByText('No entries for this account.')).toBeVisible();
  await expect(failures(page)).toHaveCount(0);
});
