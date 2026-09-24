<script lang="ts">
  // Brewery storefront — direct-to-consumer beer catalog at /shop.
  //
  // Catalog metadata (name / price / package / tasting notes) lives
  // in apps/web/src/shop/brewery-products.ts as a static module.
  // Inventory STATE (on-hand per SKU) reads from the live
  // /api/inventory/items endpoint so the storefront tracks the same
  // numbers the warehouse + ops surfaces see.
  //
  // /shop is a personal/everyone surface in the three-axis IA
  // (alongside My Day + Inbox), not a tenant-gated tier —
  // any employee or guest can browse. Checkout opens a Job (Work
  // axis) so the order becomes operational state the same way a
  // wholesale-keg-order would.

  import { href, navigate } from '../router';
  import {
    BREWERY_PRODUCTS,
    type BreweryProduct,
    packageLabel,
    priceLabel,
  } from './brewery-products';

  type InventoryRow = Readonly<{
    part_sku: string;
    on_hand: number;
    allocated: number;
  }>;

  let stock = $state<Map<string, number>>(new Map());

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/inventory/items');
        if (!r.ok) return;
        const body = (await r.json()) as InventoryRow[] | { data: InventoryRow[] };
        const rows = Array.isArray(body) ? body : (body.data ?? []);
        if (cancelled) return;
        const m = new Map<string, number>();
        for (const row of rows) {
          if (row.part_sku.startsWith('FP-')) {
            m.set(
              row.part_sku,
              Math.max(0, (row.on_hand ?? 0) - (row.allocated ?? 0)),
            );
          }
        }
        stock = m;
      } catch {
        // Silent — render the catalog with "—" availability rather
        // than blocking the page on a transient inventory blip.
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function availability(sku: string): { label: string; tone: 'in' | 'low' | 'out' | 'unknown' } {
    if (!stock.has(sku)) return { label: 'check availability', tone: 'unknown' };
    const n = stock.get(sku)!;
    if (n <= 0) return { label: 'sold out', tone: 'out' };
    if (n <= 6) return { label: `only ${n} left`, tone: 'low' };
    return { label: `${n} in stock`, tone: 'in' };
  }
</script>

<div class="catalog theme-exec">
  <header class="shop-hero">
    <div class="shop-hero-inner">
      <h1 class="shop-hero-title">Algedonic Ales — Storefront</h1>
      <p class="shop-hero-sub">
        Pick up beer direct from the brewery. Kegs ship on local
        routes; bottles ship via packing slip. Stock numbers come
        straight off the warehouse projection — what you see is
        what's in the cooler right now.
      </p>
    </div>
  </header>

  <section class="shop-grid">
    {#each BREWERY_PRODUCTS as p (p.sku)}
      {@const to = href(`/ux/shop/${encodeURIComponent(p.sku)}`)}
      {@const avail = availability(p.sku)}
      {@const isLimited = p.available_until !== null}
      <article class="shop-card brewery-card">
        <button
          type="button"
          class="shop-card-area"
          onclick={() => navigate(to)}
          aria-label={`View details for ${p.brand} (${packageLabel(p.package)})`}
        >
          <div class="shop-card-image brewery-image">
            <span class="shop-card-category">{p.style}</span>
            {#if isLimited}
              <span class="shop-card-limited">Limited</span>
            {/if}
          </div>
          <div class="shop-card-body">
            <h3 class="shop-card-title">{p.brand}</h3>
            <p class="shop-card-tagline">{p.tagline}</p>
            <div class="shop-card-specs">
              <span class="chip chip-muted">{p.abv_pct}% ABV</span>
              <span class="chip chip-muted">{p.ibu} IBU</span>
              <span class="chip chip-muted">{packageLabel(p.package)}</span>
            </div>
            <div class="shop-card-pricing">
              <div class="shop-card-price">
                <span class="shop-card-price-value">{priceLabel(p.unit_price_cents)}</span>
              </div>
              <div class="shop-card-avail shop-card-avail-{avail.tone}">
                {avail.label}
              </div>
            </div>
          </div>
        </button>
        <a
          href={to}
          onclick={(e) => e.stopPropagation()}
          class="shop-card-cta"
        >
          View details
        </a>
      </article>
    {/each}
  </section>
</div>

<style>
  .brewery-image {
    background: var(--band);
    color: var(--on-band);
    padding: 18px 16px;
    min-height: 90px;
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    position: relative;
  }
  .shop-card-area {
    background: none;
    border: 0;
    padding: 0;
    text-align: left;
    width: 100%;
    cursor: pointer;
    display: block;
    color: inherit;
    font: inherit;
  }
  .shop-card-limited {
    background: var(--busy);
    color: var(--on-busy);
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.6px;
    font-weight: 600;
  }
  .shop-card-avail {
    font-size: 13px;
    font-weight: 500;
  }
  .shop-card-avail-in { color: var(--ok); }
  .shop-card-avail-low { color: var(--warn); }
  .shop-card-avail-out { color: var(--err); }
  .shop-card-avail-unknown { color: var(--static); }
</style>
