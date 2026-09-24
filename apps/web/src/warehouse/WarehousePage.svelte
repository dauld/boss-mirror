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
  import { warehouseHeader } from './header';
  import {
    countLabel,
    failedRead,
    failedWithReason,
    listView,
    loadingRead,
    okRead,
    type ReadState,
  } from '../data/readState';
  import { rowLink } from '@boss/web-kit/ui/RowLink';
  import { href, navigate } from '../router';

  type Tab = 'overview' | 'inventory' | 'receiving';
  const TABS: ReadonlyArray<{ id: Tab; label: string }> = [
    { id: 'overview', label: 'Overview' },
    { id: 'inventory', label: 'Inventory' },
    { id: 'receiving', label: 'Receiving' },
  ];

  let inventory = $state<InventoryItem[]>([]);
  let purchaseOrders = $state<PurchaseOrder[]>([]);
  let status = $state<WarehouseStatus | null>(null);
  /// One outcome PER READ (packet 3fba9c35 made the failure visible;
  /// backlog fcd0e29e split it): a single shared `loadFailed` blanked
  /// the Receiving tab when only items failed, naming purchase orders,
  /// and one network error rejected all three reads at once. Each starts
  /// LOADING, not ok: starting as ok claimed an answer that had not
  /// arrived, so the Inventory and Receiving tabs said "No items/POs
  /// match that filter." and the header and filters counted zeros for
  /// the whole loading window (backlog 20410830, 82674b2b, 8b1deea2).
  let itemsRead = $state<ReadState>(loadingRead);
  let ordersRead = $state<ReadState>(loadingRead);
  let statusRead = $state<ReadState>(loadingRead);
  let tab = $state<Tab>('overview');

  /// One GET, settled on its own. A refusal keeps the server's status
  /// AND its text: warehouse-status answers 503 "not configured" or 502
  /// naming the failing leg, and the page used to keep neither, so an
  /// operator could not tell the two apart (backlog 0dcb0200).
  async function read(url: string): Promise<{ state: ReadState; body: unknown }> {
    try {
      const r = await fetch(url);
      if (!r.ok) return { state: failedWithReason(r.status, await r.text()), body: null };
      return { state: okRead, body: await r.json() };
    } catch (e) {
      return { state: failedRead(e instanceof Error ? e.message : String(e)), body: null };
    }
  }

  function rowsOf<T>(body: unknown): T[] {
    if (Array.isArray(body)) return body as T[];
    return (body as { data?: T[] } | null)?.data ?? [];
  }

  async function loadAll(): Promise<void> {
    const [i, p, s] = await Promise.all([
      read('/api/inventory/items'),
      read('/api/inventory/orders'),
      read('/api/inventory/warehouse-status'),
    ]);
    [itemsRead, ordersRead, statusRead] = [i.state, p.state, s.state];
    if (i.state.kind === 'ok') inventory = rowsOf<InventoryItem>(i.body);
    if (p.state.kind === 'ok') purchaseOrders = rowsOf<PurchaseOrder>(p.body);
    if (s.state.kind === 'ok') status = s.body as WarehouseStatus;
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

  let header = $derived(
    warehouseHeader(
      { read: statusRead, body: status },
      {
        read: itemsRead,
        skus: inventory.length,
        belowReorder: inventoryRows.filter((r) => r.available <= r.item.reorder_point).length,
      },
    ),
  );

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

  let invView = $derived(listView([{ source: 'inventory', state: itemsRead }], invVisible.length));

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
  let poView = $derived(
    listView([{ source: 'purchase orders', state: ordersRead }], poVisible.length),
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
  <PageHeader eyebrow="Warehouse" title={header.title} subtitle={header.subtitle} />

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
    {#if statusRead.kind === 'loading' && !status}
      <p class="empty" style="padding:16px">Loading warehouse status…</p>
    {:else if !status}
      <p class="empty" style="padding:16px">
        Warehouse status unavailable{statusRead.kind === 'failed' ? ` — ${statusRead.error}` : '.'}
      </p>
    {:else}
      {@const s = status}
      <div class="tab-content" style="padding:16px 0; display:flex; flex-direction:column; gap:16px">
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
                  <strong style={`color:${ps.below_reorder_count > 0 ? 'var(--err)' : 'var(--ok)'}`}>
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
                  <strong style={`color:${ip.late_count > 0 ? 'var(--err)' : 'var(--ok)'}`}>
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
                  <strong style={`color:${os.exception > 0 ? 'var(--err)' : 'var(--ok)'}`}>
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
                  <tr
                    use:rowLink={{
                      onActivate: () => navigate(entityHref('part', r.part_sku)),
                      label: `Part ${r.part_sku}`,
                    }}
                  >
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
              {countLabel('All', itemsRead, inventoryRows.length)}
            </FilterButton>
            <FilterButton active={invFilter === 'critical'} onclick={() => (invFilter = 'critical')}>
              {countLabel('Critical / Out', itemsRead, invCritical)}
            </FilterButton>
            <FilterButton active={invFilter === 'low'} onclick={() => (invFilter = 'low')}>
              {countLabel('Low', itemsRead, invLow)}
            </FilterButton>
        </FilterGroup>
      </aside>

      <section class="list-section">
        {#if invView.kind === 'failed'}
          <p class="empty load-failed" role="alert">
            Couldn't load {invView.source} — {invView.error}
          </p>
        {:else if invView.kind === 'loading'}
          <p class="empty">Loading {invView.source}…</p>
        {:else if invView.kind === 'empty'}
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
                <tr
                  use:rowLink={{
                    onActivate: () => navigate(entityHref('part', r.item.part_sku)),
                    label: `Part ${r.item.part_sku}`,
                  }}
                >
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
              {countLabel('Open', ordersRead, openPoCount)}
            </FilterButton>
            <FilterButton active={poFilter === 'all'} onclick={() => (poFilter = 'all')}>
              {countLabel('All', ordersRead, purchaseOrders.length)}
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
          <button class="btn btn-sm" onclick={() => (showCreatePo = !showCreatePo)}>
            {showCreatePo ? 'Cancel' : 'Create PO'}
          </button>
          {#if createPoStatus}
            <span
              style={`font-size:12px; color:${createPoStatus.startsWith('Error') ? 'var(--err)' : 'var(--ok)'}`}
            >
              {createPoStatus}
            </span>
          {/if}
        </div>

        {#if showCreatePo}
          <div
            style="padding:12px 16px; border:1px solid var(--hairline); border-radius:8px; margin-bottom:16px; background:var(--ink-raised)"
          >
            <div style="display:flex; gap:8px; flex-wrap:wrap; align-items:end">
              <div>
                <label for="cpo-vendor" style="display:block; font-size:11px; font-weight:600; color:var(--static); margin-bottom:2px">Vendor</label>
                <input
                  id="cpo-vendor"
                  class="hr-select"
                  bind:value={createPoVendor}
                  placeholder="e.g. Riverside Malting"
                  style="width:180px"
                />
              </div>
              <div>
                <label for="cpo-sku" style="display:block; font-size:11px; font-weight:600; color:var(--static); margin-bottom:2px">Part SKU</label>
                <select id="cpo-sku" class="hr-select" bind:value={createPoSku} style="width:200px">
                  <option value="">Select part...</option>
                  {#each inventory as item (item.part_sku)}
                    <option value={item.part_sku}>{item.part_sku}</option>
                  {/each}
                </select>
              </div>
              <div>
                <label for="cpo-qty" style="display:block; font-size:11px; font-weight:600; color:var(--static); margin-bottom:2px">Qty</label>
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
                <label for="cpo-cost" style="display:block; font-size:11px; font-weight:600; color:var(--static); margin-bottom:2px">Unit cost ($)</label>
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
                class="btn btn-sm btn-primary"
                onclick={handleCreatePo}
                disabled={!createPoVendor || !createPoSku}
              >
                Submit
              </button>
            </div>
          </div>
        {/if}

        {#if poView.kind === 'failed'}
          <p class="empty load-failed" role="alert">
            Couldn't load {poView.source} — {poView.error}
          </p>
        {:else if poView.kind === 'loading'}
          <p class="empty">Loading {poView.source}…</p>
        {:else if poView.kind === 'empty'}
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
                <tr
                  id={`po-${po.id}`}
                  use:rowLink={{
                    onActivate: () => navigate(entityHref('po', po.id)),
                    label: `Purchase order ${po.id}`,
                  }}
                >
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
