<script lang="ts">
  // Overview tab — AR aging + AP aging + gross margin by product line.
  // Port of the OverviewTab sub-component from
  // apps/web/src/finance/FinancePage.tsx.

  import { revenueCategoryLabel } from './types';
  import {
    loadApAging,
    type ApAging,
    type ApAgingBucket,
    type ArAgingBucket,
    type CommerceSummary,
  } from './api';
  import {
    centsToDollars,
    dateStamp,
    exportRows,
    printReport,
    type CsvColumn,
  } from './csvExport';
  import { formatMoney } from '@boss/web-kit/ui/money';

  type Props = {
    summary: CommerceSummary | null;
    loading: boolean;
  };
  let { summary, loading }: Props = $props();

  let ap = $state<ApAging | null>(null);
  let apLoading = $state(true);

  $effect(() => {
    let cancelled = false;
    apLoading = true;
    (async () => {
      const d = await loadApAging();
      if (!cancelled) {
        ap = d;
        apLoading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function exportArAgingCsv(
    buckets: ReadonlyArray<ArAgingBucket>,
    currency: string,
  ): void {
    const columns: ReadonlyArray<CsvColumn<ArAgingBucket>> = [
      { header: 'Bucket', value: (b) => b.label },
      { header: 'Invoices', value: (b) => b.count },
      { header: 'Amount', value: (b) => centsToDollars(b.total_cents) },
      { header: 'Currency', value: () => currency },
    ];
    exportRows(`ar-aging-${dateStamp(null)}.csv`, buckets, columns);
  }

  function exportApAgingCsv(
    buckets: ReadonlyArray<ApAgingBucket>,
    currency: string,
  ): void {
    const columns: ReadonlyArray<CsvColumn<ApAgingBucket>> = [
      { header: 'Bucket', value: (b) => b.label },
      { header: 'Invoices', value: (b) => b.count },
      { header: 'Amount', value: (b) => centsToDollars(b.total_cents) },
      { header: 'Currency', value: () => currency },
    ];
    exportRows(`ap-aging-${dateStamp(null)}.csv`, buckets, columns);
  }

  function money(amount_cents: number, currency: string): string {
    return formatMoney({ amount_cents, currency });
  }
</script>

{#if loading && !summary}
  <p class="empty">Loading finance summary…</p>
{:else if !summary}
  <p class="empty load-failed" role="alert">Finance summary unavailable.</p>
{:else}
  {@const s = summary}
  {@const arTotalCount = s.ar_aging.reduce((acc, b) => acc + b.count, 0)}
  {@const currency = s.currency || 'USD'}
  {@const revTotalCents = s.total_revenue_ttm_cents}
  {@const cogsTotalCents = s.total_cogs_ttm_cents}
  {@const gmTotalCents = s.total_gross_margin_ttm_cents}
  {@const gmPct = revTotalCents > 0 ? (gmTotalCents / revTotalCents) * 100 : 0}

  <div class="tab-grid finance-print-area">
    <section class="tab-section tab-section-wide">
      <div style="display:flex; align-items:baseline; gap:12px; margin-bottom:6px">
        <h3 style="margin:0">Accounts receivable aging</h3>
        <button
          type="button"
          class="secondary"
          onclick={() => exportArAgingCsv(s.ar_aging, currency)}
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
          <tr><th>Bucket</th><th class="num">Invoices</th><th class="num">Amount</th></tr>
        </thead>
        <tbody>
          {#each s.ar_aging as b (b.label)}
            <tr>
              <td>{b.label}</td>
              <td class="num">{b.count.toLocaleString()}</td>
              <td class="num">{money(b.total_cents, currency)}</td>
            </tr>
          {/each}
          <tr style="font-weight:600">
            <td>Total</td>
            <td class="num">{arTotalCount.toLocaleString()}</td>
            <td class="num">{money(s.total_outstanding_cents, currency)}</td>
          </tr>
        </tbody>
      </table>
    </section>

    <section class="tab-section tab-section-wide">
      <div style="display:flex; align-items:baseline; gap:12px; margin-bottom:6px">
        <h3 style="margin:0">Accounts payable aging</h3>
        <button
          type="button"
          class="secondary"
          disabled={!ap}
          onclick={() => ap && exportApAgingCsv(ap.buckets, ap.currency)}
          style="margin-left:auto"
        >
          Download CSV
        </button>
      </div>
      {#if apLoading && !ap}
        <p class="empty">Loading AP aging…</p>
      {:else if !ap}
        <p class="empty load-failed" role="alert">AP aging unavailable.</p>
      {:else}
        {@const apData = ap}
        <table class="data-table">
          <thead>
            <tr><th>Bucket</th><th class="num">Invoices</th><th class="num">Amount</th></tr>
          </thead>
          <tbody>
            {#each apData.buckets as b (b.label)}
              <tr>
                <td>{b.label}</td>
                <td class="num">{b.count.toLocaleString()}</td>
                <td class="num">{money(b.total_cents, apData.currency)}</td>
              </tr>
            {/each}
            <tr style="font-weight:600">
              <td>Total</td>
              <td class="num">{apData.total_invoice_count.toLocaleString()}</td>
              <td class="num">{money(apData.total_outstanding_cents, apData.currency)}</td>
            </tr>
          </tbody>
        </table>
      {/if}
      <p class="empty" style="font-size:11px; margin-top:4px">
        Aging measured from invoice received_on. Due-date aging lands with the
        vendor-terms join in a follow-up.
      </p>
    </section>

    <section class="tab-section tab-section-wide">
      <h3>Gross margin by product line (trailing 12 months)</h3>
      <table class="data-table">
        <thead>
          <tr>
            <th>Category</th>
            <th class="num">Revenue</th>
            <th class="num">COGS</th>
            <th class="num">Gross margin</th>
            <th class="num">Margin %</th>
          </tr>
        </thead>
        <tbody>
          {#each s.revenue_ttm as m (m.category)}
            <tr>
              <td>
                {revenueCategoryLabel(m.category)}
              </td>
              <td class="num">{money(m.revenue_cents, currency)}</td>
              <td class="num">{money(m.cogs_cents, currency)}</td>
              <td class="num">{money(m.gross_margin_cents, currency)}</td>
              <td class="num">{m.margin_pct.toFixed(1)}%</td>
            </tr>
          {/each}
          <tr style="font-weight:600">
            <td>Total</td>
            <td class="num">{money(revTotalCents, currency)}</td>
            <td class="num">{money(cogsTotalCents, currency)}</td>
            <td class="num">{money(gmTotalCents, currency)}</td>
            <td class="num">{gmPct.toFixed(1)}%</td>
          </tr>
        </tbody>
      </table>
    </section>
  </div>
{/if}
