<script lang="ts">
  // Invoice detail — port of apps/web/src/finance/InvoicePage.tsx.

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import Meta from '@boss/web-kit/ui/Meta.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import InvoiceStatusChip from './InvoiceStatusChip.svelte';
  import TierChip from '../accounts/TierChip.svelte';
  import {
    INVOICE_STATUS_LABEL,
    PAYMENT_METHOD_LABEL,
    revenueCategoryLabel,
    type Invoice,
    type InvoiceLineItem,
    type RevenueCategory,
  } from './types';
  import { formatMoney } from '@boss/web-kit/ui/money';
  import type { Account } from '../accounts/types';
  import { fetchAccountsPage } from '../accounts/api';
  import { href } from '../router';

  type Props = { invoiceId: string };
  let { invoiceId }: Props = $props();

  type FetchState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'notfound' }
    | { kind: 'ready'; invoice: Invoice };

  let fetchState: FetchState = $state<FetchState>({ kind: 'loading' });
  let accounts = $state<Account[]>([]);

  $effect(() => {
    const id = invoiceId;
    let cancelled = false;
    fetchState = { kind: 'loading' };
    (async () => {
      try {
        const [invResp, pPaged] = await Promise.all([
          fetch(`/api/commerce/invoices/${encodeURIComponent(id)}`),
          fetchAccountsPage(),
        ]);
        if (cancelled) return;
        if (pPaged.kind === 'ready') accounts = [...pPaged.page.data];
        if (invResp.status === 404) {
          fetchState = { kind: 'notfound' };
          return;
        }
        if (!invResp.ok) throw new Error(`commerce API: ${invResp.status}`);
        const invoice = (await invResp.json()) as Invoice;
        fetchState = { kind: 'ready', invoice };
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

  function pickDominantCategory(
    lines: ReadonlyArray<InvoiceLineItem>,
  ): RevenueCategory | null {
    if (lines.length === 0) return null;
    const byCat = new Map<RevenueCategory, number>();
    for (const l of lines) {
      byCat.set(l.revenue_category, (byCat.get(l.revenue_category) ?? 0) + l.amount_cents);
    }
    let top: RevenueCategory | null = null;
    let topAmt = -1;
    for (const [cat, amt] of byCat) {
      if (amt > topAmt) {
        top = cat;
        topAmt = amt;
      }
    }
    return top;
  }
</script>

<div class="detail-page theme-exec">
  <Breadcrumb to={href('/ux/finance')}>
    ← Finance
  </Breadcrumb>

  {#if fetchState.kind === 'loading'}
    <p class="empty">Loading invoice…</p>
  {:else if fetchState.kind === 'notfound'}
    <header class="detail-hero">
      <h1 class="detail-title">Invoice not found</h1>
    </header>
    <p class="empty">No invoice with id <code>{invoiceId}</code>.</p>
  {:else if fetchState.kind === 'error'}
    <header class="detail-hero">
      <h1 class="detail-title">Failed to load invoice</h1>
    </header>
    <p class="empty">{fetchState.message}</p>
  {:else}
    {@const invoice = fetchState.invoice}
    {@const account = accounts.find((p) => p.id === invoice.account_id)}
    {@const taxCents = invoice.tax_cents ?? 0}
    {@const lineSumCents = invoice.line_items.reduce((s, l) => s + l.amount_cents, 0)}
    {@const reconciles = lineSumCents + taxCents === invoice.amount_cents}
    {@const dominantCategory = pickDominantCategory(invoice.line_items)}
    {@const moneyFmt = (c: number) => formatMoney({ amount_cents: c, currency: invoice.currency })}

    <header class="detail-hero">
      <div>
        <div class="detail-eyebrow">
          <EntityLink kind="invoice" id={invoice.id} />
          · <InvoiceStatusChip status={invoice.status} />
          {#if dominantCategory}
            · <span>{revenueCategoryLabel(dominantCategory)}</span>
          {/if}
        </div>
        <h1 class="detail-title">{moneyFmt(invoice.amount_cents)}</h1>
        <div class="detail-tagline">
          <EntityLink
            kind="account"
            id={invoice.account_id}
            label={account?.name}
            mono={!account}
          />
          {#if account} · {account.city}, {account.state}{/if}
        </div>
        <div class="detail-meta">
          <Meta label="Issued">{invoice.issued_on}</Meta>
          <Meta label="Due">{invoice.due_on}</Meta>
          <Meta label="Paid">{invoice.paid_on ?? '—'}</Meta>
          <Meta label="Lines">{invoice.line_items.length}</Meta>
        </div>
      </div>
    </header>

    <div class="tab-grid">
      <Section title="Invoice">
          <dl class="kv">
            <dt>BOSS ID</dt><dd><EntityLink kind="invoice" id={invoice.id} /></dd>
            <dt>Status</dt><dd><InvoiceStatusChip status={invoice.status} /></dd>
            <dt>Subtotal</dt><dd>{moneyFmt(invoice.amount_cents - taxCents)}</dd>
            {#if taxCents > 0}
              <dt>Sales tax</dt>
              <dd>
                {moneyFmt(taxCents)}
                {#if invoice.tax_jurisdiction}
                  <span class="mono" style="margin-left:8px; color:#78716c">
                    {invoice.tax_jurisdiction}
                  </span>
                {/if}
              </dd>
            {/if}
            <dt>Total</dt><dd>{moneyFmt(invoice.amount_cents)}</dd>
            <dt>Issued</dt><dd>{invoice.issued_on}</dd>
            <dt>Due</dt><dd>{invoice.due_on}</dd>
            <dt>Paid</dt><dd>{invoice.paid_on ?? '—'}</dd>
            {#if invoice.payment_method}
              <dt>Method</dt><dd>{PAYMENT_METHOD_LABEL[invoice.payment_method]}</dd>
            {/if}
          </dl>
      </Section>

      {#if account}
        {@const p = account}
        <Section title="Account">
            <dl class="kv">
              <dt>Name</dt>
              <dd>
                <Link to={entityHref('account', p.id)}>
                  {p.name}
                </Link>
              </dd>
              <dt>Location</dt><dd>{p.city}, {p.state}</dd>
              <dt>Tier</dt><dd><TierChip tier={p.tier} /></dd>
            </dl>
        </Section>
      {/if}

      <Section title={`Line items (${invoice.line_items.length})`} wide>
          {#if invoice.line_items.length === 0}
            <p class="empty">No line items on this invoice.</p>
          {:else}
            <table class="data-table">
              <thead>
                <tr>
                  <th>Category</th>
                  <th>Description</th>
                  <th>Reference</th>
                  <th class="num">Amount</th>
                </tr>
              </thead>
              <tbody>
                {#each invoice.line_items as l (l.id)}
                  <tr>
                    <td>{revenueCategoryLabel(l.revenue_category)}</td>
                    <td>{l.description}</td>
                    <td>
                      {#if !l.ref_id}
                        —
                      {:else if l.ref_id.startsWith('opp-')}
                        <Link to={href(`/ux/sales/${l.ref_id}`)} className="mono">
                          {l.ref_id}
                        </Link>
                      {:else if l.ref_id.startsWith('tkt-')}
                        <Link to={href(`/ux/service/${l.ref_id}`)} className="mono">
                          {l.ref_id}
                        </Link>
                      {:else}
                        <span class="mono">{l.ref_id}</span>
                      {/if}
                    </td>
                    <td class="num">{moneyFmt(l.amount_cents)}</td>
                  </tr>
                {/each}
                <tr>
                  <td colspan="3" style="color:#78716c">Subtotal (revenue)</td>
                  <td class="num">{moneyFmt(lineSumCents)}</td>
                </tr>
                {#if taxCents > 0}
                  <tr>
                    <td colspan="3" style="color:#78716c">
                      Sales tax
                      {#if invoice.tax_jurisdiction}
                        <span class="mono" style="font-size:11px">{invoice.tax_jurisdiction}</span>
                      {/if}
                    </td>
                    <td class="num">{moneyFmt(taxCents)}</td>
                  </tr>
                {/if}
                <tr style="font-weight:600">
                  <td colspan="3">Total</td>
                  <td class="num">{moneyFmt(invoice.amount_cents)}</td>
                </tr>
                {#if !reconciles}
                  <tr>
                    <td colspan="4" class="empty">
                      ⚠ Line sum {moneyFmt(lineSumCents)} + tax {moneyFmt(taxCents)}
                      does not match header amount {moneyFmt(invoice.amount_cents)}.
                    </td>
                  </tr>
                {/if}
              </tbody>
            </table>
          {/if}
      </Section>
    </div>
  {/if}
</div>
