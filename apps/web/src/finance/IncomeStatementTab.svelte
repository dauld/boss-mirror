<script lang="ts">
  // Income statement — port of apps/web/src/finance/IncomeStatementTab.tsx.

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
    loadIncomeStatement,
    type IncomeStatement,
    type StatementLine,
  } from './ledger';
  import { appNow, appToday } from '@boss/web-kit/sim-clock';

  function startOfYearISO(): string {
    const d = appNow();
    return `${d.getUTCFullYear()}-01-01`;
  }

  let from = $state(startOfYearISO());
  let to = $state(appToday());
  let data = $state<IncomeStatement | null>(null);
  let loading = $state(true);

  $effect(() => {
    const f = from;
    const t = to;
    let cancelled = false;
    loading = true;
    (async () => {
      const d = await loadIncomeStatement(f || null, t || null);
      if (!cancelled) {
        data = d;
        loading = false;
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

  function exportCsv(d: IncomeStatement): void {
    const flatten = (section: string, lines: ReadonlyArray<StatementLine>): Row[] =>
      lines.map((l) => ({
        section,
        account_code: l.account_code,
        account_name: l.account_name,
        amount_cents: l.amount_cents,
      }));
    const rows: Row[] = [
      ...flatten('Revenue', d.revenue),
      { section: 'Revenue', account_code: '', account_name: 'Total revenue', amount_cents: d.total_revenue_cents },
      ...flatten('COGS', d.cogs),
      { section: 'COGS', account_code: '', account_name: 'Total COGS', amount_cents: d.total_cogs_cents },
      { section: 'Gross profit', account_code: '', account_name: 'Gross profit', amount_cents: d.gross_profit_cents },
      ...flatten('Operating expenses', d.operating_expenses),
      { section: 'Operating expenses', account_code: '', account_name: 'Total operating expenses', amount_cents: d.total_operating_expenses_cents },
      { section: 'Net income', account_code: '', account_name: 'Net income', amount_cents: d.net_income_cents },
    ];
    const columns: ReadonlyArray<CsvColumn<Row>> = [
      { header: 'Section', value: (r) => r.section },
      { header: 'Account code', value: (r) => r.account_code },
      { header: 'Account name', value: (r) => r.account_name },
      { header: 'Amount', value: (r) => centsToDollars(r.amount_cents) },
      { header: 'Currency', value: () => d.currency },
    ];
    const filename = `income-statement-${dateStamp(d.from)}-to-${dateStamp(d.to)}.csv`;
    exportRows(filename, rows, columns);
  }
</script>

<div class="income-statement-tab finance-print-area">
  <Section title="Income statement">
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
          disabled={!data}
          onclick={() => data && exportCsv(data)}
          style="margin-left:auto"
        >
          Download CSV
        </button>
        <button
          type="button"
          class="secondary"
          disabled={!data}
          onclick={printReport}
        >
          Print / PDF
        </button>
      </div>

      {#if loading && !data}
        <p class="empty">Loading income statement…</p>
      {:else if !data}
        <p class="empty load-failed" role="alert">Ledger unavailable.</p>
      {:else}
        {@const d = data}
        <table class="tb-table">
          <tbody>
            <tr>
              <th colspan="2" style="padding-top:12px; font-weight:700">Revenue</th>
            </tr>
            {#if d.revenue.length === 0}
              <tr>
                <td colspan="2" style="padding-left:24px; color:var(--static); font-style:italic">
                  (no activity)
                </td>
              </tr>
            {:else}
              {#each d.revenue as l (l.account_code)}
                <tr>
                  <td style="padding-left:24px">
                    <span class="mono" style="margin-right:8px; color:var(--static)">
                      {l.account_code}
                    </span>
                    {l.account_name}
                  </td>
                  <td style="text-align:right">{formatUsd(l.amount_cents)}</td>
                </tr>
              {/each}
            {/if}
            <tr style="border-top:1px solid var(--hairline)">
              <td style="padding-left:12px; font-weight:600">Total revenue</td>
              <td style="text-align:right; font-weight:600">{formatUsd(d.total_revenue_cents)}</td>
            </tr>

            <tr>
              <th colspan="2" style="padding-top:12px; font-weight:700">Cost of goods sold</th>
            </tr>
            {#if d.cogs.length === 0}
              <tr>
                <td colspan="2" style="padding-left:24px; color:var(--static); font-style:italic">
                  (no activity)
                </td>
              </tr>
            {:else}
              {#each d.cogs as l (l.account_code)}
                <tr>
                  <td style="padding-left:24px">
                    <span class="mono" style="margin-right:8px; color:var(--static)">
                      {l.account_code}
                    </span>
                    {l.account_name}
                  </td>
                  <td style="text-align:right">{formatUsd(l.amount_cents)}</td>
                </tr>
              {/each}
            {/if}
            <tr style="border-top:1px solid var(--hairline)">
              <td style="padding-left:12px; font-weight:600">Total COGS</td>
              <td style="text-align:right; font-weight:600">{formatUsd(d.total_cogs_cents)}</td>
            </tr>

            <tr style="border-top:1px solid var(--hairline)">
              <td style="font-weight:700">Gross profit</td>
              <td style="text-align:right; font-weight:700">{formatUsd(d.gross_profit_cents)}</td>
            </tr>

            <tr>
              <th colspan="2" style="padding-top:12px; font-weight:700">Operating expenses</th>
            </tr>
            {#if d.operating_expenses.length === 0}
              <tr>
                <td colspan="2" style="padding-left:24px; color:var(--static); font-style:italic">
                  (no activity)
                </td>
              </tr>
            {:else}
              {#each d.operating_expenses as l (l.account_code)}
                <tr>
                  <td style="padding-left:24px">
                    <span class="mono" style="margin-right:8px; color:var(--static)">
                      {l.account_code}
                    </span>
                    {l.account_name}
                  </td>
                  <td style="text-align:right">{formatUsd(l.amount_cents)}</td>
                </tr>
              {/each}
            {/if}
            <tr style="border-top:1px solid var(--hairline)">
              <td style="padding-left:12px; font-weight:600">Total operating expenses</td>
              <td style="text-align:right; font-weight:600">{formatUsd(d.total_operating_expenses_cents)}</td>
            </tr>

            <tr style="border-top:1px solid var(--hairline)">
              <td style="font-weight:700">Net income</td>
              <td style="text-align:right; font-weight:700">{formatUsd(d.net_income_cents)}</td>
            </tr>
          </tbody>
        </table>
      {/if}
  </Section>
</div>
