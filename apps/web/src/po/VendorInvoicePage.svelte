<script lang="ts">
  // Vendor-invoice (bill) detail. Sibling of PoPage.svelte — same
  // inventory/procurement domain, same data source.
  //
  // There is no single-vendor-invoice GET endpoint (boss-inventory
  // exposes only the list at GET /api/inventory/vendor-invoices —
  // crates/modules/boss-inventory/src/http.rs:100), so we load the
  // list and select by id, exactly as PoPage selects a PO from
  // /api/inventory/orders. Structure + loading/error states mirror
  // the invoice detail page (finance/InvoicePage.svelte).

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import Meta from '@boss/web-kit/ui/Meta.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { formatMoney } from '@boss/web-kit/ui/money';
  import type { VendorInvoice } from '../vendors/types';
  import { href } from '../router';

  type Props = { vendorInvoiceId: string };
  let { vendorInvoiceId }: Props = $props();

  type FetchState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'notfound' }
    | { kind: 'ready'; bill: VendorInvoice };

  let fetchState: FetchState = $state<FetchState>({ kind: 'loading' });

  $effect(() => {
    const id = decodeURIComponent(vendorInvoiceId);
    let cancelled = false;
    fetchState = { kind: 'loading' };
    (async () => {
      try {
        const resp = await fetch('/api/inventory/vendor-invoices');
        if (cancelled) return;
        if (!resp.ok) throw new Error(`inventory API: ${resp.status}`);
        const body = await resp.json();
        const bills: VendorInvoice[] = Array.isArray(body) ? body : (body.data ?? []);
        const bill = bills.find((vi) => vi.id === id);
        fetchState = bill ? { kind: 'ready', bill } : { kind: 'notfound' };
      } catch (e) {
        if (!cancelled) {
          fetchState = {
            kind: 'error',
            message: e instanceof Error ? e.message : String(e),
          };
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  });
</script>

<div class="detail-page theme-exec">
  <Breadcrumb to={href('/ux/warehouse')}>
    ← Warehouse
  </Breadcrumb>

  {#if fetchState.kind === 'loading'}
    <p class="empty">Loading vendor invoice…</p>
  {:else if fetchState.kind === 'notfound'}
    <header class="detail-hero">
      <h1 class="detail-title">Vendor invoice not found</h1>
    </header>
    <p class="empty">No vendor-invoice record for <code>{decodeURIComponent(vendorInvoiceId)}</code>.</p>
  {:else if fetchState.kind === 'error'}
    <header class="detail-hero">
      <h1 class="detail-title">Failed to load vendor invoice</h1>
    </header>
    <p class="empty load-failed" role="alert">{fetchState.message}</p>
  {:else}
    {@const bill = fetchState.bill}
    {@const moneyFmt = (c: number) => formatMoney({ amount_cents: c, currency: bill.currency })}

    <header class="detail-hero">
      <div>
        <div class="detail-eyebrow">
          <EntityLink kind="vendor-invoice" id={bill.id} label={bill.vendor_invoice_no} />
          · {bill.status}
          {#if bill.discrepancy_kind}
            · <span>{bill.discrepancy_kind}</span>
          {/if}
        </div>
        <h1 class="detail-title">{moneyFmt(bill.amount_cents)}</h1>
        <div class="detail-tagline">
          <EntityLink kind="vendor" id={bill.vendor} />
          · against <EntityLink kind="po" id={bill.po_id} />
        </div>
        <div class="detail-meta">
          <Meta label="Received">{bill.received_on}</Meta>
          <Meta label="Matched">{bill.matched_on ?? '—'}</Meta>
          <Meta label="Approved">{bill.approved_on ?? '—'}</Meta>
          <Meta label="Paid">{bill.paid_on ?? '—'}</Meta>
        </div>
      </div>
    </header>

    <div class="tab-grid">
      <Section title="Invoice">
          <dl class="kv">
            <dt>Invoice #</dt>
            <dd>
              <EntityLink kind="vendor-invoice" id={bill.id} label={bill.vendor_invoice_no} />
            </dd>
            <dt>BOSS ID</dt><dd><EntityLink kind="vendor-invoice" id={bill.id} /></dd>
            <dt>Vendor</dt><dd><EntityLink kind="vendor" id={bill.vendor} /></dd>
            <dt>PO</dt><dd><EntityLink kind="po" id={bill.po_id} /></dd>
            <dt>Status</dt><dd>{bill.status}</dd>
            <dt>Amount</dt><dd>{moneyFmt(bill.amount_cents)}</dd>
            <dt>Received</dt><dd>{bill.received_on}</dd>
            <dt>Matched</dt><dd>{bill.matched_on ?? '—'}</dd>
            <dt>Approved</dt><dd>{bill.approved_on ?? '—'}</dd>
            <dt>Paid</dt><dd>{bill.paid_on ?? '—'}</dd>
          </dl>
      </Section>

      <Section title="3-way match">
          <dl class="kv">
            <dt>Discrepancy</dt>
            <dd>
              {#if bill.discrepancy_kind}
                {bill.discrepancy_kind}
              {:else}
                None — invoice, PO, and receipt agree.
              {/if}
            </dd>
            {#if bill.discrepancy_cents}
              <dt>Variance</dt><dd>{moneyFmt(bill.discrepancy_cents)}</dd>
            {/if}
          </dl>
      </Section>
    </div>
  {/if}
</div>
