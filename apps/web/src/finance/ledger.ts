// Plain async fetchers + mutators for the boss-ledger API. Port of
// apps/web/src/finance/ledger.ts, sans the React hook wrappers.

import { formatMoney } from '@boss/web-kit/ui/money';
import { appToday } from '@boss/web-kit/sim-clock';
import { failedRead, failedWithReason, okRead, type ReadState } from '../data/readState';

const API_BASE = '/api/ledger';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type TrialBalanceRow = {
  account_code: string;
  account_name: string;
  kind: string;
  normal_side: 'debit' | 'credit';
  debit_total_cents: number;
  credit_total_cents: number;
  balance_cents: number;
  currency: string;
};

export type TrialBalanceResponse = {
  as_of: string | null;
  rows: ReadonlyArray<TrialBalanceRow>;
  total_debits_cents: number;
  total_credits_cents: number;
  balanced: boolean;
  currency: string;
};

export type Period = {
  id: string;
  kind: string;
  starts_on: string;
  ends_on: string;
  status: 'open' | 'locked';
  locked_at: string | null;
  locked_by: string | null;
  locked_rule_version: number | null;
  locked_checksum: string | null;
  entry_count: number;
  total_debits: number;
  total_credits: number;
};

export type LedgerEntry = {
  id: string;
  fact_id: string;
  posted_on: string;
  memo: string | null;
  rule_version: number;
  fact_kind: string;
  fact_source_table: string | null;
  fact_source_id: string | null;
};

export type LedgerEntryLine = {
  account_code: string;
  account_name: string;
  debit_cents: number;
  credit_cents: number;
  currency: string;
  memo: string | null;
  sort_order: number;
};

export type LedgerEntryDetail = {
  id: string;
  fact_id: string;
  posted_on: string;
  memo: string | null;
  rule_version: number;
  fact_kind: string;
  fact_payload: unknown;
  fact_source_table: string | null;
  fact_source_id: string | null;
  lines: ReadonlyArray<LedgerEntryLine>;
};

export type StatementLine = {
  account_code: string;
  account_name: string;
  amount_cents: number;
};

export type IncomeStatement = {
  from: string;
  to: string;
  revenue: ReadonlyArray<StatementLine>;
  total_revenue_cents: number;
  cogs: ReadonlyArray<StatementLine>;
  total_cogs_cents: number;
  gross_profit_cents: number;
  operating_expenses: ReadonlyArray<StatementLine>;
  total_operating_expenses_cents: number;
  net_income_cents: number;
  currency: string;
};

export type CashFlowStatement = {
  from: string;
  to: string;
  net_income_cents: number;
  // GL-attribution operating breakdown (the AR/AP/inventory/tax/revenue
  // offsets that drove operating cash). Working-capital / non-cash ship
  // empty on this path — the detail lives here.
  operating_activities: ReadonlyArray<StatementLine>;
  working_capital_adjustments: ReadonlyArray<StatementLine>;
  non_cash_adjustments: ReadonlyArray<StatementLine>;
  cash_from_operations_cents: number;
  investing_activities: ReadonlyArray<StatementLine>;
  cash_from_investing_cents: number;
  financing_activities: ReadonlyArray<StatementLine>;
  cash_from_financing_cents: number;
  net_change_in_cash_cents: number;
  cash_start_cents: number;
  cash_end_cents: number;
  reconciliation_gap_cents: number;
  reconciled: boolean;
  currency: string;
};

export type DirectCashFlowStatement = {
  from: string;
  to: string;
  method: 'direct';
  cash_in_from_customers_cents: number;
  cash_out_to_vendors_cents: number;
  cash_out_to_employees_cents: number;
  cash_out_to_authorities_cents: number;
  net_change_in_cash_cents: number;
  gl_cash_pool_delta_cents: number;
  gl_cash_1000_delta_cents: number;
  reconciliation_gap_cents: number;
  reconciled: boolean;
  currency: string;
};

export type BalanceSheet = {
  as_of: string;
  assets: ReadonlyArray<StatementLine>;
  total_assets_cents: number;
  liabilities: ReadonlyArray<StatementLine>;
  total_liabilities_cents: number;
  equity: ReadonlyArray<StatementLine>;
  total_equity_cents: number;
  imbalance_cents: number;
  balanced: boolean;
  currency: string;
};

export type DeferredRevenueRunoffMonth = {
  month: string;
  amount_cents: number;
};

export type DeferredRevenueRunoff = {
  as_of: string;
  horizon_months: number;
  deferred_account_balance_cents: number;
  schedules_remaining_cents: number;
  drift_cents: number;
  months: ReadonlyArray<DeferredRevenueRunoffMonth>;
  beyond_horizon_cents: number;
  currency: string;
};

export type TaxFiling = {
  id: string;
  kind: 'sales' | 'income' | 'payroll_941' | 'payroll_940';
  jurisdiction: string;
  period_start: string;
  period_end: string;
  due_on: string;
  filed_on: string | null;
  amount_cents: number;
  liability_account: '2300' | '2310' | '2150';
  status: 'accrued' | 'filed' | 'paid';
  provider: string;
};

export type TaxLiabilityRow = {
  account_code: string;
  account_name: string;
  balance_cents: number;
};

export type TaxLiabilitySummary = {
  as_of: string;
  liabilities: ReadonlyArray<TaxLiabilityRow>;
  accrued_filings: ReadonlyArray<TaxFiling>;
  next_due: TaxFiling | null;
  currency: string;
};

export type Account = {
  id: string;
  code: string;
  name: string;
  kind: 'asset' | 'liability' | 'equity' | 'revenue' | 'expense';
  normal_side: 'debit' | 'credit';
  is_active: boolean;
};

export type ManualEntryLineInput = {
  account_code: string;
  debit_cents: number;
  credit_cents: number;
  memo: string | null;
};

export type ManualEntryInput = {
  posted_on: string;
  memo: string | null;
  created_by: string | null;
  lines: ReadonlyArray<ManualEntryLineInput>;
};

export type ManualEntryResponse = {
  fact_id: string;
  entry_id: string;
  posted_on: string;
};

// ---------------------------------------------------------------------------
// Fetchers
// ---------------------------------------------------------------------------

async function getJson<T>(url: string): Promise<T | null> {
  try {
    const r = await fetch(url);
    if (!r.ok) return null;
    return (await r.json()) as T;
  } catch {
    return null;
  }
}

export function loadIncomeStatement(
  from: string | null,
  to: string | null,
): Promise<IncomeStatement | null> {
  const params = new URLSearchParams();
  if (from) params.set('from', from);
  if (to) params.set('to', to);
  const qs = params.toString();
  return getJson<IncomeStatement>(`${API_BASE}/income-statement${qs ? `?${qs}` : ''}`);
}

export function loadBalanceSheet(asOf: string | null): Promise<BalanceSheet | null> {
  const url = asOf
    ? `${API_BASE}/balance-sheet?as_of=${encodeURIComponent(asOf)}`
    : `${API_BASE}/balance-sheet`;
  return getJson<BalanceSheet>(url);
}

export function loadDeferredRevenueRunoff(
  asOf: string | null,
  months: number,
): Promise<DeferredRevenueRunoff | null> {
  const params = new URLSearchParams();
  if (asOf) params.set('as_of', asOf);
  params.set('months', String(months));
  return getJson<DeferredRevenueRunoff>(
    `${API_BASE}/deferred-revenue-runoff?${params.toString()}`,
  );
}

export function loadCashFlow(
  from: string | null,
  to: string | null,
): Promise<CashFlowStatement | null> {
  const params = new URLSearchParams();
  if (from) params.set('from', from);
  if (to) params.set('to', to);
  const qs = params.toString();
  return getJson<CashFlowStatement>(`${API_BASE}/cash-flow${qs ? `?${qs}` : ''}`);
}

export function loadCashFlowDirect(
  from: string | null,
  to: string | null,
): Promise<DirectCashFlowStatement | null> {
  const params = new URLSearchParams();
  params.set('method', 'direct');
  if (from) params.set('from', from);
  if (to) params.set('to', to);
  return getJson<DirectCashFlowStatement>(`${API_BASE}/cash-flow?${params.toString()}`);
}

export function loadTrialBalance(asOf: string | null): Promise<TrialBalanceResponse | null> {
  const url = asOf ? `${API_BASE}/trial-balance?as_of=${asOf}` : `${API_BASE}/trial-balance`;
  return getJson<TrialBalanceResponse>(url);
}

/// One GET whose failure is kept, reason and all. `getJson` answers
/// null for a failure, which the statement tabs can tell from a body
/// (a statement is never null); a LIST cannot, and folding its null
/// into [] is how a ledger outage read "No periods yet." (backlog
/// 1a2b67c9, page audit 3f964c57). Same shape as WarehousePage's read.
async function readJson<T>(url: string): Promise<{ read: ReadState; body: T | null }> {
  try {
    const r = await fetch(url);
    if (!r.ok) return { read: failedWithReason(r.status, await r.text()), body: null };
    return { read: okRead, body: (await r.json()) as T };
  } catch (e) {
    return { read: failedRead(e instanceof Error ? e.message : String(e)), body: null };
  }
}

/// The periods list and whether the read that built it worked — an
/// empty `periods` means "no periods" only when `read` is ok.
export type PeriodsRead = Readonly<{ read: ReadState; periods: ReadonlyArray<Period> }>;

export async function loadPeriods(): Promise<PeriodsRead> {
  const { read, body } = await readJson<Period[]>(`${API_BASE}/periods`);
  return { read, periods: body ?? [] };
}

export async function loadAccounts(): Promise<Account[]> {
  const body = await getJson<Account[]>(`${API_BASE}/accounts`);
  return body ?? [];
}

export function loadTaxLiability(): Promise<TaxLiabilitySummary | null> {
  return getJson<TaxLiabilitySummary>(`${API_BASE}/tax-liability`);
}

export const ENTRIES_PER_ACCOUNT_CAP = 200;

export type EntriesPage = Readonly<{
  data: ReadonlyArray<LedgerEntry>;
  /// True when the response hit the per-account cap, signalling that
  /// older entries exist beyond this window. The `/api/ledger/entries`
  /// endpoint does not expose a total count today, so we infer "more
  /// available" from "we received exactly the cap" — an over-fetch by
  /// one row sharpens the signal (cap+1 → trim and mark capped).
  capped: boolean;
  /// Whether the read worked. An empty `data` is "no entries for this
  /// account" only when this is ok (backlog 1a2b67c9).
  read: ReadState;
}>;

export async function loadEntriesForAccount(
  accountCode: string | null,
): Promise<EntriesPage> {
  if (!accountCode) return { data: [], capped: false, read: okRead };
  const probe = ENTRIES_PER_ACCOUNT_CAP + 1;
  const { read, body } = await readJson<LedgerEntry[]>(
    `${API_BASE}/entries?account_code=${accountCode}&limit=${probe}`,
  );
  const rows = body ?? [];
  if (rows.length > ENTRIES_PER_ACCOUNT_CAP) {
    return { data: rows.slice(0, ENTRIES_PER_ACCOUNT_CAP), capped: true, read };
  }
  return { data: rows, capped: false, read };
}

export function loadEntryDetail(entryId: string | null): Promise<LedgerEntryDetail | null> {
  if (!entryId) return Promise.resolve(null);
  return getJson<LedgerEntryDetail>(`${API_BASE}/entries/${entryId}`);
}

// ---------------------------------------------------------------------------
// IT-panel activity projections (bank settlements, payroll runs, tax filings)
// ---------------------------------------------------------------------------

export type BankSettlement = {
  id: string;
  invoice_id: string;
  received_on: string;
  expected_settle_on: string;
  settled_on: string | null;
  amount_cents: number;
  bank_provider: string;
  payment_method: 'ach' | 'wire' | 'check' | 'card';
  status: 'pending' | 'settled' | 'returned';
};

export type PayrollRun = {
  id: string;
  run_date: string;
  period_start: string;
  period_end: string;
  gross_cents: number;
  employer_tax_cents: number;
  withheld_cents: number;
  net_cents: number;
  employee_count: number;
  provider: string;
  status: 'draft' | 'submitted' | 'posted';
};

export async function loadBankSettlements(limit: number): Promise<BankSettlement[]> {
  const body = await getJson<BankSettlement[]>(
    `${API_BASE}/bank-settlements?limit=${limit}`,
  );
  return body ?? [];
}

export async function loadPayrollRuns(limit: number): Promise<PayrollRun[]> {
  const body = await getJson<PayrollRun[]>(
    `${API_BASE}/payroll-runs?limit=${limit}`,
  );
  return body ?? [];
}

export async function loadTaxFilings(
  status: 'accrued' | 'filed' | 'paid' | null,
): Promise<TaxFiling[]> {
  const qs = status ? `?status=${status}` : '';
  const body = await getJson<TaxFiling[]>(`${API_BASE}/tax-filings${qs}`);
  return body ?? [];
}

/// Lookup the single journal entry produced by a projection row.
/// Two round trips: list entries by (source_table, source_id) →
/// fetch the first entry's detail.
export async function loadEntryBySource(
  sourceTable: string | null,
  sourceId: string | null,
): Promise<LedgerEntryDetail | null> {
  if (!sourceTable || !sourceId) return null;
  const qs = new URLSearchParams({
    source_table: sourceTable,
    source_id: sourceId,
    limit: '1',
  }).toString();
  const summaries = await getJson<LedgerEntry[]>(`${API_BASE}/entries?${qs}`);
  if (!summaries || summaries.length === 0) return null;
  return loadEntryDetail(summaries[0]!.id);
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

export async function lockPeriod(id: string, lockedBy: string): Promise<string> {
  const resp = await fetch(`${API_BASE}/periods/${id}/lock`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ locked_by: lockedBy }),
  });
  if (!resp.ok) throw new Error(`lock failed: ${resp.status} ${await resp.text()}`);
  const body = (await resp.json()) as { checksum: string };
  return body.checksum;
}

export async function unlockPeriod(id: string): Promise<void> {
  const resp = await fetch(`${API_BASE}/periods/${id}/unlock`, { method: 'POST' });
  if (!resp.ok) throw new Error(`unlock failed: ${resp.status} ${await resp.text()}`);
}

export async function createManualEntry(input: ManualEntryInput): Promise<ManualEntryResponse> {
  const resp = await fetch(`${API_BASE}/journal-entries`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(input),
  });
  if (!resp.ok) throw new Error(`${resp.status}: ${await resp.text()}`);
  return resp.json();
}

export async function reverseEntry(
  source: LedgerEntryDetail,
): Promise<ManualEntryResponse> {
  const today = appToday();
  const lines: ReadonlyArray<ManualEntryLineInput> = source.lines.map((l) => ({
    account_code: l.account_code,
    debit_cents: l.credit_cents,
    credit_cents: l.debit_cents,
    memo: l.memo,
  }));
  const memo =
    `Reverses entry ${source.id}` + (source.memo ? ` — ${source.memo}` : '');
  return createManualEntry({ posted_on: today, memo, created_by: null, lines });
}

/// Format a USD cent value for display. Delegates to the shared
/// `formatMoney` helper so finance tables follow the same
/// precision conventions as the rest of the SPA.
///
/// - `formatUsd(192_000_00)` → `"$192,000"` (auto: ≥$100 rounds)
/// - `formatUsd(4_250)` → `"$42.50"` (auto: <$100 keeps cents)
/// - `formatUsd(192_000_00, 'cents')` → `"$192,000.00"` (force cents)
///
/// Pass `'cents'` on entry-detail / edit views where the cent
/// precision is load-bearing; the default is the right shape for
/// listings, tiles, and totals where cents add visual noise.
export function formatUsd(
  amount_cents: number,
  precision: 'auto' | 'cents' | 'whole' = 'auto',
): string {
  if (!Number.isFinite(amount_cents)) return '—';
  return formatMoney(
    { amount_cents, currency: 'USD' },
    { precision },
  );
}
