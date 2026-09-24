<script lang="ts">
  // Cash flow statement — port of apps/web/src/finance/CashFlowTab.tsx.
  //
  // Two presentations behind a Direct / Indirect toggle:
  //  - Indirect (default): GL-attribution statement — net income +
  //    operating/investing/financing sections derived from the journal.
  //  - Direct (?method=direct): the four real-world cash buckets summed
  //    straight off financial_facts, with a GL cash-pool reconciliation.

  import Section from '@boss/web-kit/ui/Section.svelte';
  import {
    centsToDollars,
    dateStamp,
    exportRows,
    printReport,
    type CsvColumn,
  } from './csvExport';
  import {
    formatUsd,
    loadCashFlow,
    loadCashFlowDirect,
    type CashFlowStatement,
    type DirectCashFlowStatement,
    type StatementLine,
  } from './ledger';
  import { appNow, appToday } from '@boss/web-kit/sim-clock';

  function startOfYearISO(): string {
    const d = appNow();
    return `${d.getUTCFullYear()}-01-01`;
  }

  type Method = 'indirect' | 'direct';

  let method = $state<Method>('indirect');
  let from = $state(startOfYearISO());
  let to = $state(appToday());

  let data = $state<CashFlowStatement | null>(null);
  let loading = $state(true);
  let directData = $state<DirectCashFlowStatement | null>(null);
  let directLoading = $state(false);

  // Indirect fetch — only runs while the Indirect tab is selected so we
  // don't keep round-tripping the GL-attribution query in the
  // background while the operator reads the Direct view.
  $effect(() => {
    if (method !== 'indirect') return;
    const f = from;
    const t = to;
    let cancelled = false;
    loading = true;
    (async () => {
      const d = await loadCashFlow(f || null, t || null);
      if (!cancelled) {
        data = d;
        loading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  // Direct fetch — mirror of the above for the cash-events presentation.
  $effect(() => {
    if (method !== 'direct') return;
    const f = from;
    const t = to;
    let cancelled = false;
    directLoading = true;
    (async () => {
      const d = await loadCashFlowDirect(f || null, t || null);
      if (!cancelled) {
        directData = d;
        directLoading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  type Row = {
    section: string;
    account_code: string;
    account_name: string;
    amount_cents: number;
  };

  function exportCsv(d: CashFlowStatement): void {
    const flatten = (section: string, lines: ReadonlyArray<StatementLine>): Row[] =>
      lines.map((l) => ({
        section,
        account_code: l.account_code,
        account_name: l.account_name,
        amount_cents: l.amount_cents,
      }));
    const rows: Row[] = [
      { section: 'Operating activities', account_code: '', account_name: 'Net income', amount_cents: d.net_income_cents },
      ...flatten('Operating activities', d.operating_activities),
      ...flatten('Operating activities — working capital', d.working_capital_adjustments),
      ...flatten('Operating activities — non-cash', d.non_cash_adjustments),
      { section: 'Operating activities', account_code: '', account_name: 'Cash from operating activities', amount_cents: d.cash_from_operations_cents },
      ...flatten('Investing activities', d.investing_activities),
      { section: 'Investing activities', account_code: '', account_name: 'Cash from investing activities', amount_cents: d.cash_from_investing_cents },
      ...flatten('Financing activities', d.financing_activities),
      { section: 'Financing activities', account_code: '', account_name: 'Cash from financing activities', amount_cents: d.cash_from_financing_cents },
      { section: 'Total', account_code: '', account_name: 'Net change in cash', amount_cents: d.net_change_in_cash_cents },
      { section: 'Total', account_code: '', account_name: 'Cash at start of period', amount_cents: d.cash_start_cents },
      { section: 'Total', account_code: '', account_name: 'Cash at end of period', amount_cents: d.cash_end_cents },
    ];
    const columns: ReadonlyArray<CsvColumn<Row>> = [
      { header: 'Section', value: (r) => r.section },
      { header: 'Account code', value: (r) => r.account_code },
      { header: 'Account name', value: (r) => r.account_name },
      { header: 'Amount', value: (r) => centsToDollars(r.amount_cents) },
      { header: 'Currency', value: () => d.currency },
    ];
    const filename = `cash-flow-${dateStamp(d.from)}-to-${dateStamp(d.to)}.csv`;
    exportRows(filename, rows, columns);
  }

  function exportDirectCsv(d: DirectCashFlowStatement): void {
    const rows: Row[] = [
      { section: 'Operating cash flows', account_code: '', account_name: 'Cash in from customers', amount_cents: d.cash_in_from_customers_cents },
      { section: 'Operating cash flows', account_code: '', account_name: 'Cash out to vendors', amount_cents: -d.cash_out_to_vendors_cents },
      { section: 'Operating cash flows', account_code: '', account_name: 'Cash out to employees', amount_cents: -d.cash_out_to_employees_cents },
      { section: 'Operating cash flows', account_code: '', account_name: 'Cash out to authorities', amount_cents: -d.cash_out_to_authorities_cents },
      { section: 'Total', account_code: '', account_name: 'Net change in cash', amount_cents: d.net_change_in_cash_cents },
    ];
    const columns: ReadonlyArray<CsvColumn<Row>> = [
      { header: 'Section', value: (r) => r.section },
      { header: 'Account name', value: (r) => r.account_name },
      { header: 'Amount', value: (r) => centsToDollars(r.amount_cents) },
      { header: 'Currency', value: () => d.currency },
    ];
    const filename = `cash-flow-direct-${dateStamp(d.from)}-to-${dateStamp(d.to)}.csv`;
    exportRows(filename, rows, columns);
  }

  function downloadActive(): void {
    if (method === 'direct') {
      if (directData) exportDirectCsv(directData);
    } else if (data) {
      exportCsv(data);
    }
  }

  const downloadDisabled = $derived(method === 'direct' ? !directData : !data);
</script>

<div class="cash-flow-tab finance-print-area">
  <Section title="Cash flow statement">
      <div
        class="cf-method-toggle"
        role="tablist"
        aria-label="Cash flow method"
        style="display:flex; gap:4px; margin-bottom:12px"
      >
        <button
          type="button"
          role="tab"
          aria-selected={method === 'indirect'}
          class={method === 'indirect' ? '' : 'secondary'}
          onclick={() => (method = 'indirect')}
        >
          Indirect
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={method === 'direct'}
          class={method === 'direct' ? '' : 'secondary'}
          onclick={() => (method = 'direct')}
        >
          Direct
        </button>
      </div>

      <div class="tb-controls" style="display:flex; gap:16px; flex-wrap:wrap">
        <label class="tb-asof">
          From
          <input type="date" bind:value={from} />
        </label>
        <label class="tb-asof">
          To
          <input type="date" bind:value={to} />
        </label>
        <button
          type="button"
          class="secondary"
          disabled={downloadDisabled}
          onclick={downloadActive}
          style="margin-left:auto"
        >
          Download CSV
        </button>
        <button
          type="button"
          class="secondary"
          disabled={downloadDisabled}
          onclick={printReport}
        >
          Print / PDF
        </button>
      </div>

      {#if method === 'indirect'}
        {#if loading && !data}
          <p class="empty">Loading cash flow…</p>
        {:else if !data}
          <p class="empty load-failed" role="alert">Ledger unavailable.</p>
        {:else}
          {@const d = data}
          {#if !d.reconciled}
            <div
              role="alert"
              style="margin:8px 0; padding:10px 14px; border:1px solid var(--busy); background:var(--warn-wash); border-radius:6px; font-size:13px; color:var(--warn)"
            >
              <strong>Reconciliation gap: {formatUsd(d.reconciliation_gap_cents)}</strong>
              — the calculated cash change doesn't match the actual cash-account delta.
              Usually means a working-capital bucket outside AR / AP / Inventory is
              driving cash; promoting more accounts into the adjustments section
              (or adding a missing account mapping) fixes this.
            </div>
          {/if}

          <table class="tb-table">
            <tbody>
              <tr>
                <th colspan="2" style="padding-top:12px; font-weight:700">Operating activities</th>
              </tr>
              <tr style="border-top:1px solid var(--hairline)">
                <td style="padding-left:12px; font-weight:600">Net income</td>
                <td style="text-align:right; font-weight:600">{formatUsd(d.net_income_cents)}</td>
              </tr>
              {#if d.operating_activities.length > 0}
                <tr>
                  <td colspan="2" style="padding-left:12px; color:var(--static); font-style:italic; font-size:12px">
                    Cash from operating activities (by source):
                  </td>
                </tr>
                {#each d.operating_activities as l (l.account_code)}
                  <tr>
                    <td style="padding-left:24px">
                      <span class="mono" style="margin-right:8px; color:var(--static)">{l.account_code}</span>
                      {l.account_name}
                    </td>
                    <td style="text-align:right">{formatUsd(l.amount_cents)}</td>
                  </tr>
                {/each}
              {/if}
              {#if d.working_capital_adjustments.length > 0}
                <tr>
                  <td colspan="2" style="padding-left:12px; color:var(--static); font-style:italic; font-size:12px">
                    Adjustments for changes in working capital:
                  </td>
                </tr>
                {#each d.working_capital_adjustments as l (l.account_code)}
                  <tr>
                    <td style="padding-left:24px">
                      <span class="mono" style="margin-right:8px; color:var(--static)">{l.account_code}</span>
                      {l.account_name}
                    </td>
                    <td style="text-align:right">{formatUsd(l.amount_cents)}</td>
                  </tr>
                {/each}
              {/if}
              {#if d.non_cash_adjustments.length > 0}
                <tr>
                  <td colspan="2" style="padding-left:12px; color:var(--static); font-style:italic; font-size:12px">
                    Non-cash charges:
                  </td>
                </tr>
                {#each d.non_cash_adjustments as l (l.account_code)}
                  <tr>
                    <td style="padding-left:24px">
                      <span class="mono" style="margin-right:8px; color:var(--static)">{l.account_code}</span>
                      {l.account_name}
                    </td>
                    <td style="text-align:right">{formatUsd(l.amount_cents)}</td>
                  </tr>
                {/each}
              {/if}
              <tr style="border-top:1px solid var(--hairline)">
                <td style="font-weight:700">Cash from operating activities</td>
                <td style="text-align:right; font-weight:700">{formatUsd(d.cash_from_operations_cents)}</td>
              </tr>

              <tr>
                <th colspan="2" style="padding-top:12px; font-weight:700">Investing activities</th>
              </tr>
              {#if d.investing_activities.length === 0}
                <tr>
                  <td colspan="2" style="padding-left:24px; color:var(--static); font-style:italic">(none)</td>
                </tr>
              {:else}
                {#each d.investing_activities as l (l.account_code)}
                  <tr>
                    <td style="padding-left:24px">
                      <span class="mono" style="margin-right:8px; color:var(--static)">{l.account_code}</span>
                      {l.account_name}
                    </td>
                    <td style="text-align:right">{formatUsd(l.amount_cents)}</td>
                  </tr>
                {/each}
              {/if}
              <tr style="border-top:1px solid var(--hairline)">
                <td style="font-weight:700">Cash from investing activities</td>
                <td style="text-align:right; font-weight:700">{formatUsd(d.cash_from_investing_cents)}</td>
              </tr>

              <tr>
                <th colspan="2" style="padding-top:12px; font-weight:700">Financing activities</th>
              </tr>
              {#if d.financing_activities.length === 0}
                <tr>
                  <td colspan="2" style="padding-left:24px; color:var(--static); font-style:italic">(none)</td>
                </tr>
              {:else}
                {#each d.financing_activities as l (l.account_code)}
                  <tr>
                    <td style="padding-left:24px">
                      <span class="mono" style="margin-right:8px; color:var(--static)">{l.account_code}</span>
                      {l.account_name}
                    </td>
                    <td style="text-align:right">{formatUsd(l.amount_cents)}</td>
                  </tr>
                {/each}
              {/if}
              <tr style="border-top:1px solid var(--hairline)">
                <td style="font-weight:700">Cash from financing activities</td>
                <td style="text-align:right; font-weight:700">{formatUsd(d.cash_from_financing_cents)}</td>
              </tr>

              <tr style="border-top:1px solid var(--hairline)">
                <td style="font-weight:700">Net change in cash</td>
                <td style="text-align:right; font-weight:700">{formatUsd(d.net_change_in_cash_cents)}</td>
              </tr>
              <tr style="border-top:1px solid var(--hairline)">
                <td style="padding-left:12px; font-weight:600">Cash at start of period</td>
                <td style="text-align:right; font-weight:600">{formatUsd(d.cash_start_cents)}</td>
              </tr>
              <tr style="border-top:1px solid var(--hairline)">
                <td style="padding-left:12px; font-weight:600">Cash at end of period</td>
                <td style="text-align:right; font-weight:600">{formatUsd(d.cash_end_cents)}</td>
              </tr>
            </tbody>
          </table>
        {/if}
      {:else}
        {#if directLoading && !directData}
          <p class="empty">Loading cash flow…</p>
        {:else if !directData}
          <p class="empty load-failed" role="alert">Ledger unavailable.</p>
        {:else}
          {@const d = directData}
          {#if !d.reconciled}
            <div
              role="alert"
              style="margin:8px 0; padding:10px 14px; border:1px solid var(--busy); background:var(--warn-wash); border-radius:6px; font-size:13px; color:var(--warn)"
            >
              <strong>Reconciliation gap: {formatUsd(d.reconciliation_gap_cents)}</strong>
              — the four cash buckets don't match the GL cash-pool (1000 + 1010)
              delta of {formatUsd(d.gl_cash_pool_delta_cents)}. Usually means cash
              moved via a path outside the tracked events (a same-day invoice
              collection or a manual cash entry).
            </div>
          {/if}

          <table class="tb-table">
            <tbody>
              <tr>
                <th colspan="2" style="padding-top:12px; font-weight:700">Operating cash flows</th>
              </tr>
              <tr style="border-top:1px solid var(--hairline)">
                <td style="padding-left:12px">Cash in from customers</td>
                <td style="text-align:right">{formatUsd(d.cash_in_from_customers_cents)}</td>
              </tr>
              <tr>
                <td style="padding-left:12px">Cash out to vendors</td>
                <td style="text-align:right">({formatUsd(d.cash_out_to_vendors_cents)})</td>
              </tr>
              <tr>
                <td style="padding-left:12px">Cash out to employees</td>
                <td style="text-align:right">({formatUsd(d.cash_out_to_employees_cents)})</td>
              </tr>
              <tr>
                <td style="padding-left:12px">Cash out to authorities</td>
                <td style="text-align:right">({formatUsd(d.cash_out_to_authorities_cents)})</td>
              </tr>
              <tr style="border-top:1px solid var(--hairline)">
                <td style="font-weight:700">Net change in cash</td>
                <td style="text-align:right; font-weight:700">{formatUsd(d.net_change_in_cash_cents)}</td>
              </tr>
              <tr style="border-top:1px solid var(--hairline)">
                <td style="padding-left:12px; color:var(--static); font-size:12px">
                  GL cash-pool delta (1000 + 1010), for reconciliation
                </td>
                <td style="text-align:right; color:var(--static); font-size:12px">
                  {formatUsd(d.gl_cash_pool_delta_cents)}
                </td>
              </tr>
            </tbody>
          </table>
        {/if}
      {/if}
  </Section>
</div>
