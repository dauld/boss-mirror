<script lang="ts">
  // Balance sheet — port of apps/web/src/finance/BalanceSheetTab.tsx.

  import Section from '@boss/web-kit/ui/Section.svelte';
  import DeferredRevenueRunoffPanel from './DeferredRevenueRunoffPanel.svelte';
  import {
    centsToDollars,
    dateStamp,
    exportRows,
    printReport,
    type CsvColumn,
  } from './csvExport';
  import {
    formatUsd,
    loadBalanceSheet,
    type BalanceSheet,
    type StatementLine,
  } from './ledger';
  import { appToday } from '@boss/web-kit/sim-clock';

  let asOf = $state(appToday());
  let data = $state<BalanceSheet | null>(null);
  let loading = $state(true);

  $effect(() => {
    const a = asOf;
    let cancelled = false;
    loading = true;
    (async () => {
      const d = await loadBalanceSheet(a || null);
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

  function exportCsv(d: BalanceSheet): void {
    const flatten = (section: string, lines: ReadonlyArray<StatementLine>): Row[] =>
      lines.map((l) => ({
        section,
        account_code: l.account_code,
        account_name: l.account_name,
        amount_cents: l.amount_cents,
      }));
    const rows: Row[] = [
      ...flatten('Assets', d.assets),
      { section: 'Assets', account_code: '', account_name: 'Total assets', amount_cents: d.total_assets_cents },
      ...flatten('Liabilities', d.liabilities),
      { section: 'Liabilities', account_code: '', account_name: 'Total liabilities', amount_cents: d.total_liabilities_cents },
      ...flatten('Equity', d.equity),
      { section: 'Equity', account_code: '', account_name: 'Total equity', amount_cents: d.total_equity_cents },
      {
        section: 'Total',
        account_code: '',
        account_name: 'Total liabilities + equity',
        amount_cents: d.total_liabilities_cents + d.total_equity_cents,
      },
    ];
    const columns: ReadonlyArray<CsvColumn<Row>> = [
      { header: 'Section', value: (r) => r.section },
      { header: 'Account code', value: (r) => r.account_code },
      { header: 'Account name', value: (r) => r.account_name },
      { header: 'Amount', value: (r) => centsToDollars(r.amount_cents) },
      { header: 'Currency', value: () => d.currency },
    ];
    exportRows(`balance-sheet-${dateStamp(d.as_of)}.csv`, rows, columns);
  }
</script>

<div class="balance-sheet-tab finance-print-area">
  <Section title="Balance sheet">
      <div class="tb-controls" style="display:flex; gap:16px; flex-wrap:wrap">
        <label class="tb-asof">
          As of
          <input type="date" bind:value={asOf} />
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
        <p class="empty">Loading balance sheet…</p>
      {:else if !data}
        <p class="empty load-failed" role="alert">Ledger unavailable.</p>
      {:else}
        {@const d = data}
        {#if !d.balanced}
          <div
            role="alert"
            style="margin:8px 0; padding:10px 14px; border:1px solid var(--troubled); background:var(--err-wash); border-radius:6px; font-size:13px; color:var(--err)"
          >
            <strong>Imbalance: {formatUsd(d.imbalance_cents)}</strong>
            — the accounting equation isn't holding. Check for a recent miscoded
            account or a posting rule that touches only one side.
          </div>
        {/if}

        <table class="tb-table">
          <tbody>
            <tr>
              <th colspan="2" style="padding-top:12px; font-weight:700">Assets</th>
            </tr>
            {#if d.assets.length === 0}
              <tr>
                <td colspan="2" style="padding-left:24px; color:var(--static); font-style:italic">(none)</td>
              </tr>
            {:else}
              {#each d.assets as l (l.account_code)}
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
              <td style="font-weight:700">Total assets</td>
              <td style="text-align:right; font-weight:700">{formatUsd(d.total_assets_cents)}</td>
            </tr>

            <tr><td colspan="2" style="padding-top:8px"></td></tr>

            <tr>
              <th colspan="2" style="padding-top:12px; font-weight:700">Liabilities</th>
            </tr>
            {#if d.liabilities.length === 0}
              <tr>
                <td colspan="2" style="padding-left:24px; color:var(--static); font-style:italic">(none)</td>
              </tr>
            {:else}
              {#each d.liabilities as l (l.account_code)}
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
              <td style="padding-left:12px; font-weight:600">Total liabilities</td>
              <td style="text-align:right; font-weight:600">{formatUsd(d.total_liabilities_cents)}</td>
            </tr>

            <tr>
              <th colspan="2" style="padding-top:12px; font-weight:700">Equity</th>
            </tr>
            {#if d.equity.length === 0}
              <tr>
                <td colspan="2" style="padding-left:24px; color:var(--static); font-style:italic">(none)</td>
              </tr>
            {:else}
              {#each d.equity as l (l.account_code)}
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
              <td style="padding-left:12px; font-weight:600">Total equity</td>
              <td style="text-align:right; font-weight:600">{formatUsd(d.total_equity_cents)}</td>
            </tr>

            <tr style="border-top:1px solid var(--hairline)">
              <td style="font-weight:700">Total liabilities + equity</td>
              <td style="text-align:right; font-weight:700">
                {formatUsd(d.total_liabilities_cents + d.total_equity_cents)}
              </td>
            </tr>
          </tbody>
        </table>
      {/if}
  </Section>

  <div style="margin-top:24px">
    <DeferredRevenueRunoffPanel {asOf} />
  </div>
</div>
