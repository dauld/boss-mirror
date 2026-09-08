<script lang="ts">
  // Part detail — port of apps/web/src/parts/PartPage.tsx.

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { appToday } from '@boss/web-kit/sim-clock';
  import Meta from '@boss/web-kit/ui/Meta.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { formatMoney } from '@boss/web-kit/ui/money';
  import StatusChip from '@boss/web-kit/ui/StatusChip.svelte';
  import {
    collectParts,
    kindFromSku,
    stockStatus,
    stockTone,
    type CatalogPart,
    type DeviceModel,
    type InventoryItem,
    type PartUsedBy,
    type PurchaseOrder,
  } from './types';
  import {
    InventoryItemListSchema,
    PurchaseOrderListSchema,
  } from './schemas';
  import {
    DeviceModelListSchema,
    CatalogPartListSchema,
  } from '../catalog/schemas';
  import { fetchValidated, z } from '../data/parseResponse';
  import { href } from '../router';

  // Schema for /api/catalog/documents response — matches
  // `EntityDocument` in boss-catalog/src/types.rs. Inline because
  // documents is a generic surface used across entity kinds and
  // doesn't have a dedicated schemas module yet (the dedicated
  // catalog schemas cover AssetModel + CatalogPart only).
  const KbDocumentSchema = z.object({
    id: z.string(),
    doc_type: z.string(),
    title: z.string(),
    url: z.string().nullable().optional(),
    version: z.string().nullable().optional(),
    audience: z.string(),
    uploaded_at: z.string().nullable().optional(),
  });
  type KbDocument = z.infer<typeof KbDocumentSchema>;
  const KbDocumentListSchema = z.array(KbDocumentSchema);

  let { partSku } = $props<{ partSku: string }>();

  let models = $state<DeviceModel[]>([]);
  let inventory = $state<InventoryItem[]>([]);
  let pos = $state<PurchaseOrder[]>([]);
  // Brewery (and any tenant that seeds parts directly into the
  // `parts` table without satellite system_models linkage) — pull
  // the canonical /api/catalog/parts list so we can resolve a part
  // even when no device-model references it. Without this the
  // detail page rendered "Part not found" for every brewery
  // ingredient/packaging SKU despite the inventory row existing.
  let catalogParts = $state<CatalogPart[]>([]);
  // KB documents tagged to this part (#52). Sourced from the
  // generic /api/catalog/documents endpoint with entity_kind=part.
  // Tenant-seeded — COA scans for hops + malt, MSDS sheets for
  // cleaning chems, vendor datasheets for packaging. Empty array
  // for parts with nothing on file (the common case at v1.0.6).
  let documents = $state<KbDocument[]>([]);
  let loading = $state(true);
  let parseError = $state<string | null>(null);

  $effect(() => {
    void partSku;
    let cancelled = false;
    loading = true;
    parseError = null;
    (async () => {
      const encoded = encodeURIComponent(partSku);
      const [mRes, iRes, pRes, cpRes, dRes] = await Promise.all([
        fetchValidated('/api/catalog/models', DeviceModelListSchema),
        fetchValidated('/api/inventory/items', InventoryItemListSchema),
        fetchValidated('/api/inventory/orders', PurchaseOrderListSchema),
        fetchValidated('/api/catalog/parts', CatalogPartListSchema),
        // KB documents for this ingredient/part (#52). Generic
        // entity-keyed endpoint — same surface the system catalog,
        // accounts, vendors etc. use.
        fetchValidated(
          `/api/catalog/documents?entity_kind=part&entity_id=${encoded}`,
          KbDocumentListSchema,
        ),
      ]);
      if (cancelled) return;
      // Surface the first schema mismatch — schema bugs should
      // be loud, not papered over by the [] fallback. HTTP errors
      // (offline, 5xx) fall through to empty arrays, matching the
      // old behavior.
      for (const r of [mRes, iRes, pRes, cpRes, dRes]) {
        if (r.kind === 'invalid') {
          parseError = r.reason;
          loading = false;
          return;
        }
      }
      models = mRes.kind === 'ok' ? (mRes.data as unknown as DeviceModel[]) : [];
      inventory = iRes.kind === 'ok' ? iRes.data : [];
      pos = pRes.kind === 'ok' ? (pRes.data as unknown as PurchaseOrder[]) : [];
      catalogParts = cpRes.kind === 'ok' ? cpRes.data : [];
      documents = dRes.kind === 'ok' ? dRes.data : [];
      loading = false;
    })();
    return () => {
      cancelled = true;
    };
  });

  let sku = $derived(decodeURIComponent(partSku));
  let modelBySku = $derived.by(() => {
    const m = new Map<string, DeviceModel>();
    for (const model of models) m.set(model.sku, model);
    return m;
  });

  // Resolve the part from either source: device-model satellite
  // linkage (collectParts) OR the canonical /api/catalog/parts table
  // (brewery's direct seeding). The order matters — model-derived
  // entries carry `used_by` linkage that catalog-derived entries
  // don't, so prefer model lookup when available.
  let part = $derived.by<PartUsedBy | undefined>(() => {
    const fromModels = collectParts(models).find((p) => p.sku === sku);
    if (fromModels) return fromModels;
    const fromCatalog = catalogParts.find((p) => p.part_sku === sku);
    if (!fromCatalog) return undefined;
    const inferred = kindFromSku(sku);
    return {
      sku,
      // Synthesize a SparePart-shaped object from CatalogPart.
      // `high_usage` doesn't apply to brewery ingredients/packaging;
      // default to false so the detail page renders.
      part: { ...fromCatalog, high_usage: false },
      // PartUsedBy.kind is 'spare' | 'consumable' — brewery
      // ingredients/packaging don't map cleanly, so we label them
      // 'spare' here and rely on the displayed sku prefix +
      // tenant-aware labels for the human-readable category.
      kind: inferred === 'spare' ? 'spare' : 'consumable',
      used_by: [],
    };
  });
  let item = $derived(inventory.find((i) => i.part_sku === sku));

  // Reorder Job state. Vendor is derived from the most recent
  // PO line for this SKU; if no PO history exists the button
  // still renders, the operator types the vendor id, and the
  // POST falls through. Most ingredient SKUs have at least
  // one closed PO line so the auto-pick covers the common case.
  let suggestedVendor = $derived.by<string | null>(() => {
    const lines: Array<{ vendor: string; placed_on: string }> = [];
    for (const po of pos) {
      for (const line of po.lines) {
        if (line.part_sku === sku) {
          lines.push({ vendor: po.vendor, placed_on: po.placed_on });
        }
      }
    }
    lines.sort((a, b) => b.placed_on.localeCompare(a.placed_on));
    return lines[0]?.vendor ?? null;
  });

  let reorderVendorOverride = $state<string>('');
  let reorderSubmitting = $state(false);
  let reorderJobId = $state<string | null>(null);
  let reorderError = $state<string | null>(null);

  async function startReorder(): Promise<void> {
    const vendor = (reorderVendorOverride || suggestedVendor || '').trim();
    if (!vendor) {
      reorderError = 'Pick a vendor first (no PO history for this SKU).';
      return;
    }
    reorderSubmitting = true;
    reorderError = null;
    reorderJobId = null;
    try {
      const todayIso = appToday();
      const body = {
        kind: 'ingredient-restock',
        subject: { subject_kind: 'vendor', id: vendor },
        title: `Ingredient restock — ${vendor} (${sku})`,
        // bootstrap-admin is the OSS quickstart's default operator;
        // a deployment with a real on-call buyer rebinds this via
        // the assignee field on the procurement step.
        owner_id: 'emp-bootstrap-admin',
        status: 'open',
        priority: 'urgent',
        opened_on: todayIso,
        tags: ['manual-reorder', 'kb-initiated'],
        metadata: { triggered_by_sku: sku },
      };
      const r = await fetch('/api/jobs', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!r.ok) {
        reorderError = `Failed (${r.status}): ${await r.text()}`;
        return;
      }
      const created = (await r.json()) as { id?: string };
      reorderJobId = created.id ?? null;
    } catch (e) {
      reorderError = e instanceof Error ? e.message : String(e);
    } finally {
      reorderSubmitting = false;
    }
  }
</script>

{#if loading}
  <div class="catalog theme-exec">
    <p class="empty">Loading part…</p>
  </div>
{:else if parseError}
  <div class="catalog theme-exec">
    <div class="exec-header">
      <h1 class="exec-title">Server returned an unexpected payload shape</h1>
    </div>
    <p class="empty">
      One of /api/catalog/models, /api/inventory/items,
      /api/inventory/orders, or /api/catalog/parts returned a body
      that didn't match the expected schema. Details:
    </p>
    <pre class="empty">{parseError}</pre>
  </div>
{:else if !part || !item}
  <div class="catalog theme-exec">
    <div class="exec-header"><h1 class="exec-title">Part not found</h1></div>
    <p class="empty">No inventory record for <code>{sku}</code>.</p>
  </div>
{:else}
  {@const status = stockStatus(item)}
  {@const available = item.on_hand - item.allocated}
  {@const monthsOfCover = item.trailing_90d_usage === 0
    ? Infinity
    : (available / item.trailing_90d_usage) * 3}
  {@const openLines = pos.flatMap((po) =>
    po.status === 'received' || po.status === 'closed'
      ? []
      : po.lines.filter((l) => l.part_sku === sku).map((l) => ({ po, line: l })),
  )}
  {@const usedByModels = part.used_by
    .map((modelSku) => modelBySku.get(modelSku))
    .filter((m): m is DeviceModel => m !== undefined)}

  <div class="detail-page theme-exec">
    <Breadcrumb to={href('/ux/parts')}>
      ← All parts
    </Breadcrumb>

    <header class="detail-hero">
      <div>
        <div class="detail-eyebrow">
          <EntityLink kind="part" id={sku} /> · {part.kind} ·
          <StatusChip value={status} tone={stockTone(status)} />
        </div>
        <h1 class="detail-title">{part.part.name}</h1>
        <div class="detail-tagline">{part.part.description}</div>
        <div class="detail-meta">
          <Meta label="Available">{available}</Meta>
          <Meta label="On hand">{item.on_hand}</Meta>
          <Meta label="Reorder point">{item.reorder_point}</Meta>
          <Meta label="Months of cover">
              {monthsOfCover === Infinity ? '—' : monthsOfCover.toFixed(1)}
          </Meta>
        </div>
      </div>
    </header>

    <div class="tab-grid">
      <Section title="Stock">
          <dl class="kv">
            <dt>On hand</dt><dd>{item.on_hand}</dd>
            <dt>Allocated</dt><dd>{item.allocated}</dd>
            <dt>Available</dt><dd>{available}</dd>
            <dt>Reorder point</dt><dd>{item.reorder_point}</dd>
            <dt>Reorder qty</dt><dd>{item.reorder_qty}</dd>
            <dt>Bin</dt><dd><span class="mono">{item.bin}</span></dd>
          </dl>
      </Section>

      <Section title="Part">
          <dl class="kv">
            <dt>SKU</dt><dd><EntityLink kind="part" id={sku} /></dd>
            <dt>Unit price</dt><dd>{formatMoney({ amount_cents: part.part.unit_price_cents, currency: part.part.currency })}</dd>
            <dt>Lead time</dt>
            <dd>
              {'lead_time_days' in part.part ? `${part.part.lead_time_days} days` : '—'}
            </dd>
            <dt>Kind</dt><dd>{part.kind}</dd>
            <dt>Trailing 90-day usage</dt><dd>{item.trailing_90d_usage}</dd>
          </dl>
      </Section>

      <Section title="Open purchase orders" wide>
          {#if openLines.length === 0}
            <p class="empty">No open POs for this SKU.</p>
          {:else}
            <table class="data-table">
              <thead>
                <tr>
                  <th>PO</th>
                  <th>Vendor</th>
                  <th>Status</th>
                  <th>Placed</th>
                  <th>Expected</th>
                  <th class="num">Qty</th>
                  <th class="num">Unit cost</th>
                </tr>
              </thead>
              <tbody>
                {#each openLines as ol (`${ol.po.id}-${ol.line.part_sku}`)}
                  <tr>
                    <td class="mono"><EntityLink kind="po" id={ol.po.id} /></td>
                    <td><EntityLink kind="vendor" id={ol.po.vendor} /></td>
                    <td>{ol.po.status.replace(/-/g, ' ')}</td>
                    <td>{ol.po.placed_on}</td>
                    <td>{ol.po.expected_on}</td>
                    <td class="num">{ol.line.qty}</td>
                    <td class="num">{formatMoney({ amount_cents: ol.line.unit_cost_cents, currency: ol.line.currency })}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
      </Section>

      <Section title="Reorder" wide>
        {#if suggestedVendor || reorderVendorOverride}
          <p>
            Vendor: <EntityLink kind="vendor" id={reorderVendorOverride || (suggestedVendor as string)} />
            {#if suggestedVendor && !reorderVendorOverride}
              <span style="color:#78716c; margin-left:8px">(from most recent PO)</span>
            {/if}
          </p>
        {/if}
        <div style="display:flex; gap:8px; align-items:center; flex-wrap:wrap">
          <input
            type="text"
            placeholder={suggestedVendor ? `Override vendor (default: ${suggestedVendor})` : 'Vendor id'}
            bind:value={reorderVendorOverride}
            disabled={reorderSubmitting}
            style="padding:6px 10px; border:1px solid #d6d3d1; border-radius:4px; font-family:inherit; min-width:240px"
          />
          <button
            type="button"
            onclick={startReorder}
            disabled={reorderSubmitting || (!suggestedVendor && !reorderVendorOverride)}
            style="padding:8px 16px; background:#1c1917; color:white; border:none; border-radius:4px; font-family:inherit; cursor:pointer; font-weight:600"
          >
            {reorderSubmitting ? 'Opening Job…' : 'Start reorder Job'}
          </button>
        </div>
        {#if reorderError}
          <p style="color:#dc2626; margin-top:8px">{reorderError}</p>
        {/if}
        {#if reorderJobId}
          <p style="margin-top:8px">
            ✓ Job opened: <Link to={entityHref('job', reorderJobId)}>{reorderJobId}</Link>
          </p>
        {/if}
      </Section>

      <Section
        title={`Used by ${usedByModels.length} model${usedByModels.length === 1 ? '' : 's'}`}
        wide
      >
          <ul class="checklist">
            {#each usedByModels as m (m.sku)}
              <li>
                <Link to={href(`/ux/catalog/${m.sku}`)}>
                  {m.name}
                </Link>
                <span style="color:#78716c; margin-left:8px">
                  ({m.category.replace(/-/g, ' ')})
                </span>
              </li>
            {/each}
          </ul>
      </Section>

      <Section
        title={`Documents (${documents.length})`}
        wide
      >
        {#if documents.length === 0}
          <p style="color:#78716c; margin:0;">
            No documents on file. Tenants can attach COA scans,
            MSDS sheets, vendor datasheets, etc. by INSERTing into the
            <code>documents</code> table with
            <code>entity_kind='part', entity_id='{partSku}'</code>.
          </p>
        {:else}
          <ul class="checklist">
            {#each documents as d (d.id)}
              <li>
                {#if d.url}
                  <a href={d.url} target="_blank" rel="noopener noreferrer">
                    {d.title}
                  </a>
                {:else}
                  {d.title}
                {/if}
                <span style="color:#78716c; margin-left:8px">
                  ({d.doc_type}{d.version ? ` · v${d.version}` : ''})
                </span>
              </li>
            {/each}
          </ul>
        {/if}
      </Section>
    </div>
  </div>
{/if}
