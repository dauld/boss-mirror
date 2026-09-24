<script lang="ts">
  import { surfaceOf } from '../steps/surfaceRegistry.svelte';
  // Brewery beer detail page — direct-to-consumer purchase flow.
  //
  // Reads catalog metadata from brewery-products.ts (static), live
  // on-hand from /api/inventory/items, and on submit POSTs a new
  // `direct-shop-order` Job to /api/jobs. The Job's step graph
  // walks intake → handoff → shipment → billing the same way a
  // wholesale-keg-order does — Same primitive, different front
  // door (per the brewery DTC /shop TODO entry).

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { href, navigate } from '../router';
  import { appToday } from '@boss/web-kit/sim-clock';
  import {
    findProduct,
    packageLabel,
    priceLabel,
    type BreweryProduct,
  } from './brewery-products';

  type Props = { sku: string };
  let { sku }: Props = $props();

  let product = $state<BreweryProduct | null>(findProduct(sku) ?? null);
  let onHand = $state<number | null>(null);
  let invLoaded = $state(false);
  /// True when the availability check FAILED. It used to render as
  /// "Currently sold out" — a stock claim the page had no basis for
  /// (packet 3fba9c35, the false-empty sweep). A failed check says
  /// so, and keeps the order form open.
  let invFailed = $state(false);

  $effect(() => {
    const s = sku;
    let cancelled = false;
    product = findProduct(s) ?? null;
    invLoaded = false;
    invFailed = false;
    onHand = null;
    (async () => {
      try {
        const r = await fetch('/api/inventory/items');
        if (!r.ok) {
          if (!cancelled) {
            invFailed = true;
            invLoaded = true;
          }
          return;
        }
        const body = (await r.json()) as
          | Array<{ part_sku: string; on_hand: number; allocated: number }>
          | { data: Array<{ part_sku: string; on_hand: number; allocated: number }> };
        const rows = Array.isArray(body) ? body : (body.data ?? []);
        if (cancelled) return;
        const row = rows.find((r) => r.part_sku === s);
        onHand = row ? Math.max(0, (row.on_hand ?? 0) - (row.allocated ?? 0)) : 0;
        invLoaded = true;
      } catch {
        if (!cancelled) {
          invFailed = true;
          invLoaded = true;
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  // Order form state
  let qty = $state(1);
  let email = $state('');
  let name = $state('');
  let phone = $state('');
  let notes = $state('');
  let submitting = $state(false);
  let submittedJobId = $state<string | null>(null);
  let error = $state<string | null>(null);

  let canSubmit = $derived.by(() => {
    if (!product) return false;
    if (qty < 1) return false;
    if (!email || !name) return false;
    if (onHand !== null && qty > onHand) return false;
    return true;
  });

  let totalCents = $derived(product ? product.unit_price_cents * qty : 0);

  async function submit(): Promise<void> {
    if (!product || !canSubmit) return;
    submitting = true;
    error = null;
    try {
      const todayIso = appToday();
      // The buyer gets a durable home first (boss-customers, Q4b):
      // POST derives a deterministic id from the email, so the same
      // buyer always lands on the same row and re-checkout is
      // idempotent. Best-effort — a refused create still takes the
      // order (the inline metadata fields below remain the shipping
      // surface's source), it just goes unlinked.
      let customerId: string | null = null;
      try {
        const cr = await fetch('/api/customers', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ name, email, phone: phone || undefined }),
        });
        if (cr.ok) {
          customerId = ((await cr.json()) as { id?: string }).id ?? null;
        }
      } catch {
        // Unlinked order beats a failed order.
      }
      const body = {
        kind: 'direct-shop-order',
        subject: { subject_kind: 'account', id: 'acc-direct-shop' },
        title: `Direct order — ${product.brand} (${packageLabel(product.package)}) × ${qty}`,
        // The boss-jobs API expects the full Job struct on POST.
        // owner_id is the catch-all "direct-shop" pseudo-employee
        // — direct orders aren't routed to a real human until the
        // shipping-clerk picks one up off the queue.
        owner_id: 'direct-shop',
        status: 'open',
        priority: 'standard',
        opened_on: todayIso,
        tags: ['direct-shop'],
        metadata: {
          customer_id: customerId,
          customer_email: email,
          customer_name: name,
          customer_phone: phone,
          line_items: [
            {
              part_sku: product.sku,
              qty,
              unit_price_cents: product.unit_price_cents,
              description: `${product.brand} — ${packageLabel(product.package)}`,
            },
          ],
          total_cents: totalCents,
          notes,
        },
      };
      const r = await fetch('/api/jobs', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!r.ok) {
        const t = await r.text();
        error = `Order failed (${r.status}): ${t}`;
        submitting = false;
        return;
      }
      const created = (await r.json()) as { id?: string };
      submittedJobId = created.id ?? null;

      // Overlay line_items onto the shipment + billing steps so
      // the side effects (shipping.create / products.consume on
      // shipment-done; commerce.invoice.issue on billing-done)
      // see them in step metadata. Job.metadata.line_items is not
      // automatically copied into step metadata by
      // materialize_steps. Best-effort: failure here just means
      // the operator clicks the step open and pastes line_items
      // before completing — the Job is still created.
      //
      // The shipment overlay also sets `consumes_products` (the
      // products.consume handler reads this to decrement
      // finished_product_inventory). Same SKU+qty pairs as
      // line_items but with location_id added per row.
      if (submittedJobId) {
        try {
          const stepsResp = await fetch(`/api/jobs/${submittedJobId}/steps`);
          if (stepsResp.ok) {
            const steps = (await stepsResp.json()) as Array<{
              id: string;
              kind: string;
              metadata?: Record<string, unknown>;
            }>;
            const shipmentStep = steps.find((s) => surfaceOf(s.kind) === 'shipment');
            const billingStep = steps.find((s) => surfaceOf(s.kind) === 'billing');
            const lineItems = body.metadata.line_items;
            const consumesProducts = lineItems.map((li) => ({
              sku: li.part_sku,
              qty: li.qty,
              location_id: 'loc-brewery-brewhouse',
            }));
            if (shipmentStep) {
              await fetch(`/api/jobs/${submittedJobId}/steps/${shipmentStep.id}`, {
                method: 'PUT',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                  metadata: {
                    ...(shipmentStep.metadata ?? {}),
                    line_items: lineItems,
                    consumes_products: consumesProducts,
                  },
                }),
              });
            }
            if (billingStep) {
              await fetch(`/api/jobs/${submittedJobId}/steps/${billingStep.id}`, {
                method: 'PUT',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                  metadata: {
                    ...(billingStep.metadata ?? {}),
                    line_items: lineItems,
                    amount_cents: totalCents,
                  },
                }),
              });
            }
          }
        } catch {
          // Silent — Job is created; metadata overlay is opportunistic.
        }
      }
    } catch (e) {
      error = `Order error: ${e instanceof Error ? e.message : String(e)}`;
    }
    submitting = false;
  }
</script>

{#if !product}
  <div class="catalog theme-exec">
    <Breadcrumb to={href('/ux/shop')}>← All beer</Breadcrumb>
    <div class="exec-header"><h1 class="exec-title">Beer not found</h1></div>
    <p class="empty">No beer with SKU <code>{sku}</code> in the brewery catalog.</p>
  </div>
{:else}
  {@const p = product}
  {@const isLimited = p.available_until !== null}
  {@const stockKnown = onHand !== null}
  {@const inStock = (onHand ?? 0) > 0}

  <div class="detail-page theme-exec">
    <Breadcrumb to={href('/ux/shop')}>← All beer</Breadcrumb>

    <header class="detail-hero">
      <div>
        <div class="detail-eyebrow">{p.style}</div>
        <h1 class="detail-title">{p.brand}</h1>
        <div class="detail-tagline">{p.tagline}</div>
        <div class="shop-product-pricing" style="margin-top:16px">
          <div class="shop-price-block">
            <span class="shop-price-label">{packageLabel(p.package)}</span>
            <span class="shop-price-big">{priceLabel(p.unit_price_cents)}</span>
          </div>
          {#if isLimited}
            <div class="shop-price-block shop-price-refurb">
              <span class="shop-price-label">Available until</span>
              <span class="shop-price-big">{p.available_until}</span>
            </div>
          {/if}
        </div>
        {#if invLoaded}
          {#if invFailed}
            <p class="shop-stock-line">
              Couldn't check availability right now — place the order and
              the shipping team will confirm stock.
            </p>
          {:else if inStock}
            <p class="shop-stock-line shop-stock-in">
              {onHand} unit{onHand === 1 ? '' : 's'} on hand at the brewhouse cooler
            </p>
          {:else}
            <p class="shop-stock-line shop-stock-out">
              Currently sold out. The brewing line refills this SKU on the next batch.
            </p>
          {/if}
        {/if}
      </div>
    </header>

    {#if submittedJobId}
      <div class="shop-quote-form">
        <div class="shop-quote-success">
          <h4>Order placed</h4>
          <p>
            Your order is open as Job <code>{submittedJobId}</code>. The
            shipping team will see it in their queue and confirm
            packing within one business day. We'll send tracking to
            <strong>{email}</strong> once the keg leaves the cooler.
          </p>
        </div>
      </div>
    {:else if inStock || !invLoaded || invFailed}
      <div class="shop-quote-form">
        <h4>Order — {p.brand} ({packageLabel(p.package)})</h4>
        <div class="shop-quote-grid">
          <div class="shop-quote-field">
            <label for="ord-qty">Quantity</label>
            <input
              id="ord-qty"
              type="number"
              min="1"
              max={onHand ?? 99}
              bind:value={qty}
            />
          </div>
          <div class="shop-quote-field">
            <label for="ord-name">Your name</label>
            <input
              id="ord-name"
              type="text"
              bind:value={name}
              placeholder="Sam Brewer"
            />
          </div>
          <div class="shop-quote-field">
            <label for="ord-email">Email</label>
            <input
              id="ord-email"
              type="email"
              bind:value={email}
              placeholder="sam@example.com"
            />
          </div>
          <div class="shop-quote-field">
            <label for="ord-phone">Phone (optional)</label>
            <input
              id="ord-phone"
              type="tel"
              bind:value={phone}
              placeholder="(555) 123-4567"
            />
          </div>
          <div class="shop-quote-field shop-quote-field-wide">
            <label for="ord-notes">Notes</label>
            <textarea
              id="ord-notes"
              rows="2"
              bind:value={notes}
              placeholder="Pickup vs delivery, special handling, etc."
            ></textarea>
          </div>
        </div>
        <div class="shop-order-total">
          <span class="shop-price-label">Order total</span>
          <span class="shop-price-big">{priceLabel(totalCents)}</span>
        </div>
        {#if error}
          <p class="shop-order-error">{error}</p>
        {/if}
        <div class="shop-quote-actions">
          <button
            type="button"
            class="shop-cta-primary"
            disabled={!canSubmit || submitting}
            onclick={submit}
          >
            {submitting ? 'Placing order…' : 'Place order'}
          </button>
        </div>
      </div>
    {/if}

    <div class="tab-grid">
      <Section title="About this beer" wide>
        <p class="prose">{p.description}</p>
      </Section>

      <Section title="Specs">
        <dl class="kv">
          <dt>Style</dt><dd>{p.style}</dd>
          <dt>ABV</dt><dd>{p.abv_pct}%</dd>
          <dt>IBU</dt><dd>{p.ibu}</dd>
          <dt>Package</dt><dd>{packageLabel(p.package)}</dd>
          <dt>Price</dt><dd>{priceLabel(p.unit_price_cents)}</dd>
          {#if isLimited && p.available_until}
            <dt>Available until</dt><dd>{p.available_until}</dd>
          {:else}
            <dt>Availability</dt><dd>Year-round</dd>
          {/if}
        </dl>
      </Section>
    </div>
  </div>
{/if}

<style>
  .shop-stock-line {
    margin-top: 12px;
    font-size: 14px;
    font-weight: 500;
  }
  .shop-stock-in { color: var(--ok); }
  .shop-stock-out { color: var(--err); }
  .shop-order-total {
    display: flex;
    align-items: baseline;
    gap: 12px;
    margin: 12px 0;
    padding: 8px 12px;
    background: var(--ink-raised);
    border-radius: 4px;
  }
  .shop-order-total .shop-price-big {
    font-size: 20px;
  }
  .shop-order-error {
    color: var(--err);
    font-size: 14px;
    margin: 8px 0;
  }
</style>
