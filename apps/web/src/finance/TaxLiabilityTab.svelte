<script lang="ts">
  // Tax liability — port of apps/web/src/finance/TaxLiabilityTab.tsx.

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
    loadTaxLiability,
    type TaxFiling,
    type TaxLiabilityRow,
    type TaxLiabilitySummary,
  } from './ledger';

  const FILING_KIND_LABEL: Record<TaxFiling['kind'], string> = {
    sales: 'Sales tax',
    income: 'Income tax (estimated)',
    payroll_941: 'Payroll (Form 941)',
    payroll_940: 'Payroll (Form 940)',
  };

  const LIABILITY_DESCRIPTION: Record<string, string> = {
    '2150': 'Payroll withholdings + employer-side tax; drained quarterly (941)',
    '2300': 'Sales tax collected on invoices; drained monthly per jurisdiction',
    '2310': 'Estimated income tax; drained quarterly',
  };

  let data = $state<TaxLiabilitySummary | null>(null);
  let loading = $state(true);

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      const d = await loadTaxLiability();
      if (!cancelled) {
        data = d;
        loading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function exportCsv(rows: ReadonlyArray<TaxLiabilityRow>, currency: string): void {
    const columns: ReadonlyArray<CsvColumn<TaxLiabilityRow>> = [
      { header: 'Account code', value: (r) => r.account_code },
      { header: 'Account name', value: (r) => r.account_name },
      { header: 'Balance', value: (r) => centsToDollars(r.balance_cents) },
      { header: 'Currency', value: () => currency },
    ];
    exportRows(`tax-liability-${dateStamp(null)}.csv`, rows, columns);
  }
</script>

{#if loading && !data}
  <p class="empty">Loading tax liability…</p>
{:else if !data}
  <p class="empty">Tax liability unavailable.</p>
{:else}
  {@const d = data}
  {@const totalLiabilityCents = d.liabilities.reduce((s, r) => s + r.balance_cents, 0)}
  {@const sortedFilings = [...d.accrued_filings].sort((a, b) => a.due_on.localeCompare(b.due_on))}

  <div class="tax-liability-tab finance-print-area">
    <Section title="Outstanding tax liability">
        <div class="tb-controls" style="display:flex; gap:16px; flex-wrap:wrap">
          <span class="tb-asof">
            As of <strong>{d.as_of}</strong>
          </span>
          <button
            type="button"
            class="secondary"
            disabled={d.liabilities.length === 0}
            onclick={() => exportCsv(d.liabilities, d.currency)}
            style="margin-left:auto"
          >
            Download CSV
          </button>
          <button type="button" class="secondary" onclick={printReport}>
            Print / PDF
          </button>
        </div>

        <table class="data-table">
          <thead>
            <tr>
              <th>Account</th>
              <th>Description</th>
              <th class="num">Balance</th>
            </tr>
          </thead>
          <tbody>
            {#each d.liabilities as r (r.account_code)}
              <tr>
                <td class="mono">{r.account_code} · {r.account_name}</td>
                <td style="color:var(--static); font-size:13px">
                  {LIABILITY_DESCRIPTION[r.account_code] ?? ''}
                </td>
                <td class="num">{formatUsd(r.balance_cents)}</td>
              </tr>
            {/each}
            <tr style="font-weight:600">
              <td>Total</td>
              <td></td>
              <td class="num">{formatUsd(totalLiabilityCents)}</td>
            </tr>
          </tbody>
        </table>
        {#if d.next_due}
          <p class="empty" style="font-size:12px; margin-top:8px">
            <strong>Next due:</strong> {FILING_KIND_LABEL[d.next_due.kind]}
            · {d.next_due.jurisdiction}
            · {formatUsd(d.next_due.amount_cents)}
            by {d.next_due.due_on}
          </p>
        {/if}
    </Section>

    <Section title={`Accrued filings (${d.accrued_filings.length})`}>
        {#if d.accrued_filings.length === 0}
          <p class="empty">
            No filings awaiting remittance. The tax-authorities generator sweeps
            sales tax on the 20th of each month and payroll-941 on the 15th of
            Jan / Apr / Jul / Oct.
          </p>
        {:else}
          <table class="data-table data-table-striped">
            <thead>
              <tr>
                <th>Filing</th>
                <th>Kind</th>
                <th>Jurisdiction</th>
                <th>Period</th>
                <th>Due</th>
                <th class="num">Amount</th>
              </tr>
            </thead>
            <tbody>
              {#each sortedFilings as f (f.id)}
                <tr>
                  <td class="mono">{f.id}</td>
                  <td>{FILING_KIND_LABEL[f.kind]}</td>
                  <td>{f.jurisdiction}</td>
                  <td>{f.period_start} → {f.period_end}</td>
                  <td>{f.due_on}</td>
                  <td class="num">{formatUsd(f.amount_cents)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
    </Section>
  </div>
{/if}
