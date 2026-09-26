// /ux/finance — the page's own spec: every control it renders, pinned
// as it behaves on main (page audit 3f964c57, its `test` step; backlog
// e0732f75, gap 13 of that audit).
//
// WHY THIS FILE EXISTS. The route sat in DEFERRED (_routes.ts) for want
// of object-shaped statement fixtures, and no spec clicked any of its
// controls: the audit's measure step counted 52 buttons, 10 links, 8
// inputs and 4 writes, and none was exercised. The fixtures are now in
// _smokeMocks.ts (COMMERCE_SUMMARY, AP_AGING, LEDGER_STATEMENTS), so the
// three crawls open the route; this file carries what a crawl cannot —
// what each control PROMISES, checked against what it does.
//
// Four narrower specs landed with the audit's other gaps and are not
// repeated here:
//   finance-department-jobs     the department's In / Working / Out panel (4d4dc204)
//   finance-failed-reads        each statement's failure line and its empty twin (1a2b67c9)
//   finance-po-approvals-refused  a refused, unreachable and accepted Approve (1acaa10e)
//   finance-url-state           the tab and ?entry= / ?fact= in the URL, back and reload (2ab44d55)
//
// THE AUDIT'S OPEN GAPS are named on the line that pins the behaviour
// each one would change, so the car that fixes one finds its line here
// red and rewrites it:
//   801b6a7a  the headline, AR aging, gross margin and Invoices read boss-commerce, which
//             no Algedonic protocol writes (waits on design 72ccb3b2; a7bad6f0, payables,
//             was folded into it)
//   b7263ac5  the nightly ledger recognize / replay runs declare no department, so the
//             page's department panel cannot show them
//   e0b754d3  a ledger entry sourced from a packet renders "jobs · <id>", not a link
//   975c228f  Lock posts locked_by 'operator' and Reverse posts created_by null
//   e536e91a  Lock and Unlock are one confirm and one POST: no protocol, no reason recorded
//   c8b71886  the Tax liability tab describes the brewery simulator's generator and
//             hard-codes the account descriptions

import { readFile } from 'node:fs/promises';
import { expect, test, type Download, type Page, type Request, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { parseRoute } from '../../src/router';
import { FINANCE_TABS } from '../../src/finance/financeQuery';

const PATH = ROUTE_CATALOG.finance.path;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

/// A refusal as the services send one: a status and a reason in text.
const refuse = (status: number, reason: string) => (r: Route): Promise<void> =>
  r.fulfill({ status, contentType: 'text/plain', body: reason });

// ---------------------------------------------------------------------------
// Fixtures — Algedonic's own shapes where the audit measured them (one
// sponsorship posted $1.00 to 4200 from packet 49cd9cc9; one period,
// 2026-09, open), and the commerce/inventory shapes the page reads.
// ---------------------------------------------------------------------------

const SUMMARY = {
  revenue_ttm: [
    { category: 'sponsorship', revenue_cents: 1_234_500, cogs_cents: 234_500, gross_margin_cents: 1_000_000, margin_pct: 81 },
  ],
  total_revenue_ttm_cents: 1_234_500, total_cogs_ttm_cents: 234_500, total_gross_margin_ttm_cents: 1_000_000,
  ar_aging: [
    { label: 'Current', count: 2, total_cents: 30_000 },
    { label: '1-30 days', count: 1, total_cents: 12_000 },
  ],
  total_outstanding_cents: 42_000, total_invoice_count: 4, revenue_by_month: [], currency: 'USD',
};

const AP_AGING = {
  buckets: [{ label: 'Current', count: 3, total_cents: 45_000 }],
  total_outstanding_cents: 45_000, total_invoice_count: 3, currency: 'USD',
};

const line = (id: string, description: string) => ({
  id: `${id}-l1`, invoice_id: id, revenue_category: 'sponsorship', amount_cents: 0, currency: 'USD', description, ref_id: null,
});
const invoice = (
  id: string, account_id: string, status: string, payment_method: string, amount_cents: number,
  issued_on: string, due_on: string, paid_on: string | null, description: string,
) => ({
  id, account_id, status, payment_method, amount_cents, currency: 'USD', issued_on, due_on, paid_on,
  tax_cents: status === 'paid' ? 800 : 0, tax_jurisdiction: status === 'paid' ? 'US-CA' : null,
  line_items: [line(id, description)],
});
const INVOICES = [
  invoice('inv-0001', 'acct-1', 'paid', 'card', 10_000, '2026-09-01', '2026-09-30', '2026-09-05', 'Gold sponsor'),
  invoice('inv-0002', 'acct-2', 'outstanding', 'ach', 20_000, '2026-09-10', '2026-10-10', null, 'Hosting, September'),
  invoice('inv-0003', 'acct-1', 'past-due', 'wire', 12_000, '2026-08-01', '2026-08-31', null, 'Hosting, August'),
  invoice('inv-0004', 'acct-2', 'written-off', 'check', 5_000, '2026-07-01', '2026-07-31', null, 'Support, July'),
];
const ACCOUNTS = [
  { id: 'acct-1', name: 'Hop Collective' },
  { id: 'acct-2', name: 'Barley Works' },
];

const po = (id: string, status: string) => ({
  id, vendor: 'vendor-0001', status, placed_on: '2026-09-20', expected_on: '2026-09-30',
  lines: [{ part_sku: 'HOPS-CASCADE', qty: 4, unit_cost_cents: 2_500, currency: 'USD' }],
});

const INCOME_STATEMENT = {
  from: '2026-01-01', to: '2026-09-23',
  revenue: [{ account_code: '4200', account_name: 'Sponsorship revenue', amount_cents: 100 }],
  total_revenue_cents: 100, cogs: [], total_cogs_cents: 0, gross_profit_cents: 100,
  operating_expenses: [{ account_code: '6250', account_name: 'Infrastructure (cloud, domains)', amount_cents: 40 }],
  total_operating_expenses_cents: 40, net_income_cents: 60, currency: 'USD',
};

const BALANCE_SHEET = {
  as_of: '2026-09-23',
  assets: [{ account_code: '1010', account_name: 'Cash in Transit', amount_cents: 100 }], total_assets_cents: 100,
  liabilities: [], total_liabilities_cents: 0,
  equity: [{ account_code: '3200', account_name: 'Retained earnings', amount_cents: 100 }], total_equity_cents: 100,
  imbalance_cents: 0, balanced: true, currency: 'USD',
};

const RUNOFF = {
  as_of: '2026-09-23', horizon_months: 12, deferred_account_balance_cents: 3_000, schedules_remaining_cents: 2_500,
  drift_cents: 500, months: [{ month: '2026-10-01', amount_cents: 2_000 }], beyond_horizon_cents: 500, currency: 'USD',
};

const CASH_FLOW = {
  from: '2026-01-01', to: '2026-09-23', net_income_cents: 100,
  operating_activities: [{ account_code: '1200', account_name: 'Accounts receivable', amount_cents: -100 }],
  working_capital_adjustments: [], non_cash_adjustments: [], cash_from_operations_cents: 0,
  investing_activities: [], cash_from_investing_cents: 0, financing_activities: [], cash_from_financing_cents: 0,
  net_change_in_cash_cents: 0, cash_start_cents: 0, cash_end_cents: 100,
  reconciliation_gap_cents: 100, reconciled: false, currency: 'USD',
};

const CASH_FLOW_DIRECT = {
  from: '2026-01-01', to: '2026-09-23', method: 'direct', cash_in_from_customers_cents: 100,
  cash_out_to_vendors_cents: 40, cash_out_to_employees_cents: 0, cash_out_to_authorities_cents: 0,
  net_change_in_cash_cents: 60, gl_cash_pool_delta_cents: 60, gl_cash_1000_delta_cents: 0,
  reconciliation_gap_cents: 0, reconciled: true, currency: 'USD',
};

const tbRow = (code: string, name: string, kind: string, debit: number, credit: number) => ({
  account_code: code, account_name: name, kind, normal_side: kind === 'asset' ? 'debit' : 'credit',
  debit_total_cents: debit, credit_total_cents: credit, balance_cents: debit + credit, currency: 'USD',
});
const TRIAL_BALANCE = {
  as_of: '2026-09-23',
  rows: [
    tbRow('1000', 'Cash', 'asset', 0, 0),
    tbRow('1010', 'Cash in Transit', 'asset', 100, 0),
    tbRow('4200', 'Sponsorship revenue', 'revenue', 0, 100),
  ],
  total_debits_cents: 100, total_credits_cents: 100, balanced: true, currency: 'USD',
};

const PERIOD_OPEN = {
  id: 'per-2026-09', kind: 'month', starts_on: '2026-09-01', ends_on: '2026-09-30', status: 'open',
  locked_at: null, locked_by: null, locked_rule_version: null, locked_checksum: null,
  entry_count: 1, total_debits: 100, total_credits: 100,
};
const PERIOD_LOCKED = {
  ...PERIOD_OPEN, status: 'locked', locked_at: '2026-09-24T00:00:00Z', locked_by: 'operator',
  locked_rule_version: 1, locked_checksum: 'sha256:abcdef0123456789abcdef',
};

/// The ledger's only entry on the live instance (73acb2e8), sourced from
/// a packet; and one sourced from an invoice, the only kind that links.
const PACKET_ID = '49cd9cc9-0000-0000-0000-000000000001';
const ENTRIES = [
  { id: 'ent-0001', fact_id: 'fact-0001', posted_on: '2026-09-17', memo: 'Sponsorship received', rule_version: 1,
    fact_kind: 'finance.sponsorship.received', fact_source_table: 'jobs', fact_source_id: PACKET_ID },
  { id: 'ent-0002', fact_id: 'fact-0002', posted_on: '2026-09-18', memo: 'Invoice paid', rule_version: 1,
    fact_kind: 'finance.invoice.paid', fact_source_table: 'invoices', fact_source_id: 'inv-0001' },
];
const ENTRY_DETAIL = {
  ...ENTRIES[0], fact_payload: { amount_cents: 100, packet: PACKET_ID },
  lines: [
    { account_code: '1010', account_name: 'Cash in Transit', debit_cents: 100, credit_cents: 0, currency: 'USD', memo: null, sort_order: 0 },
    { account_code: '4200', account_name: 'Sponsorship revenue', debit_cents: 0, credit_cents: 100, currency: 'USD', memo: 'sponsor', sort_order: 1 },
  ],
};

const TAX_FILING = {
  id: 'tf-0001', kind: 'sales', jurisdiction: 'US-CA', period_start: '2026-08-01', period_end: '2026-08-31',
  due_on: '2026-09-20', filed_on: null, amount_cents: 800, liability_account: '2300', status: 'accrued', provider: 'manual',
};
const TAX_LIABILITY = {
  as_of: '2026-09-23',
  liabilities: [
    { account_code: '2150', account_name: 'Payroll liabilities', balance_cents: 0 },
    { account_code: '2300', account_name: 'Sales tax payable', balance_cents: 800 },
    { account_code: '2310', account_name: 'Income tax payable', balance_cents: 0 },
  ],
  accrued_filings: [TAX_FILING], next_due: TAX_FILING, currency: 'USD',
};

// ---------------------------------------------------------------------------
// The backend: the smoke floor, then each of the page's reads answered
// by the fixtures above. An override replaces one read's answer — a body,
// or a handler for a refusal.
// ---------------------------------------------------------------------------

type Answer = unknown | ((r: Route) => Promise<void>);
type Reads = Partial<Record<keyof typeof ENDPOINTS, Answer>>;

const ENDPOINTS = {
  summary: /\/api\/commerce\/summary$/,
  invoices: /\/api\/commerce\/invoices(\?|$)/,
  // The page an invoice link opens, so following one lands on a page
  // that renders rather than on the floor's `[]`.
  invoiceDetail: /\/api\/commerce\/invoices\/inv-0001$/,
  accounts: /\/api\/people\/accounts\?limit=1000$/,
  apAging: /\/api\/inventory\/ap-aging$/,
  orders: /\/api\/inventory\/orders$/,
  incomeStatement: /\/api\/ledger\/income-statement(\?|$)/,
  balanceSheet: /\/api\/ledger\/balance-sheet(\?|$)/,
  runoff: /\/api\/ledger\/deferred-revenue-runoff(\?|$)/,
  cashFlow: /\/api\/ledger\/cash-flow(\?|$)/,
  trialBalance: /\/api\/ledger\/trial-balance(\?|$)/,
  periods: /\/api\/ledger\/periods$/,
  entries: /\/api\/ledger\/entries\?account_code=/,
  entryDetail: /\/api\/ledger\/entries\/ent-000\d$/,
  taxLiability: /\/api\/ledger\/tax-liability$/,
} as const;

const DEFAULTS: Record<keyof typeof ENDPOINTS, Answer> = {
  summary: SUMMARY,
  invoices: { data: INVOICES, total: INVOICES.length, limit: 1000, offset: 0 },
  invoiceDetail: INVOICES[0],
  accounts: { data: ACCOUNTS, total: ACCOUNTS.length, limit: 1000, offset: 0 },
  apAging: AP_AGING,
  orders: [po('po-0001', 'draft'), po('po-0002', 'submitted')],
  incomeStatement: INCOME_STATEMENT,
  balanceSheet: BALANCE_SHEET,
  runoff: RUNOFF,
  cashFlow: (r: Route) =>
    json(r, new URL(r.request().url()).searchParams.get('method') === 'direct' ? CASH_FLOW_DIRECT : CASH_FLOW),
  trialBalance: TRIAL_BALANCE,
  periods: [PERIOD_OPEN],
  entries: ENTRIES,
  entryDetail: ENTRY_DETAIL,
  taxLiability: TAX_LIABILITY,
};

/// Every API request the page sends, in order, as "METHOD url".
type Sent = Readonly<{ method: string; url: string; body: string | null }>;

async function install(page: Page, reads: Reads = {}): Promise<Sent[]> {
  // window.print() is the Print / PDF buttons' whole effect: counted,
  // not opened (a headed run would block on the real dialog).
  await page.addInitScript(() => {
    const w = window as unknown as { __prints: number };
    w.__prints = 0;
    window.print = () => {
      w.__prints += 1;
    };
  });
  await installSmokeMocks(page);
  for (const [name, re] of Object.entries(ENDPOINTS) as [keyof typeof ENDPOINTS, RegExp][]) {
    const answer = name in reads ? reads[name] : DEFAULTS[name];
    await page.route(re, (r) =>
      typeof answer === 'function' ? (answer as (r: Route) => Promise<void>)(r) : json(r, answer));
  }
  const sent: Sent[] = [];
  page.on('request', (r: Request) => {
    if (new URL(r.url()).pathname.startsWith('/api/')) sent.push({ method: r.method(), url: r.url(), body: r.postData() });
  });
  return sent;
}

const tab = (page: Page, name: string) => page.getByRole('tab', { name, exact: true });
const panel = (page: Page) => page.locator('.tab-panel');
const prints = (page: Page): Promise<number> =>
  page.evaluate(() => (window as unknown as { __prints: number }).__prints);
const reads = (sent: ReadonlyArray<Sent>, re: RegExp): string[] =>
  sent.filter((s) => s.method === 'GET' && re.test(s.url)).map((s) => s.url);
const writes = (sent: ReadonlyArray<Sent>): Sent[] => sent.filter((s) => s.method !== 'GET' && !/surface-opens/.test(s.url));

/// The catalog entry a link lands under — its own path, or the nearest
/// catalogued parent of a detail route — read from nav-catalog.ts, and
/// what the router serves it as. `entry` null is a route the router
/// serves that no catalog entry owns.
function landing(href: string): Readonly<{ route: string; entry: string | null }> {
  const path = href.split(/[?#]/)[0]!;
  const entry = Object.values(ROUTE_CATALOG)
    .map((e) => e as { id: string; path: string })
    .filter((e) => path === e.path || path.startsWith(`${e.path}/`))
    .sort((a, b) => b.path.length - a.path.length)[0];
  return { route: parseRoute(path).kind, entry: entry?.id ?? null };
}

async function download(page: Page, click: () => Promise<void>): Promise<Readonly<{ name: string; text: string }>> {
  const [dl] = await Promise.all([page.waitForEvent('download'), click()]) as [Download, void];
  const file = await dl.path();
  return { name: dl.suggestedFilename(), text: file ? await readFile(file, 'utf8') : '' };
}

const csvHeader = (text: string): string => text.replace(/^﻿/, '').split('\r\n')[0]!;

// ---------------------------------------------------------------------------
// The frame: header, the two create links, the close package, the tabs.
// ---------------------------------------------------------------------------

test.describe('/ux/finance — the frame', () => {
  test('the header reads the commerce summary, and says when it could not', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH);
    const header = page.locator('.exec-header');
    await expect(header.locator('.exec-eyebrow')).toHaveText('Finance');
    // 801b6a7a: this headline is boss-commerce's trailing revenue, which
    // no Algedonic protocol writes; the ledger's income statement is the
    // figure a protocol produces, and the two cannot agree.
    await expect(header.locator('h1')).toHaveText('$12K trailing revenue');
    await expect(header.locator('p')).toHaveText('$10K gross margin (81.0%) · $420 receivables outstanding');

    await install(page, { summary: refuse(503, 'commerce down') });
    await page.reload();
    await expect(header.locator('h1')).toHaveText('Finance');
    await expect(header.locator('p')).toHaveText('Summary unavailable');
  });

  test('eight tabs, the registry of FINANCE_TABS, each opening its own statement', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH);
    await expect(page.getByRole('tablist').first().getByRole('tab')).toHaveText(FINANCE_TABS.map((t) => t.label));
    await expect(tab(page, 'Overview')).toHaveAttribute('aria-selected', 'true');

    const headings: ReadonlyArray<readonly [string, ReadonlyArray<string>]> = [
      ['Overview', ['Accounts receivable aging', 'Accounts payable aging', 'Gross margin by product line (trailing 12 months)']],
      ['PO Approvals', ['Pending approval (1)', 'Recently approved']],
      ['Income statement', ['Income statement']],
      ['Balance sheet', ['Balance sheet', 'Deferred revenue runoff']],
      ['Cash flow', ['Cash flow statement']],
      ['Trial Balance', ['Trial balance', 'Periods']],
      ['Tax liability', ['Outstanding tax liability', 'Accrued filings (1)']],
    ];
    for (const [name, h3s] of headings) {
      await tab(page, name).click();
      await expect(tab(page, name)).toHaveAttribute('aria-selected', 'true');
      await expect(panel(page).locator('h3')).toHaveText(h3s);
    }
    await tab(page, 'Invoices').click();
    await expect(panel(page).getByPlaceholder('Invoice, account…')).toBeVisible();
  });

  test('the two create links land under the finance catalog entry, and back returns', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH);
    const newInvoice = page.getByRole('link', { name: '+ New invoice' });
    const newEntry = page.getByRole('link', { name: '+ New journal entry' });
    await expect(newInvoice).toHaveAttribute('href', '/ux/finance/new');
    await expect(newEntry).toHaveAttribute('href', '/ux/finance/journal-entries/new');
    expect(landing('/ux/finance/new')).toEqual({ route: 'newInvoice', entry: 'finance' });
    expect(landing('/ux/finance/journal-entries/new')).toEqual({ route: 'newJournalEntry', entry: 'finance' });

    await newInvoice.click();
    await expect.poll(() => new URL(page.url()).pathname).toBe('/ux/finance/new');
    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(page.locator('.exec-header h1')).toHaveText('$12K trailing revenue');
  });

  test('invoices and the summary re-read every 30 seconds, and nothing else does', async ({ page }) => {
    await page.clock.install();
    const sent = await install(page);
    await mountPage(page, PATH);
    // Counted from what the mount left, never from one: an installed
    // clock still runs on real time, so on a starved runner the page's
    // own 30 s poll had already read again before the count was taken
    // (31 real seconds held after mount: expected 1, received 2;
    // backlog 3027f808). -1 while a tick's two reads are only partly in.
    const both = (): number => {
      const n = reads(sent, ENDPOINTS.summary).length;
      return n === reads(sent, ENDPOINTS.invoices).length ? n : -1;
    };
    let before = -1;
    await expect.poll(() => (before = both())).toBeGreaterThan(0);

    await page.clock.runFor(30_000);
    await expect.poll(() => both()).toBeGreaterThan(before);
    // However many ticks real time added, nothing else re-reads.
    expect(reads(sent, ENDPOINTS.apAging)).toHaveLength(1);
  });
});

test.describe('/ux/finance — the monthly close package', () => {
  const opener = (page: Page) => page.locator('.mcp-wrap > button');
  const dialog = (page: Page) => page.getByRole('dialog', { name: 'Monthly close package' });

  test('the button opens a panel it then closes, and says which it will do', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH);
    await expect(opener(page)).toHaveText('Monthly close package');
    await opener(page).click();
    await expect(dialog(page)).toBeVisible();
    await expect(opener(page)).toHaveText('Close');
    await expect(dialog(page).getByLabel('Month')).toHaveValue(/^\d{4}-\d{2}$/);
    await expect(dialog(page).locator('.mcp-note')).toHaveText(
      'Bundles trial balance, income statement, balance sheet, and cash flow for the selected month plus a README documenting source endpoints. All amounts in USD.',
    );
    await opener(page).click();
    await expect(dialog(page)).toHaveCount(0);
    await expect(opener(page)).toHaveText('Monthly close package');
  });

  test('Download ZIP reads the four statements for the chosen month and downloads one ZIP', async ({ page }) => {
    const sent = await install(page);
    await mountPage(page, PATH);
    await opener(page).click();
    await dialog(page).getByLabel('Month').fill('2026-08');
    const before = sent.length;

    const zip = await download(page, () => dialog(page).getByRole('button', { name: 'Download ZIP' }).click());
    expect(zip.name).toMatch(/^monthly-close-2026-08-\d{4}-\d{2}-\d{2}\.zip$/);
    const asked = sent.slice(before).map((s) => { const u = new URL(s.url); return `${u.pathname}${u.search}`; }).sort();
    expect(asked).toEqual([
      '/api/ledger/balance-sheet?as_of=2026-08-31',
      '/api/ledger/cash-flow?from=2026-08-01&to=2026-08-31',
      '/api/ledger/income-statement?from=2026-08-01&to=2026-08-31',
      '/api/ledger/trial-balance?as_of=2026-08-31',
    ]);
    await expect(dialog(page).locator('.mcp-ok')).toHaveText('Download started.');
    await expect(dialog(page).locator('.mcp-warn-list')).toHaveCount(0);
  });

  test('a statement that fails to load is named, and the ZIP ships without it', async ({ page }) => {
    await install(page, { trialBalance: refuse(500, 'ledger down') });
    await mountPage(page, PATH);
    await opener(page).click();

    const zip = await download(page, () => dialog(page).getByRole('button', { name: 'Download ZIP' }).click());
    expect(zip.name).toMatch(/\.zip$/);
    await expect(dialog(page).locator('.mcp-ok')).toHaveText('Download started. (1 warning)');
    await expect(dialog(page).locator('.mcp-warn-list li')).toHaveText(['trial-balance failed to load']);
  });
});

// ---------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------

test.describe('/ux/finance — Overview', () => {
  test('AR aging, AP aging and gross margin render their rows and totals', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH);
    const [ar, ap, gm] = [0, 1, 2].map((i) => panel(page).locator('section.tab-section').nth(i));
    // formatMoney's auto precision: $100 and over rounds to the dollar.
    await expect(ar!.locator('tbody tr')).toHaveText([/Current\s*2\s*\$300\s*$/, /1-30 days\s*1\s*\$120\s*$/, /Total\s*3\s*\$420\s*$/]);
    // a7bad6f0 (closed into 801b6a7a): AP aging reads boss-inventory, which
    // Algedonic does not run, and the ledger's own bills go unread.
    await expect(ap!.locator('tbody tr')).toHaveText([/Current\s*3\s*\$450\s*$/, /Total\s*3\s*\$450\s*$/]);
    await expect(ap!.locator('p.empty')).toHaveText(
      'Aging measured from invoice received_on. Due-date aging lands with the vendor-terms join in a follow-up.',
    );
    await expect(gm!.locator('tbody tr')).toHaveText([
      /Sponsorship\s*\$12,345\s*\$2,345\s*\$10,000\s*81\.0%/,
      /Total\s*\$12,345\s*\$2,345\s*\$10,000\s*81\.0%/,
    ]);
  });

  test('each Download CSV downloads its table, and Print / PDF opens the print dialog', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH);
    const buttons = panel(page).getByRole('button', { name: 'Download CSV' });
    await expect(buttons).toHaveCount(2);

    const ar = await download(page, () => buttons.nth(0).click());
    expect(ar.name).toMatch(/^ar-aging-\d{4}-\d{2}-\d{2}\.csv$/);
    expect(ar.text.replace(/^﻿/, '').split('\r\n').slice(0, 3)).toEqual([
      'Bucket,Invoices,Amount,Currency', 'Current,2,300.00,USD', '1-30 days,1,120.00,USD',
    ]);
    const ap = await download(page, () => buttons.nth(1).click());
    expect(ap.name).toMatch(/^ap-aging-\d{4}-\d{2}-\d{2}\.csv$/);
    expect(csvHeader(ap.text)).toBe('Bucket,Invoices,Amount,Currency');

    await panel(page).getByRole('button', { name: 'Print / PDF' }).click();
    expect(await prints(page)).toBe(1);
  });

  test("AP aging's Download CSV is disabled while AP aging is unavailable", async ({ page }) => {
    await install(page, { apAging: refuse(502, 'inventory upstream unavailable') });
    await mountPage(page, PATH);
    await expect(panel(page).locator(FAILURE_MARKER)).toHaveText('AP aging unavailable.');
    await expect(panel(page).getByRole('button', { name: 'Download CSV' }).nth(1)).toBeDisabled();
    await expect(panel(page).getByRole('button', { name: 'Download CSV' }).nth(0)).toBeEnabled();
  });
});

// ---------------------------------------------------------------------------
// Invoices
// ---------------------------------------------------------------------------

test.describe('/ux/finance — Invoices', () => {
  const rows = (page: Page) => panel(page).locator('tbody tr');
  const ids = (page: Page) => rows(page).locator('td:first-child');
  const filter = (page: Page, name: string) => panel(page).getByRole('button', { name, exact: true });

  test('status and method filters count every invoice and narrow the list', async ({ page }) => {
    await install(page);
    await mountPage(page, `${PATH}?tab=invoices`);
    // Issued, newest first, is the landing order.
    await expect(ids(page)).toHaveText(['inv-0002', 'inv-0001', 'inv-0003', 'inv-0004']);
    const buttons = panel(page).locator('.catalog-filters button');
    await expect(buttons).toHaveText([
      'All (4)', 'Unpaid (2)', 'Past due (1)', 'Paid (1)', 'Written off (1)',
      'All (4)', 'ACH (1)', 'Wire (1)', 'Check (1)', 'Card (1)',
    ]);

    await filter(page, 'Unpaid (2)').click();
    await expect(filter(page, 'Unpaid (2)')).toHaveAttribute('aria-pressed', 'true');
    await expect(ids(page)).toHaveText(['inv-0002', 'inv-0003']);
    await filter(page, 'Past due (1)').click();
    await expect(ids(page)).toHaveText(['inv-0003']);
    await filter(page, 'Paid (1)').click();
    await expect(ids(page)).toHaveText(['inv-0001']);
    await filter(page, 'Written off (1)').click();
    await expect(ids(page)).toHaveText(['inv-0004']);

    await buttons.nth(0).click();
    await filter(page, 'Wire (1)').click();
    await expect(ids(page)).toHaveText(['inv-0003']);
    await filter(page, 'Paid (1)').click();
    await expect(rows(page)).toHaveCount(0);
    await expect(panel(page).locator('p.empty')).toHaveText('No invoices match those filters.');
    await expect(panel(page).locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('Written off is offered only when an invoice was written off', async ({ page }) => {
    const three = INVOICES.slice(0, 3);
    await install(page, { invoices: { data: three, total: 3, limit: 1000, offset: 0 } });
    await mountPage(page, `${PATH}?tab=invoices`);
    await expect(panel(page).locator('.catalog-filters button').nth(0)).toHaveText('All (3)');
    await expect(panel(page).getByRole('button', { name: /^Written off/ })).toHaveCount(0);
  });

  test('search matches an invoice id, an account name and a line description', async ({ page }) => {
    await install(page);
    await mountPage(page, `${PATH}?tab=invoices`);
    const search = panel(page).getByPlaceholder('Invoice, account…');
    await search.fill('inv-0003');
    await expect(ids(page)).toHaveText(['inv-0003']);
    await search.fill('barley');
    await expect(ids(page)).toHaveText(['inv-0002', 'inv-0004']);
    await search.fill('gold sponsor');
    await expect(ids(page)).toHaveText(['inv-0001']);
    await search.fill('nothing like this');
    await expect(panel(page).locator('p.empty')).toHaveText('No invoices match those filters.');
  });

  test('every column header sorts, money descending first', async ({ page }) => {
    await install(page);
    await mountPage(page, `${PATH}?tab=invoices`);
    const header = (name: string) => panel(page).locator('thead th', { hasText: name });
    await expect(panel(page).locator('thead th')).toHaveCount(10);
    await expect(header('Issued')).toHaveAttribute('aria-sort', 'descending');

    await header('Amount').click();
    await expect(header('Amount')).toHaveAttribute('aria-sort', 'descending');
    await expect(ids(page)).toHaveText(['inv-0002', 'inv-0003', 'inv-0001', 'inv-0004']);
    await header('Amount').click();
    await expect(header('Amount')).toHaveAttribute('aria-sort', 'ascending');
    await expect(ids(page)).toHaveText(['inv-0004', 'inv-0001', 'inv-0003', 'inv-0002']);

    await header('Account').click();
    await expect(header('Account')).toHaveAttribute('aria-sort', 'ascending');
    await expect(rows(page).locator('td:nth-child(3)')).toHaveText(['Barley Works', 'Barley Works', 'Hop Collective', 'Hop Collective']);
    for (const name of ['Invoice', 'Status', 'Lines', 'Tax', 'Method', 'Due', 'Paid']) {
      await header(name).click();
      await expect(header(name)).toHaveAttribute('aria-sort', /ascending|descending/);
    }
  });

  test('a row links its invoice and its account, and back returns to the tab', async ({ page }) => {
    await install(page);
    await mountPage(page, `${PATH}?tab=invoices`);
    const row = rows(page).filter({ hasText: 'inv-0001' });
    await expect(row.getByRole('link', { name: 'inv-0001' })).toHaveAttribute('href', '/ux/finance/inv-0001');
    await expect(row.getByRole('link', { name: 'Hop Collective' })).toHaveAttribute('href', '/ux/accounts/acct-1');
    expect(landing('/ux/finance/inv-0001')).toEqual({ route: 'invoice', entry: 'finance' });
    expect(landing('/ux/accounts/acct-1')).toEqual({ route: 'account', entry: 'accounts' });
    await expect(row).toContainText('$100');
    await expect(row).toContainText('$8.00');
    await expect(row).toContainText('CA');
    await expect(row).toContainText('Card');

    await row.getByRole('link', { name: 'inv-0001' }).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe('/ux/finance/inv-0001');
    await page.goBack();
    await expect.poll(() => `${new URL(page.url()).pathname}${new URL(page.url()).search}`).toBe(`${PATH}?tab=invoices`);
    await expect(tab(page, 'Invoices')).toHaveAttribute('aria-selected', 'true');
  });

  test('an account whose name did not load is linked by its id — by design, not a failure', async ({ page }) => {
    await install(page, { accounts: refuse(503, 'people down') });
    await mountPage(page, `${PATH}?tab=invoices`);
    await expect(rows(page).filter({ hasText: 'inv-0001' }).getByRole('link', { name: 'acct-1' }))
      .toHaveAttribute('href', '/ux/accounts/acct-1');
    await expect(panel(page).locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('a list the summary says is longer than the page loaded says so', async ({ page }) => {
    await install(page, { summary: { ...SUMMARY, total_invoice_count: 1500 } });
    await mountPage(page, `${PATH}?tab=invoices`);
    await expect(panel(page).locator('.overflow-banner')).toHaveText(
      /Showing\s+4\s+of\s+1,500\s+invoices\.\s+Use search or status filters to narrow the list\./,
    );
  });

  test('a failed invoice read is a failure line, never "No invoices match"', async ({ page }) => {
    await install(page, { invoices: refuse(503, 'commerce down') });
    await mountPage(page, `${PATH}?tab=invoices`);
    const failed = panel(page).locator(FAILURE_MARKER);
    await expect(failed).toHaveAttribute('role', 'alert');
    await expect(failed).toContainText("Couldn't load invoices —");
    await expect(failed).toContainText('HTTP 503');
    await expect(panel(page).getByText('No invoices match those filters.')).toHaveCount(0);
  });
});

// ---------------------------------------------------------------------------
// PO Approvals (the Approve write's refusals: finance-po-approvals-refused)
// ---------------------------------------------------------------------------

test.describe('/ux/finance — PO Approvals', () => {
  test('pending and recently approved POs link the PO and the vendor', async ({ page }) => {
    await install(page);
    await mountPage(page, `${PATH}?tab=approvals`);
    const [pending, recent] = [0, 1].map((i) => panel(page).locator('section.tab-section').nth(i));
    await expect(pending!.locator('tbody tr')).toHaveText([/po-0001\s*vendor-0001\s*HOPS-CASCADE x4\s*\$100\s*2026-09-20\s*Approve/]);
    await expect(recent!.locator('tbody tr')).toHaveText([/po-0002\s*vendor-0001\s*HOPS-CASCADE x4\s*submitted/]);
    for (const s of [pending!, recent!]) {
      await expect(s.getByRole('link', { name: /^po-000/ })).toHaveAttribute('href', /^\/ux\/purchase-orders\/po-000\d$/);
      await expect(s.getByRole('link', { name: 'vendor-0001' })).toHaveAttribute('href', '/ux/vendors/vendor-0001');
    }
    // The PO detail is a route the router serves that NO catalog entry
    // owns; the vendor lands under Vendors.
    expect(landing('/ux/purchase-orders/po-0001')).toEqual({ route: 'po', entry: null });
    expect(landing('/ux/vendors/vendor-0001')).toEqual({ route: 'vendor', entry: 'vendors' });
  });

  test('Approve sends the PO to submitted', async ({ page }) => {
    const sent = await install(page);
    await page.route(/\/api\/inventory\/orders\/po-0001\/status$/, (r) => json(r, po('po-0001', 'submitted')));
    await mountPage(page, `${PATH}?tab=approvals`);
    await panel(page).getByRole('button', { name: 'Approve' }).click();
    await expect(panel(page).getByText('No purchase orders awaiting approval.')).toBeVisible();
    expect(writes(sent).map((s) => [s.method, new URL(s.url).pathname, s.body])).toEqual([
      ['PUT', '/api/inventory/orders/po-0001/status', JSON.stringify({ status: 'submitted' })],
    ]);
  });

  test('no draft is an empty section; a failed read is a failure line', async ({ page }) => {
    await install(page, { orders: [] });
    await mountPage(page, `${PATH}?tab=approvals`);
    await expect(panel(page).locator('h3')).toHaveText(['Pending approval (0)']);
    await expect(panel(page).locator('p.empty')).toHaveText('No purchase orders awaiting approval.');

    await install(page, { orders: refuse(503, 'inventory down') });
    await page.reload();
    const failed = panel(page).locator(FAILURE_MARKER);
    await expect(failed).toHaveText("Couldn't load purchase orders — HTTP 503");
    await expect(failed).toHaveAttribute('role', 'alert');
  });
});

// ---------------------------------------------------------------------------
// The four statements
// ---------------------------------------------------------------------------

test.describe('/ux/finance — Income statement', () => {
  test('renders the ledger lines, re-reads on a date, downloads and prints', async ({ page }) => {
    const sent = await install(page);
    await mountPage(page, `${PATH}?tab=income-statement`);
    const table = panel(page).locator('table.tb-table');
    await expect(table).toContainText('4200 Sponsorship revenue');
    await expect(table.locator('tr', { hasText: 'Total revenue' })).toContainText('$1.00');
    await expect(table.locator('tr', { hasText: '(no activity)' })).toHaveCount(1);
    await expect(table.locator('tr', { hasText: 'Net income' })).toContainText('$0.60');

    await panel(page).getByLabel('From').fill('2026-09-01');
    await expect.poll(() => reads(sent, ENDPOINTS.incomeStatement).some((u) => u.includes('from=2026-09-01'))).toBe(true);
    const to = await panel(page).getByLabel('To').inputValue();

    const csv = await download(page, () => panel(page).getByRole('button', { name: 'Download CSV' }).click());
    // The file is named from the statement's own bounds, as the ledger answered them.
    expect(csv.name).toBe('income-statement-2026-01-01-to-2026-09-23.csv');
    expect(to).toMatch(/^\d{4}-\d{2}-\d{2}$/);
    await panel(page).getByRole('button', { name: 'Print / PDF' }).click();
    expect(await prints(page)).toBe(1);
  });
});

test.describe('/ux/finance — Balance sheet', () => {
  test('renders, says when it does not balance, and the runoff says when it drifts', async ({ page }) => {
    await install(page, { balanceSheet: { ...BALANCE_SHEET, balanced: false, imbalance_cents: 2_500 } });
    await mountPage(page, `${PATH}?tab=balance-sheet`);
    const sheet = panel(page).locator('section.tab-section').nth(0);
    await expect(sheet.getByRole('alert')).toContainText('Imbalance: $25.00');
    await expect(sheet.locator('tr', { hasText: 'Total assets' })).toContainText('$1.00');
    await expect(sheet.locator('tr', { hasText: '(none)' })).toHaveCount(1);
    await expect(sheet.locator('tr', { hasText: 'Total liabilities + equity' })).toContainText('$1.00');

    const runoff = panel(page).locator('section.tab-section').nth(1);
    await expect(runoff.locator('.stat-value')).toHaveText(['$30.00', '$25.00', '$5.00']);
    await expect(runoff.getByRole('note')).toContainText('Drift:');
    await expect(runoff.locator('tbody tr')).toHaveText([/Oct 2026\s*\$20\.00/, /Beyond 12 months\s*\$5\.00/, /Total schedules remaining\s*\$25\.00/]);
  });

  test('the as-of date re-reads the sheet and the runoff; CSV and print', async ({ page }) => {
    const sent = await install(page);
    await mountPage(page, `${PATH}?tab=balance-sheet`);
    await expect(panel(page).getByRole('alert')).toHaveCount(0);
    await panel(page).getByLabel('As of').fill('2026-06-30');
    await expect.poll(() => reads(sent, ENDPOINTS.balanceSheet).some((u) => u.includes('as_of=2026-06-30'))).toBe(true);
    await expect.poll(() => reads(sent, ENDPOINTS.runoff).some((u) => u.includes('as_of=2026-06-30&months=12'))).toBe(true);

    const csv = await download(page, () => panel(page).getByRole('button', { name: 'Download CSV' }).click());
    expect(csv.name).toBe('balance-sheet-2026-09-23.csv');
    expect(csvHeader(csv.text)).toBe('Section,Account code,Account name,Amount,Currency');
    await panel(page).getByRole('button', { name: 'Print / PDF' }).click();
    expect(await prints(page)).toBe(1);
  });
});

test.describe('/ux/finance — Cash flow', () => {
  test('Indirect by default, Direct on the toggle, each with its own CSV', async ({ page }) => {
    const sent = await install(page);
    await mountPage(page, `${PATH}?tab=cash-flow`);
    const method = (name: string) => panel(page).getByRole('tablist', { name: 'Cash flow method' }).getByRole('tab', { name, exact: true });
    await expect(method('Indirect')).toHaveAttribute('aria-selected', 'true');
    await expect(panel(page).getByRole('alert')).toContainText('Reconciliation gap: $1.00');
    await expect(panel(page).locator('table.tb-table')).toContainText('1200 Accounts receivable');
    const indirect = await download(page, () => panel(page).getByRole('button', { name: 'Download CSV' }).click());
    expect(indirect.name).toBe('cash-flow-2026-01-01-to-2026-09-23.csv');

    await method('Direct').click();
    await expect(method('Direct')).toHaveAttribute('aria-selected', 'true');
    await expect.poll(() => reads(sent, ENDPOINTS.cashFlow).some((u) => u.includes('method=direct'))).toBe(true);
    await expect(panel(page).locator('table.tb-table tr', { hasText: 'Cash in from customers' })).toContainText('$1.00');
    await expect(panel(page).locator('table.tb-table tr', { hasText: 'Cash out to vendors' })).toContainText('($0.40)');
    await expect(panel(page).getByRole('alert')).toHaveCount(0);
    const direct = await download(page, () => panel(page).getByRole('button', { name: 'Download CSV' }).click());
    expect(direct.name).toBe('cash-flow-direct-2026-01-01-to-2026-09-23.csv');
    expect(csvHeader(direct.text)).toBe('Section,Account name,Amount,Currency');

    await panel(page).getByLabel('From').fill('2026-07-01');
    await expect.poll(() => reads(sent, ENDPOINTS.cashFlow).some((u) => u.includes('method=direct&from=2026-07-01'))).toBe(true);
    await panel(page).getByRole('button', { name: 'Print / PDF' }).click();
    expect(await prints(page)).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// Trial Balance: the drill-down, the periods and the three ledger writes
// ---------------------------------------------------------------------------

test.describe('/ux/finance — Trial Balance', () => {
  const tbTable = (page: Page) => panel(page).locator('table.tb-table');
  const accountRow = (page: Page, code: string) => tbTable(page).locator('tr.tb-row', { hasText: code });
  const periodRow = (page: Page) => panel(page).locator('table.tb-periods tbody tr');

  test('shows the accounts that moved, the totals and whether they balance; CSV, print and as-of', async ({ page }) => {
    const sent = await install(page);
    await mountPage(page, `${PATH}?tab=trial-balance`);
    await expect(tbTable(page).locator('tbody tr td:first-child')).toHaveText(['1010', '4200']);
    await expect(tbTable(page).locator('tfoot')).toHaveText(/Totals\s*\$1\.00\s*\$1\.00\s*BALANCED/);

    const csv = await download(page, () => panel(page).getByRole('button', { name: 'Download CSV' }).click());
    expect(csv.name).toBe('trial-balance-2026-09-23.csv');
    expect(csvHeader(csv.text)).toBe('Account code,Account name,Kind,Normal side,Debits,Credits,Balance,Currency');
    await panel(page).getByRole('button', { name: 'Print / PDF' }).click();
    expect(await prints(page)).toBe(1);

    await panel(page).getByLabel('As of').fill('2026-08-31');
    await expect.poll(() => reads(sent, ENDPOINTS.trialBalance).some((u) => u.endsWith('as_of=2026-08-31'))).toBe(true);
  });

  test('a trial balance that does not balance says MISMATCH', async ({ page }) => {
    await install(page, { trialBalance: { ...TRIAL_BALANCE, total_credits_cents: 90, balanced: false } });
    await mountPage(page, `${PATH}?tab=trial-balance`);
    await expect(tbTable(page).locator('tfoot')).toContainText('MISMATCH');
  });

  test('an account opens the entries that touch it; an entry opens its lines and its fact', async ({ page }) => {
    await install(page);
    await mountPage(page, `${PATH}?tab=trial-balance`);
    await accountRow(page, '4200').click();
    const drill = panel(page).locator('section.tab-section', { hasText: 'Entries touching 4200 — Sponsorship revenue' });
    await expect(drill.locator('h3')).toHaveText('Entries touching 4200 — Sponsorship revenue');
    await expect(drill.locator('table.tb-entries tbody tr')).toHaveCount(2);

    // A packet-sourced entry — the only kind Algedonic's ledger holds —
    // renders its source as text. e0b754d3: it should link the packet.
    const fromPacket = drill.locator('table.tb-entries tr.tb-row', { hasText: 'Sponsorship received' });
    await expect(fromPacket.locator('td:nth-child(4)')).toHaveText(`jobs · ${PACKET_ID}`);
    await expect(fromPacket.getByRole('link')).toHaveCount(0);
    // An invoice-sourced one links the invoice, under the finance entry.
    const fromInvoice = drill.locator('table.tb-entries tr.tb-row', { hasText: 'Invoice paid' });
    await expect(fromInvoice.getByRole('link', { name: 'inv-0001' })).toHaveAttribute('href', '/ux/finance/inv-0001');

    await fromPacket.click();
    const detail = drill.locator('.tb-entry-detail');
    await expect(detail.locator('h4')).toContainText('Entry ent-0001… · posted 2026-09-17');
    await expect(detail.locator('.ruleset-badge')).toHaveText('RuleSet v1');
    await expect(detail.locator('table.tb-entry-lines tbody tr')).toHaveText([
      /1010\s*Cash in Transit\s*\$1\.00/, /4200\s*Sponsorship revenue\s*\$1\.00/,
    ]);
    // e0b754d3 again, in the fact line.
    await expect(detail.locator('summary')).toHaveText(`Fact: finance.sponsorship.received · jobs · ${PACKET_ID}`);
    await expect(detail.locator('pre')).toBeHidden();
    await detail.locator('summary').click();
    await expect(detail.locator('pre')).toContainText('"amount_cents": 100');

    await fromPacket.click();
    await expect(detail).toHaveCount(0);
    await accountRow(page, '4200').click();
    await expect(drill).toHaveCount(0);
  });

  test('Refresh re-reads the periods and the trial balance', async ({ page }) => {
    const sent = await install(page);
    await mountPage(page, `${PATH}?tab=trial-balance`);
    await expect(periodRow(page)).toHaveCount(1);
    const [p0, t0] = [reads(sent, ENDPOINTS.periods).length, reads(sent, ENDPOINTS.trialBalance).length];
    await panel(page).getByRole('button', { name: 'Refresh' }).click();
    await expect.poll(() => reads(sent, ENDPOINTS.periods).length).toBe(p0 + 1);
    await expect.poll(() => reads(sent, ENDPOINTS.trialBalance).length).toBe(t0 + 1);
  });

  test('Lock locks the period as "operator" and re-reads it', async ({ page }) => {
    let locked = false;
    const sent = await install(page, { periods: (r: Route) => json(r, [locked ? PERIOD_LOCKED : PERIOD_OPEN]) });
    await page.route(/\/api\/ledger\/periods\/per-2026-09\/lock$/, (r) => {
      locked = true;
      return json(r, { checksum: PERIOD_LOCKED.locked_checksum });
    });
    await mountPage(page, `${PATH}?tab=trial-balance`);
    await expect(periodRow(page)).toHaveText(/2026-09-01\s*open\s*1\s*\$1\.00\s*\$1\.00\s*—\s*—\s*Lock/);

    await periodRow(page).getByRole('button', { name: 'Lock' }).click();
    // 975c228f: the body names no one — 'operator' is a literal in
    // TrialBalanceTab, and the Locked by column renders it back.
    // e536e91a: no protocol and no sign-off stand between the click and
    // the lock.
    await expect(periodRow(page)).toHaveText(/2026-09-01\s*locked\s*1\s*\$1\.00\s*\$1\.00\s*operator\s*abcdef012345\s*Unlock/);
    expect(writes(sent).map((s) => [s.method, new URL(s.url).pathname, s.body])).toEqual([
      ['POST', '/api/ledger/periods/per-2026-09/lock', JSON.stringify({ locked_by: 'operator' })],
    ]);
  });

  test('a refused Lock says so on its row, with the status and the reason', async ({ page }) => {
    await install(page);
    await page.route(/\/api\/ledger\/periods\/per-2026-09\/lock$/, refuse(403, 'not permitted to lock periods'));
    await mountPage(page, `${PATH}?tab=trial-balance`);
    await periodRow(page).getByRole('button', { name: 'Lock' }).click();
    await expect(periodRow(page).locator('.error')).toHaveText('Error: lock failed: 403 not permitted to lock periods');
    await expect(periodRow(page).getByRole('button', { name: 'Lock' })).toBeEnabled();
    await expect(periodRow(page)).toContainText('open');
  });

  test('Unlock asks first, sends nothing when declined, and records no reason when accepted', async ({ page }) => {
    const sent = await install(page, { periods: [PERIOD_LOCKED] });
    await page.route(/\/api\/ledger\/periods\/per-2026-09\/unlock$/, (r) => r.fulfill({ status: 204 }));
    await mountPage(page, `${PATH}?tab=trial-balance`);

    let asked = '';
    page.once('dialog', (d) => { asked = d.message(); void d.dismiss(); });
    await periodRow(page).getByRole('button', { name: 'Unlock' }).click();
    expect(asked).toBe('Unlock 2026-09-01? Clears the lock + checksum and allows new entries in this period again.');
    expect(writes(sent)).toEqual([]);

    page.once('dialog', (d) => void d.accept());
    await periodRow(page).getByRole('button', { name: 'Unlock' }).click();
    // e536e91a: one confirm and one POST, with no body — no reason is
    // asked for or recorded for reopening a closed month.
    await expect.poll(() => writes(sent).map((s) => [s.method, new URL(s.url).pathname, s.body])).toEqual([
      ['POST', '/api/ledger/periods/per-2026-09/unlock', null],
    ]);
  });

  test('a refused Unlock says so on its row', async ({ page }) => {
    await install(page, { periods: [PERIOD_LOCKED] });
    await page.route(/\/api\/ledger\/periods\/per-2026-09\/unlock$/, refuse(409, 'period has a successor lock'));
    await mountPage(page, `${PATH}?tab=trial-balance`);
    page.once('dialog', (d) => void d.accept());
    await periodRow(page).getByRole('button', { name: 'Unlock' }).click();
    await expect(periodRow(page).locator('.error')).toHaveText('Error: unlock failed: 409 period has a successor lock');
  });

  test('Reverse this entry asks, then posts every line flipped', async ({ page }) => {
    const sent = await install(page);
    await page.route(/\/api\/ledger\/journal-entries$/, (r) =>
      json(r, { fact_id: 'fact-0009', entry_id: 'ent-0009', posted_on: '2026-09-25' }));
    await mountPage(page, `${PATH}?entry=ent-0001`);
    const detail = page.locator('.tb-linked-entry .tb-entry-detail');

    let asked = '';
    page.once('dialog', (d) => { asked = d.message(); void d.dismiss(); });
    await detail.getByRole('button', { name: 'Reverse this entry' }).click();
    expect(asked).toBe('Post a reversing entry for ent-0001…?\n\nEvery debit becomes a credit and vice versa, dated today.');
    expect(writes(sent)).toEqual([]);

    page.once('dialog', (d) => void d.accept());
    await detail.getByRole('button', { name: 'Reverse this entry' }).click();
    await expect(detail.locator('.tb-reverse-result')).toHaveText('Reversal posted: ent-0009');
    // 2349285d: the reversal's id links to that entry on the Trial Balance.
    const reversal = detail.locator('.tb-reverse-result').getByRole('link', { name: 'ent-0009' });
    await expect(reversal).toHaveAttribute('href', /\/ux\/finance\?tab=trial-balance&entry=ent-0009$/);
    const [post] = writes(sent);
    expect([post!.method, new URL(post!.url).pathname]).toEqual(['POST', '/api/ledger/journal-entries']);
    const body = JSON.parse(post!.body!) as { posted_on: string; memo: string; created_by: string | null; lines: unknown[] };
    // 975c228f: created_by is null — the reversal names no one.
    expect({ ...body, posted_on: 'today' }).toEqual({
      posted_on: 'today',
      memo: 'Reverses entry ent-0001 — Sponsorship received',
      created_by: null,
      lines: [
        { account_code: '1010', debit_cents: 0, credit_cents: 100, memo: null },
        { account_code: '4200', debit_cents: 100, credit_cents: 0, memo: 'sponsor' },
      ],
    });
    expect(body.posted_on).toMatch(/^\d{4}-\d{2}-\d{2}$/);
  });

  test('a refused Reverse says so beside the button, and the button stays', async ({ page }) => {
    await install(page);
    await page.route(/\/api\/ledger\/journal-entries$/, refuse(409, 'period 2026-09 is locked'));
    await mountPage(page, `${PATH}?entry=ent-0001`);
    const detail = page.locator('.tb-linked-entry .tb-entry-detail');
    page.once('dialog', (d) => void d.accept());
    await detail.getByRole('button', { name: 'Reverse this entry' }).click();
    await expect(detail.locator('.tb-reverse-error')).toHaveText('Error: 409: period 2026-09 is locked');
    await expect(detail.getByRole('button', { name: 'Reverse this entry' })).toBeEnabled();
  });
});

// ---------------------------------------------------------------------------
// Tax liability
// ---------------------------------------------------------------------------

test.describe('/ux/finance — Tax liability', () => {
  test('lists each liability account, the next filing due and the accrued filings', async ({ page }) => {
    await install(page);
    await mountPage(page, `${PATH}?tab=tax-liability`);
    const [outstanding, filings] = [0, 1].map((i) => panel(page).locator('section.tab-section').nth(i));
    await expect(outstanding!.locator('.tb-asof')).toHaveText('As of 2026-09-23');
    // c8b71886: these three descriptions are literals in TaxLiabilityTab,
    // not read from the chart of accounts.
    await expect(outstanding!.locator('tbody tr')).toHaveText([
      /2150 · Payroll liabilities\s*Payroll withholdings \+ employer-side tax; drained quarterly \(941\)\s*\$0\.00/,
      /2300 · Sales tax payable\s*Sales tax collected on invoices; drained monthly per jurisdiction\s*\$8\.00/,
      /2310 · Income tax payable\s*Estimated income tax; drained quarterly\s*\$0\.00/,
      /Total\s*\$8\.00/,
    ]);
    await expect(outstanding!.locator('p.empty')).toHaveText('Next due: Sales tax · US-CA · $8.00 by 2026-09-20');
    await expect(filings!.locator('tbody tr')).toHaveText([/tf-0001\s*Sales tax\s*US-CA\s*2026-08-01 → 2026-08-31\s*2026-09-20\s*\$8\.00/]);

    const csv = await download(page, () => panel(page).getByRole('button', { name: 'Download CSV' }).click());
    expect(csv.name).toMatch(/^tax-liability-\d{4}-\d{2}-\d{2}\.csv$/);
    expect(csvHeader(csv.text)).toBe('Account code,Account name,Balance,Currency');
    await panel(page).getByRole('button', { name: 'Print / PDF' }).click();
    expect(await prints(page)).toBe(1);
  });

  test('no filings: the empty line describes a generator this instance does not run', async ({ page }) => {
    await install(page, { taxLiability: { as_of: '2026-09-23', liabilities: [], accrued_filings: [], next_due: null, currency: 'USD' } });
    await mountPage(page, `${PATH}?tab=tax-liability`);
    await expect(panel(page).getByRole('button', { name: 'Download CSV' })).toBeDisabled();
    await expect(panel(page).locator('h3')).toHaveText(['Outstanding tax liability', 'Accrued filings (0)']);
    // c8b71886: the brewery simulator's tax-authorities generator; no
    // Algedonic protocol accrues a filing (/api/ledger/tax-filings is []).
    await expect(panel(page).locator('section.tab-section').nth(1).locator('p.empty')).toHaveText(
      'No filings awaiting remittance. The tax-authorities generator sweeps sales tax on the 20th of each month and payroll-941 on the 15th of Jan / Apr / Jul / Oct.',
    );
  });
});

// ---------------------------------------------------------------------------
// The auditor's page: every write and both create links are withheld.
// ---------------------------------------------------------------------------

test.describe('/ux/finance — an auditor reads, and writes nothing', () => {
  const AUDITOR = {
    id: 'emp-aud', name: 'Ada Auditor', email: 'ada@demo', role: 'auditor', department: 'finance',
    hire_date: '2024-01-01', status: 'active', location: 'HQ', employment_type: 'full-time', skills: [], certifications: [],
  };

  test('no create link, no Lock or Unlock, no Reverse', async ({ page }) => {
    await install(page, { periods: [PERIOD_OPEN, { ...PERIOD_LOCKED, id: 'per-2026-08', starts_on: '2026-08-01' }] });
    await page.route(/\/api\/people$/, (r) => json(r, [AUDITOR]));
    await page.route(/\/api\/session$/, (r) => json(r, { username: 'ada', employee_id: AUDITOR.id, role: 'auditor' }));
    await mountPage(page, `${PATH}?entry=ent-0001`);

    await expect(page.getByRole('link', { name: '+ New invoice' })).toHaveCount(0);
    await expect(page.getByRole('link', { name: '+ New journal entry' })).toHaveCount(0);
    await expect(page.locator('.mcp-wrap > button')).toHaveText('Monthly close package');
    await expect(page.locator('.tb-linked-entry .tb-entry-detail h4')).toContainText('Entry ent-0001');
    await expect(page.getByRole('button', { name: 'Reverse this entry' })).toHaveCount(0);
    await expect(panel(page).locator('table.tb-periods tbody tr')).toHaveCount(2);
    await expect(panel(page).locator('table.tb-periods').getByRole('button')).toHaveCount(0);
  });
});
