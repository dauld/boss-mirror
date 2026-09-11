<script lang="ts">
  // Finance dashboard — port of apps/web/src/finance/FinancePage.tsx.
  //
  // Eight tabs: Overview (AR/AP aging + margins), Invoices (filterable
  // list), PO Approvals (draft POs), and the five ledger-derived
  // financial statements. New Invoice / New JE actions render unless
  // the user is an auditor.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { formatMoney } from '@boss/web-kit/ui/money';
  import IncomeStatementTab from './IncomeStatementTab.svelte';
  import BalanceSheetTab from './BalanceSheetTab.svelte';
  import CashFlowTab from './CashFlowTab.svelte';
  import TrialBalanceTab from './TrialBalanceTab.svelte';
  import TaxLiabilityTab from './TaxLiabilityTab.svelte';
  import OverviewTab from './OverviewTab.svelte';
  import InvoicesTab from './InvoicesTab.svelte';
  import ApprovalsTab from './ApprovalsTab.svelte';
  import MonthlyClosePackageButton from './MonthlyClosePackageButton.svelte';
  import type { Invoice } from './types';
  import {
    loadInvoices,
    loadCommerceSummary,
    type CommerceSummary,
  } from './api';
  import type { PagedResult } from '../data/paginated';
  import { href } from '../router';
  import { session } from '@boss/web-kit/session/session.svelte';

  type Tab =
    | 'overview'
    | 'invoices'
    | 'approvals'
    | 'income-statement'
    | 'balance-sheet'
    | 'cash-flow'
    | 'trial-balance'
    | 'tax-liability';

  const PAGE_TABS: ReadonlyArray<{ id: Tab; label: string }> = [
    { id: 'overview', label: 'Overview' },
    { id: 'invoices', label: 'Invoices' },
    { id: 'approvals', label: 'PO Approvals' },
    { id: 'income-statement', label: 'Income statement' },
    { id: 'balance-sheet', label: 'Balance sheet' },
    { id: 'cash-flow', label: 'Cash flow' },
    { id: 'trial-balance', label: 'Trial Balance' },
    { id: 'tax-liability', label: 'Tax liability' },
  ];

  let tab = $state<Tab>('overview');

  /// The invoice list as loaded — failed is its own state, rendered
  /// by InvoicesTab, so an outage never reads as "no invoices"
  /// (packet 3fba9c35).
  let invoicesResult = $state<PagedResult<Invoice> | null>(null);
  let summary = $state<CommerceSummary | null>(null);
  let summaryLoading = $state(true);

  let invoices = $derived(
    invoicesResult?.kind === 'ready' ? invoicesResult.page.data : [],
  );
  let invoicesError = $derived(
    invoicesResult?.kind === 'failed' ? invoicesResult.error : null,
  );

  let readOnly = $derived(
    session.value.kind === 'ready' && session.value.user.role === 'auditor',
  );

  $effect(() => {
    let cancelled = false;
    summaryLoading = true;
    const load = async () => {
      const [inv, s] = await Promise.all([loadInvoices(), loadCommerceSummary()]);
      if (!cancelled) {
        invoicesResult = inv;
        summary = s;
        summaryLoading = false;
      }
    };
    void load();
    const timer = window.setInterval(() => void load(), 30_000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  });

  function glance(cents: number): string {
    return formatMoney({ amount_cents: cents, currency: 'USD' }, { precision: 'compact' });
  }

  let headline = $derived.by(() => {
    if (!summary) {
      return {
        title: 'Finance',
        subtitle: summaryLoading ? 'Loading…' : 'Summary unavailable',
      };
    }
    const marginPct =
      summary.total_revenue_ttm_cents > 0
        ? (summary.total_gross_margin_ttm_cents / summary.total_revenue_ttm_cents) * 100
        : 0;
    return {
      title: `${glance(summary.total_revenue_ttm_cents)} trailing revenue`,
      subtitle:
        `${glance(summary.total_gross_margin_ttm_cents)} gross margin ` +
        `(${marginPct.toFixed(1)}%) · ` +
        `${glance(summary.total_outstanding_cents)} receivables outstanding`,
    };
  });
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Finance"
    title={headline.title}
    subtitle={headline.subtitle}
  />

  <div class="finance-actions" style="display:flex; gap:8px; align-items:flex-start; flex-wrap:wrap">
    {#if !readOnly}
      <Link to={href('/ux/finance/new')} className="fin-new-invoice">
        + New invoice
      </Link>
      <Link to={href('/ux/finance/journal-entries/new')} className="fin-new-invoice">
        + New journal entry
      </Link>
    {/if}
    <MonthlyClosePackageButton />
  </div>

  <nav class="tabs" role="tablist">
    {#each PAGE_TABS as t (t.id)}
      <button
        type="button"
        role="tab"
        aria-selected={tab === t.id}
        class="tab {tab === t.id ? 'tab-active' : ''}"
        onclick={() => (tab = t.id)}
      >
        {t.label}
      </button>
    {/each}
  </nav>

  <div class="tab-panel">
    {#if tab === 'overview'}
      <OverviewTab {summary} loading={summaryLoading} />
    {:else if tab === 'invoices'}
      <InvoicesTab
        {invoices}
        loadError={invoicesError}
        totalCount={summary?.total_invoice_count ?? invoices.length}
      />
    {:else if tab === 'approvals'}
      <ApprovalsTab />
    {:else if tab === 'income-statement'}
      <IncomeStatementTab />
    {:else if tab === 'balance-sheet'}
      <BalanceSheetTab />
    {:else if tab === 'cash-flow'}
      <CashFlowTab />
    {:else if tab === 'trial-balance'}
      <TrialBalanceTab />
    {:else if tab === 'tax-liability'}
      <TaxLiabilityTab />
    {/if}
  </div>
</div>

