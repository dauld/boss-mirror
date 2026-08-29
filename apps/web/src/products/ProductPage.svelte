<script lang="ts">
  // Detail view for one finished-product SKU. Catalog row + on-hand-
  // by-location rollup. Sibling to PartPage.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { formatMoney } from '@boss/web-kit/ui/money';
  import { href } from '../router';
  import type { ProductDetail, ProductInventory } from './types';

  type Props = { sku: string };
  let { sku }: Props = $props();

  let detail = $state<ProductDetail | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  $effect(() => {
    let cancelled = false;
    loading = true;
    error = null;
    (async () => {
      try {
        const resp = await fetch(`/api/products/${encodeURIComponent(sku)}`);
        if (!resp.ok) {
          error = resp.status === 404 ? 'Product not found' : `HTTP ${resp.status}`;
          loading = false;
          return;
        }
        const body = (await resp.json()) as ProductDetail;
        if (cancelled) return;
        detail = body;
        loading = false;
      } catch (e) {
        if (!cancelled) {
          error = e instanceof Error ? e.message : 'Network error';
          loading = false;
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function fmtMoney(cents: number | null | undefined): string {
    if (cents == null) return '—';
    return formatMoney({ amount_cents: cents, currency: 'USD' }, { precision: 'cents' });
  }

  function metaEntries(d: ProductDetail | null): Array<[string, string]> {
    if (!d) return [];
    return Object.entries(d.metadata ?? {})
      .filter(([k]) => k !== 'msrp_cents')
      .map(([k, v]) => [k, String(v)]);
  }
</script>

<div class="page">
  {#if loading}
    <p class="empty">Loading product…</p>
  {:else if error}
    <p class="empty err">{error} — <Link to={href('/ux/products')}>back to products</Link></p>
  {:else if detail}
    <PageHeader
      title={detail.name}
      subtitle={detail.description ?? ''}
      eyebrow={detail.sku}
    />

    <div class="meta-grid">
      <div class="meta-cell">
        <div class="meta-key">Kind</div>
        <div class="meta-val">{detail.product_kind}</div>
      </div>
      <div class="meta-cell">
        <div class="meta-key">Package</div>
        <div class="meta-val">{detail.package_unit}</div>
      </div>
      <div class="meta-cell">
        <div class="meta-key">MSRP</div>
        <div class="meta-val">{fmtMoney(detail.metadata?.msrp_cents as number | undefined)}</div>
      </div>
      <div class="meta-cell">
        <div class="meta-key">Total on hand</div>
        <div class="meta-val total">{detail.total_on_hand}</div>
      </div>
    </div>

    {#if metaEntries(detail).length > 0}
      <h2 class="section-h">Product specs</h2>
      <dl class="spec-list">
        {#each metaEntries(detail) as [k, v] (k)}
          <dt>{k}</dt>
          <dd>{v}</dd>
        {/each}
      </dl>
    {/if}

    <h2 class="section-h">On-hand by location</h2>
    {#if detail.inventory.length === 0}
      <p class="empty">No inventory rows yet.</p>
    {:else}
      <table class="inv-table">
        <thead>
          <tr>
            <th>Location</th>
            <th class="num">On hand</th>
            <th class="num">Reserved</th>
            <th class="num">Available</th>
          </tr>
        </thead>
        <tbody>
          {#each detail.inventory as row, i (row.product_sku + ':' + row.location_id + ':' + i)}
            <tr>
              <td><span class="loc">{row.location_id}</span></td>
              <td class="num">{row.on_hand}</td>
              <td class="num muted">{row.reserved}</td>
              <td class="num strong">{row.on_hand - row.reserved}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    <p class="back">
      <Link to={href('/ux/products')}>← All products</Link>
    </p>
  {/if}
</div>

<style>
  .page {
    padding: 24px;
    max-width: 900px;
  }
  .empty {
    color: #78716c;
    font-style: italic;
  }
  .err {
    color: #b91c1c;
  }
  .meta-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
    gap: 16px;
    background: #fafaf9;
    border: 1px solid #e7e5e4;
    border-radius: 6px;
    padding: 16px;
    margin: 16px 0;
  }
  .meta-cell {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .meta-key {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.4px;
    color: #78716c;
    font-weight: 500;
  }
  .meta-val {
    font-size: 16px;
    font-weight: 500;
    color: #1c1917;
  }
  .meta-val.total {
    color: #166534;
    font-variant-numeric: tabular-nums;
  }
  .section-h {
    font-size: 14px;
    margin: 24px 0 8px;
    color: #44403c;
  }
  .spec-list {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 4px 16px;
    margin: 0 0 16px;
    font-size: 13px;
  }
  .spec-list dt {
    color: #78716c;
    text-transform: capitalize;
  }
  .spec-list dd {
    margin: 0;
    color: #1c1917;
  }
  .inv-table {
    width: 100%;
    border-collapse: collapse;
    background: #fafaf9;
    border: 1px solid #e7e5e4;
    border-radius: 6px;
    overflow: hidden;
  }
  .inv-table th,
  .inv-table td {
    padding: 8px 12px;
    text-align: left;
    border-bottom: 1px solid #e7e5e4;
    font-size: 13px;
  }
  .inv-table th {
    background: #f5f5f4;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.4px;
    color: #44403c;
  }
  .inv-table tr:last-child td {
    border-bottom: none;
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .strong {
    font-weight: 600;
  }
  .muted {
    color: #78716c;
  }
  .loc {
    font-family: ui-monospace, monospace;
    font-size: 12px;
  }
  .back {
    margin-top: 24px;
    font-size: 13px;
  }
</style>
