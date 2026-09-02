<script lang="ts">
  // Warehouse — port of apps/web/src/warehouse/WarehousePage.tsx.
  //
  // Three tabs: Overview (backed by the /warehouse-status projection),
  // Inventory (filterable SKU list), Receiving (PO queue + Create PO).

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import FilterGroup from '@boss/web-kit/ui/FilterGroup.svelte';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import StatusChip from '@boss/web-kit/ui/StatusChip.svelte';
  import SortHeader from '@boss/web-kit/ui/SortHeader.svelte';
  import { createSortState } from '@boss/web-kit/ui/sort-state.svelte';
  import {
    stockStatus,
    stockTone,
    type InventoryItem,
    type PoStatus,
    type PurchaseOrder,
    type StockStatus,
  } from '../parts/types';
  import type { WarehouseStatus } from './types';
  import { href } from '../router';

  type Tab = 'overview' | 'inventory' | 'receiving';
  const TABS: ReadonlyArray<{ id: Tab; label: string }> = [
    { id: 'overview', label: 'Overview' },
    { id: 'inventory', label: 'Inventory' },
    { id: 'receiving', label: 'Receiving' },
  ];

  let inventory = $state<InventoryItem[]>([]);
  let purchaseOrders = $state<PurchaseOrder[]>([]);
  let status = $state<WarehouseStatus | null>(null);
  /// Non-null when the inventory/PO reads failed — rendered instead
  /// of "0 tracked SKUs" and empty tables, which an outage is not
  /// (packet 3fba9c35, the false-empty sweep). The status tile keeps
  /// its own honest "unavailable" render.
  let loadFailed = $state<string | null>(null);
  let statusLoading = $state(true);
  let tab = $state<Tab>('overview');

  async function loadAll(): Promise<void> {
    try {
      const [iResp, pResp, sResp] = await Promise.all([
        fetch('/api/inventory/items'),
        fetch('/api/inventory/orders'),
        fetch('/api/inventory/warehouse-status'),
      ]);
      if (iResp.ok) {
        const body = await iResp.json();
        inventory = Array.isArray(body) ? body : (body.data ?? []);
      }
      if (pResp.ok) {
        const body = await pResp.json();
        purchaseOrders = Array.isArray(body) ? body : (body.data ?? []);
      }
      if (sResp.ok) status = (await sResp.json()) as WarehouseStatus;
      const down = [iResp, pResp].find((x) => !x.ok);
      loadFailed = down ? `HTTP ${down.status}` : null;
    } catch (e) {
      loadFailed = e instanceof Error ? e.message : String(e);
    }
    statusLoading = false;
  }

  $effect(() => {
    void loadAll();
  });

  let inventoryRows = $derived(
    inventory.map((item) => ({
      item,
      available: item.on_hand - item.allocated,
      status: stockStatus(item),
    })),
  );

  let headerTitle = $derived(
    status
      ? `${status.parts_stock.total_skus} tracked SKUs`
      : `${inventory.length} tracked SKUs`,
  );
  // Tenant-aware subtitle: drop the refurb-WIP / ready-for-sale
  // segments when they're zero. Brewery never has either; used-
  // device-shop always has both — same code, no per-tenant gate.
  let headerSubtitle = $derived.by(() => {
    if (!status) {
      return `${inventoryRows.filter((r) => r.available <= r.item.reorder_point).length} below reorder point`;
    }
    const parts = [
      `${status.parts_stock.below_reorder_count} below reorder`,
      `${status.inbound_pos.total_open} open POs`,
    ];
    if (status.refurb_wip.total_in_flight > 0) {
      parts.push(`${status.refurb_wip.total_in_flight} refurb WIP`);
    }
    if (status.ready_for_sale_count > 0) {
      parts.push(`${status.ready_for_sale_count} ready for sale`);
    }
    return parts.join(' · ');
  });

  // Inventory filter
  type InvFilter = 'all' | 'critical' | 'low';
  let invFilter = $state<InvFilter>('all');

  let invVisible = $derived(
    inventoryRows.filter((r) => {
      if (invFilter === 'critical') return r.status === 'critical' || r.status === 'out';
      if (invFilter === 'low') return r.status === 'low';
      return true;
    }),
  );

  // The status column sorts by SEVERITY rank, not alphabet — and
  // severity-first stays the landing order it always was (CAR-4:
  // sorting became clickable, the default did not move).
  const STOCK_RANK: Record<StockStatus, number> = { out: 0, critical: 1, low: 2, healthy: 3 };
  type InvSortKey = 'sku' | 'bin' | 'on_hand' | 'allocated' | 'available' | 'reorder' | 'status';
  const INV_DESC_FIRST: ReadonlyArray<InvSortKey> = [
    'on_hand',
    'allocated',
    'available',
    'reorder',
  ];
  const invSort = createSortState<InvSortKey>({ key: 'status', dir: 'asc' }, (k) =>
    INV_DESC_FIRST.includes(k) ? 'desc' : 'asc',
  );
  let invSorted = $derived(
    invSort.sorted(invVisible, {
      sku: (r) => r.item.part_sku,
      bin: (r) => r.item.bin,
      on_hand: (r) => r.item.on_hand,
      allocated: (r) => r.item.allocated,
      available: (r) => r.available,
      reorder: (r) => r.item.reorder_point,
      status: (r) => STOCK_RANK[r.status],
    }),
  );

  let invCritical = $derived(
    inventoryRows.filter((r) => r.status === 'critical' || r.status === 'out').length,
  );
  let invLow = $derived(inventoryRows.filter((r) => r.status === 'low').length);

  // Receiving / PO filter
  const PO_STATUSES: ReadonlyArray<PoStatus> = [
    'draft', 'submitted', 'acknowledged', 'in-transit', 'received', 'closed',
  ];
  // 'open' covers the in-flight queue (everything that hasn't landed
  // yet) — same working-set shape as the Shipping page's
  // 'undelivered' default. The full 12-month seed window otherwise
  // buries today's open POs under hundreds of closed historical rows.
  type PoFilter = 'all' | 'open' | PoStatus;
  let poFilter = $state<PoFilter>('open');

  let poCounts = $derived.by(() => {
    const m = new Map<PoStatus, number>();
    for (const s of PO_STATUSES) m.set(s, 0);
    for (const po of purchaseOrders) {
      m.set(po.status as PoStatus, (m.get(po.status as PoStatus) ?? 0) + 1);
    }
    return m;
  });
  let openPoCount = $derived(
    purchaseOrders.filter((po) => po.status !== 'received' && po.status !== 'closed').length,
  );
  let poVisible = $derived(
    purchaseOrders.filter((po) => {
      if (poFilter === 'all') return true;
      if (poFilter === 'open') return po.status !== 'received' && po.status !== 'closed';
      return po.status === poFilter;
    }),
  );

  // Create PO modal
  let showCreatePo = $state(false);
  let createPoVendor = $state('');
  let createPoSku = $state('');
  let createPoQty = $state(10);
  let createPoCost = $state(500);
  let createPoStatus = $state<string | null>(null);

  async function handleCreatePo(): Promise<void> {
    if (!createPoVendor || !createPoSku || createPoQty < 1) return;
    try {
      const r = await fetch('/api/inventory/orders/create', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          vendor: createPoVendor,
          lines: [
            {
              part_sku: createPoSku,
              qty: createPoQty,
              unit_cost_cents: createPoCost * 100,
              currency: 'USD',
            },
          ],
        }),
      });
      if (r.ok) {
        const result = (await r.json()) as { id: string };
        createPoStatus = `PO ${result.id} created`;
        showCreatePo = false;
        createPoVendor = '';
        createPoSku = '';
        createPoQty = 10;
        await loadAll();
      } else {
        createPoStatus = `Error: ${await r.text()}`;
      }
    } catch (e) {
      createPoStatus = `Error: ${e instanceof Error ? e.message : 'unknown'}`;
    }
  }
</script>

<div class="catalog theme-exec">
  <PageHeader eyebrow="Warehouse" title={headerTitle} subtitle={headerSubtitle} />

  <nav class="tabs" role="tablist">
    {#each TABS as t (t.id)}
      <button
        type="button"
        role="tab"
        aria-selected={tab === t.id}
        class="tab {tab === t.id ? 'tab-active' : ''}"
        onclick={() => (tab = t.id)}
      >
        {t.label}
      </button>
    {/each}
  </nav>

  {#if tab === 'overview'}
    {#if statusLoading && !status}
      <p class="empty" style="padding:16px">Loading warehouse status…</p>
    {:else if !status}
      <p class="empty" style="padding:16px">Warehouse status unavailable.</p>
    {:else}
      {@const s = status}
      <div class="tab-content" style="padding:16px 0; display:flex; flex-direction:column; gap:16px">
        {#if s.refurb_wip.total_in_flight > 0 || s.ready_for_sale_count > 0}
          <!-- Tenant-aware refurb pipeline. Brewery never has
               either bucket populated → section hides. Used-device-
               shop always does → section shows. No tenant flag, no
               per-tenant code path. -->
          <section class="tab-section">
            <h3 style="margin-top:0">
              Refurb pipeline · {s.refurb_wip.total_in_flight.toLocaleString()} in flight ·
              <span style="color:#065f46">{s.ready_for_sale_count.toLocaleString()}</span>
              ready for sale
            </h3>
            <div style="display:flex; gap:8px; flex-wrap:wrap">
              {#each s.refurb_wip.by_stage as row (row.stage)}
                <div
                  style="flex:1 1 0; min-width:120px; padding:10px 12px; border:1px solid #e7e5e4; border-radius:8px; background:#fafaf9"
                >
                  <div style="font-size:11px; color:#78716c; text-transform:uppercase; letter-spacing:0.4px">
                    {row.stage}
                  </div>
                  <div style="font-size:24px; font-weight:600; margin-top:2px">
                    {row.count.toLocaleString()}
                  </div>
                </div>
              {/each}
            </div>
          </section>
        {/if}

        <div style="display:flex; flex-wrap:wrap; gap:16px">
          <Section title="Parts stock">
              {@const ps = s.parts_stock}
              <dl class="kv">
                <dt>Total SKUs</dt><dd><strong>{ps.total_skus.toLocaleString()}</strong></dd>
                <dt>On hand</dt><dd><strong>{ps.total_on_hand.toLocaleString()}</strong></dd>
                <dt>Allocated</dt><dd><strong>{ps.total_allocated.toLocaleString()}</strong></dd>
                <dt>Available</dt><dd><strong>{ps.total_available.toLocaleString()}</strong></dd>
                <dt>Below reorder</dt>
                <dd>
                  <strong style={`color:${ps.below_reorder_count > 0 ? '#dc2626' : '#059669'}`}>
                    {ps.below_reorder_count.toLocaleString()}
                  </strong>
                </dd>
              </dl>
          </Section>

          <Section title="Inbound POs">
              {@const ip = s.inbound_pos}
              <dl class="kv">
                <dt>Open</dt><dd><strong>{ip.total_open.toLocaleString()}</strong></dd>
                <dt>Draft</dt><dd><strong>{ip.draft_count.toLocaleString()}</strong></dd>
                <dt>Submitted</dt><dd><strong>{ip.submitted_count.toLocaleString()}</strong></dd>
                <dt>In transit</dt><dd><strong>{ip.in_transit_count.toLocaleString()}</strong></dd>
                <dt>Late</dt>
                <dd>
                  <strong style={`color:${ip.late_count > 0 ? '#dc2626' : '#059669'}`}>
                    {ip.late_count.toLocaleString()}
                  </strong>
                </dd>
                <dt>Arriving this week</dt>
                <dd><strong>{ip.arriving_this_week_count.toLocaleString()}</strong></dd>
              </dl>
          </Section>

          <Section title="Outbound shipments">
              {@const os = s.outbound_shipments}
              <dl class="kv">
                <dt>Label created</dt><dd><strong>{os.label_created.toLocaleString()}</strong></dd>
                <dt>Picked up</dt><dd><strong>{os.picked_up.toLocaleString()}</strong></dd>
                <dt>In transit</dt><dd><strong>{os.in_transit.toLocaleString()}</strong></dd>
                <dt>Exception</dt>
                <dd>
                  <strong style={`color:${os.exception > 0 ? '#dc2626' : '#059669'}`}>
                    {os.exception.toLocaleString()}
                  </strong>
                </dd>
                <dt>Delivered (7d)</dt><dd><strong>{os.delivered_7d.toLocaleString()}</strong></dd>
              </dl>
          </Section>
        </div>

        <section class="tab-section">
          {#if s.parts_stock.below_reorder_items.length === 0}
            <h3 style="margin-top:0">Below reorder</h3>
            <p class="empty">All SKUs at or above reorder point.</p>
          {:else}
            <h3 style="margin-top:0">
              Below reorder · showing {s.parts_stock.below_reorder_items.length} of
              {s.parts_stock.below_reorder_count.toLocaleString()}
            </h3>
            <table class="data-table data-table-striped">
              <thead>
                <tr>
                  <th>Part SKU</th>
                  <th>Bin</th>
                  <th class="num">On hand</th>
                  <th class="num">Allocated</th>
                  <th class="num">Available</th>
                  <th class="num">Reorder pt</th>
                </tr>
              </thead>
              <tbody>
                {#each s.parts_stock.below_reorder_items as r (r.part_sku)}
                  <tr class="data-table-row-link">
                    <td class="mono">
                      <Link to={entityHref('part', r.part_sku)}>
                        {r.part_sku}
                      </Link>
                    </td>
                    <td class="mono">{r.bin}</td>
                    <td class="num">{r.on_hand.toLocaleString()}</td>
                    <td class="num">{r.allocated.toLocaleString()}</td>
                    <td class="num">{r.available.toLocaleString()}</td>
                    <td class="num">{r.reorder_point.toLocaleString()}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        </section>
      </div>
    {/if}
  {:else if tab === 'inventory'}
    <div class="catalog-layout" style="margin-top:16px">
      <aside class="catalog-filters">
        <FilterGroup label="Status">
            <FilterButton active={invFilter === 'all'} onclick={() => (invFilter = 'all')}>
              All ({inventoryRows.length})
            </FilterButton>
            <FilterButton active={invFilter === 'critical'} onclick={() => (invFilter = 'critical')}>
              Critical / Out ({invCritical})
            </FilterButton>
            <FilterButton active={invFilter === 'low'} onclick={() => (invFilter = 'low')}>
              Low ({invLow})
            </FilterButton>
        </FilterGroup>
      </aside>

      <section class="list-section">
        {#if loadFailed}
          <p class="empty load-failed" role="alert">
            Couldn't load inventory — {loadFailed}
          </p>
        {:else if invVisible.length === 0}
          <p class="empty">No items match that filter.</p>
        {:else}
          <table class="data-table data-table-striped">
            <thead>
              <tr>
                <SortHeader sort={invSort} key="sku">Part SKU</SortHeader>
                <SortHeader sort={invSort} key="bin">Bin</SortHeader>
                <SortHeader sort={invSort} key="on_hand" num={true}>On hand</SortHeader>
                <SortHeader sort={invSort} key="allocated" num={true}>Allocated</SortHeader>
                <SortHeader sort={invSort} key="available" num={true}>Available</SortHeader>
                <SortHeader sort={invSort} key="reorder" num={true}>Reorder pt</SortHeader>
                <SortHeader sort={invSort} key="status">Status</SortHeader>
              </tr>
            </thead>
            <tbody>
              {#each invSorted as r (r.item.part_sku)}
                <tr class="data-table-row-link">
                  <td class="mono">
                    <Link to={entityHref('part', r.item.part_sku)}>
                      {r.item.part_sku}
                    </Link>
                  </td>
                  <td class="mono">{r.item.bin}</td>
                  <td class="num">{r.item.on_hand}</td>
                  <td class="num">{r.item.allocated}</td>
                  <td class="num">{r.available}</td>
                  <td class="num">{r.item.reorder_point}</td>
                  <td><StatusChip value={r.status} tone={stockTone(r.status)} /></td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </section>
    </div>
  {:else if tab === 'receiving'}
    <div class="catalog-layout" style="margin-top:16px">
      <aside class="catalog-filters">
        <FilterGroup label="PO status">
            <FilterButton active={poFilter === 'open'} onclick={() => (poFilter = 'open')}>
              Open ({openPoCount})
            </FilterButton>
            <FilterButton active={poFilter === 'all'} onclick={() => (poFilter = 'all')}>
              All ({purchaseOrders.length})
            </FilterButton>
            {#each PO_STATUSES as s (s)}
              {@const c = poCounts.get(s) ?? 0}
              {#if c > 0}
                <FilterButton active={poFilter === s} onclick={() => (poFilter = s)}>
                  {s.replace(/-/g, ' ')} ({c})
                </FilterButton>
              {/if}
            {/each}
        </FilterGroup>
      </aside>

      <section class="list-section">
        <div style="margin-bottom:12px; display:flex; gap:8px; align-items:center">
          <button class="hr-action-btn" onclick={() => (showCreatePo = !showCreatePo)}>
            {showCreatePo ? 'Cancel' : 'Create PO'}
          </button>
          {#if createPoStatus}
            <span
              style={`font-size:12px; color:${createPoStatus.startsWith('Error') ? '#dc2626' : '#16a34a'}`}
            >
              {createPoStatus}
            </span>
          {/if}
        </div>

        {#if showCreatePo}
          <div
            style="padding:12px 16px; border:1px solid #e7e5e4; border-radius:8px; margin-bottom:16px; background:#fafaf9"
          >
            <div style="display:flex; gap:8px; flex-wrap:wrap; align-items:end">
              <div>
                <label for="cpo-vendor" style="display:block; font-size:11px; font-weight:600; color:#78716c; margin-bottom:2px">Vendor</label>
                <input
                  id="cpo-vendor"
                  class="hr-select"
                  bind:value={createPoVendor}
                  placeholder="e.g. Riverside Malting"
                  style="width:180px"
                />
              </div>
              <div>
                <label for="cpo-sku" style="display:block; font-size:11px; font-weight:600; color:#78716c; margin-bottom:2px">Part SKU</label>
                <select id="cpo-sku" class="hr-select" bind:value={createPoSku} style="width:200px">
                  <option value="">Select part...</option>
                  {#each inventory as item (item.part_sku)}
                    <option value={item.part_sku}>{item.part_sku}</option>
                  {/each}
                </select>
              </div>
              <div>
                <label for="cpo-qty" style="display:block; font-size:11px; font-weight:600; color:#78716c; margin-bottom:2px">Qty</label>
                <input
                  id="cpo-qty"
                  class="hr-select"
                  type="number"
                  min="1"
                  bind:value={createPoQty}
                  style="width:60px"
                />
              </div>
              <div>
                <label for="cpo-cost" style="display:block; font-size:11px; font-weight:600; color:#78716c; margin-bottom:2px">Unit cost ($)</label>
                <input
                  id="cpo-cost"
                  class="hr-select"
                  type="number"
                  min="1"
                  bind:value={createPoCost}
                  style="width:80px"
                />
              </div>
              <button
                class="hr-action-btn"
                onclick={handleCreatePo}
                disabled={!createPoVendor || !createPoSku}
              >
                Submit
              </button>
            </div>
          </div>
        {/if}

        {#if loadFailed}
          <p class="empty load-failed" role="alert">
            Couldn't load purchase orders — {loadFailed}
          </p>
        {:else if poVisible.length === 0}
          <p class="empty">No POs match that filter.</p>
        {:else}
          <table class="data-table data-table-striped">
            <thead>
              <tr>
                <th>PO ID</th>
                <th>Vendor</th>
                <th>Status</th>
                <th>Placed</th>
                <th>Expected</th>
                <th>Lines</th>
              </tr>
            </thead>
            <tbody>
              {#each poVisible as po (po.id)}
                <tr id={`po-${po.id}`} class="data-table-row-link">
                  <td class="mono"><EntityLink kind="po" id={po.id} /></td>
                  <td><EntityLink kind="vendor" id={po.vendor} /></td>
                  <td>{po.status.replace(/-/g, ' ')}</td>
                  <td>{po.placed_on}</td>
                  <td>{po.expected_on}</td>
                  <td class="prose-cell">
                    {po.lines
                      .map(
                        (l) =>
                          `${(l as { part_sku: string; qty: number }).part_sku} x${(l as { part_sku: string; qty: number }).qty}`,
                      )
                      .join(', ')}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </section>
    </div>
  {/if}
</div>
