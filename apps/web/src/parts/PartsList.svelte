<script lang="ts">
  // Parts list — port of apps/web/src/parts/PartsList.tsx.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import FilterGroup from '@boss/web-kit/ui/FilterGroup.svelte';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import SearchInput from '@boss/web-kit/ui/SearchInput.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import StatusChip from '@boss/web-kit/ui/StatusChip.svelte';
  import {
    collectParts,
    kindFromSku,
    stockStatus,
    stockTone,
    type CatalogPart,
    type DeviceModel,
    type InventoryItem,
    type PurchaseOrder,
    type StockStatus,
  } from './types';
  import { countedLabel, partsHeader, stockRead } from './stock-counts';
  import { rowLink } from '@boss/web-kit/ui/RowLink';
  import { href, navigate } from '../router';
  import { getLabel } from '@boss/web-kit/session/manifest.svelte';
  import { departmentLabel } from '@boss/web-kit/nav';
  import { departments } from '@boss/web-kit/session/departments.svelte';
  import DepartmentThirds from '../departments/DepartmentThirds.svelte';

  // The department whose packets this page lists beside its parts —
  // the catalog entry's `department`, handed in by App.svelte the way
  // the two queues get theirs. The four reads below are stock; none is
  // a jobs read, so until this prop a warehouse packet could never
  // appear on the warehouse's page (backlog 044dffa1, page audit
  // 63d810aa). Empty draws no panel.
  let { department = '' } = $props<{ department?: string }>();
  const departmentName = $derived(departmentLabel(department, departments()));

  type RowKind = 'ingredient' | 'packaging' | 'spare' | 'consumable';
  type Filter = 'all' | 'needs-attention' | RowKind | StockStatus;

  let models = $state<DeviceModel[]>([]);
  let inventory = $state<InventoryItem[]>([]);
  let pos = $state<PurchaseOrder[]>([]);
  // The brewery seeds parts directly into the `parts` table —
  // no system_models.spare_parts/consumables linkage. Pull the
  // canonical /api/catalog/parts list and fall back to the
  // device-asset shape (collectParts) for tenants that DO use
  // satellite linkage.
  let catalogParts = $state<CatalogPart[]>([]);
  /// Non-null when a row-source load failed — rendered instead of the
  /// empty state, so an outage never reads as "no parts" (packet
  /// 3fba9c35, the false-empty sweep).
  let loadFailed = $state<string | null>(null);
  let loading = $state(true);
  // Defaults to "all" so the playground shows the SKUs on first
  // load. Operators rebrowsing for stockouts click "Needs
  // attention" themselves; first-impression empty-state was
  // confusing.
  let filter = $state<Filter>('all');
  let query = $state('');

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const [mResp, iResp, pResp, cpResp] = await Promise.all([
          fetch('/api/catalog/models'),
          fetch('/api/inventory/items'),
          fetch('/api/inventory/orders'),
          fetch('/api/catalog/parts'),
        ]);
        // The row sources (models, inventory, catalog parts) are the
        // page's primary data — any of them failing fails the list.
        // The PO list only feeds "on order" counts and degrades.
        const primaryDown = [mResp, iResp, cpResp].find((r) => !r.ok);
        const mBody = mResp.ok ? await mResp.json() : [];
        const iBody = iResp.ok ? await iResp.json() : [];
        const pBody = pResp.ok ? await pResp.json() : [];
        const cpBody = cpResp.ok ? await cpResp.json() : [];
        if (!cancelled) {
          models = Array.isArray(mBody) ? mBody : (mBody.data ?? []);
          inventory = Array.isArray(iBody) ? iBody : (iBody.data ?? []);
          pos = Array.isArray(pBody) ? pBody : (pBody.data ?? []);
          catalogParts = Array.isArray(cpBody) ? cpBody : (cpBody.data ?? []);
          loadFailed = primaryDown ? `HTTP ${primaryDown.status}` : null;
          loading = false;
        }
      } catch (e) {
        if (!cancelled) {
          loadFailed = e instanceof Error ? e.message : String(e);
          loading = false;
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let catalogSkuSet = $derived(new Set(models.map((m) => m.sku)));
  let parts = $derived(collectParts(models));
  let catalogPartBySku = $derived(
    new Map(catalogParts.map((p) => [p.part_sku, p])),
  );

  let onOrder = $derived.by(() => {
    const m = new Map<string, number>();
    for (const po of pos) {
      if (po.status === 'received' || po.status === 'closed') continue;
      for (const line of po.lines) {
        m.set(line.part_sku, (m.get(line.part_sku) ?? 0) + line.qty);
      }
    }
    return m;
  });

  type Row = {
    item: InventoryItem;
    name: string;
    description: string;
    kind: RowKind;
    used_by: number;
    status: StockStatus;
    on_order: number;
  };

  let rows = $derived<Row[]>(
    inventory.map((item) => {
      // Prefer the device-catalog satellite linkage when it
      // exists (used-device-shop shape — gives the "used by N
      // models" count). Fall back to /api/catalog/parts when
      // the part isn't linked to a system_model (brewery
      // shape — ingredients + packaging).
      const meta = parts.find((p) => p.sku === item.part_sku);
      const flat = catalogPartBySku.get(item.part_sku);
      return {
        item,
        name: meta?.part.name ?? flat?.name ?? item.part_sku,
        description: meta?.part.description ?? flat?.description ?? '',
        kind: meta?.kind ?? kindFromSku(item.part_sku),
        used_by: meta?.used_by.length ?? 0,
        status: stockStatus(item),
        on_order: onOrder.get(item.part_sku) ?? 0,
      };
    }),
  );

  let counts = $derived({
    total: rows.length,
    out: rows.filter((r) => r.status === 'out').length,
    critical: rows.filter((r) => r.status === 'critical').length,
    low: rows.filter((r) => r.status === 'low').length,
    healthy: rows.filter((r) => r.status === 'healthy').length,
    spare: rows.filter((r) => r.kind === 'spare').length,
    consumable: rows.filter((r) => r.kind === 'consumable').length,
    ingredient: rows.filter((r) => r.kind === 'ingredient').length,
    packaging: rows.filter((r) => r.kind === 'packaging').length,
  });
  let attention = $derived(counts.out + counts.critical + counts.low);

  // The header and the filter buttons count only a list that was read —
  // backlog f867d71c: they counted the `[]` the inventory starts as, so
  // a loading or failed read printed "0 need attention · 0 out · 0
  // critical" and All (0) above an honest "Couldn't load parts".
  let read = $derived(stockRead(loading, loadFailed));
  let header = $derived(
    partsHeader(read, getLabel('parts.page_title', 'parts'), {
      total: counts.total,
      attention,
      out: counts.out,
      critical: counts.critical,
    }),
  );

  let visible = $derived(
    rows.filter((r) => {
      if (filter === 'needs-attention' && r.status === 'healthy') return false;
      if (
        (filter === 'spare' ||
          filter === 'consumable' ||
          filter === 'ingredient' ||
          filter === 'packaging') &&
        r.kind !== filter
      ) return false;
      if (
        (filter === 'out' || filter === 'critical' || filter === 'low' || filter === 'healthy') &&
        r.status !== filter
      ) return false;
      if (query) {
        const q = query.toLowerCase();
        if (!`${r.item.part_sku} ${r.name} ${r.description}`.toLowerCase().includes(q)) {
          return false;
        }
      }
      return true;
    }),
  );

  let sortedVisible = $derived.by(() => {
    const rank: Record<StockStatus, number> = { out: 0, critical: 1, low: 2, healthy: 3 };
    return [...visible].sort((a, b) => rank[a.status] - rank[b.status]);
  });
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Inventory"
    title={header.title}
    subtitle={header.subtitle}
  />

  <div class="catalog-layout">
    <aside class="catalog-filters">
      <FilterGroup label="Search">
          <SearchInput bind:value={query} placeholder="SKU, name…" label="Search" />
      </FilterGroup>

      <FilterGroup label="Stock status">
          <FilterButton active={filter === 'needs-attention'} onclick={() => (filter = 'needs-attention')}>
            {countedLabel('Needs attention', attention, read)}
          </FilterButton>
          <FilterButton active={filter === 'all'} onclick={() => (filter = 'all')}>
            {countedLabel('All', counts.total, read)}
          </FilterButton>
          <FilterButton active={filter === 'out'} onclick={() => (filter = 'out')}>
            {countedLabel('Out of stock', counts.out, read)}
          </FilterButton>
          <FilterButton active={filter === 'critical'} onclick={() => (filter = 'critical')}>
            {countedLabel('Critical', counts.critical, read)}
          </FilterButton>
          <FilterButton active={filter === 'low'} onclick={() => (filter = 'low')}>
            {countedLabel('Low', counts.low, read)}
          </FilterButton>
          <FilterButton active={filter === 'healthy'} onclick={() => (filter = 'healthy')}>
            {countedLabel('Healthy', counts.healthy, read)}
          </FilterButton>
      </FilterGroup>

      <FilterGroup label="Kind">
          {#if counts.ingredient > 0}
            <FilterButton active={filter === 'ingredient'} onclick={() => (filter = 'ingredient')}>
              {countedLabel('Ingredients', counts.ingredient, read)}
            </FilterButton>
          {/if}
          {#if counts.packaging > 0}
            <FilterButton active={filter === 'packaging'} onclick={() => (filter = 'packaging')}>
              {countedLabel('Packaging', counts.packaging, read)}
            </FilterButton>
          {/if}
          {#if counts.spare > 0}
            <FilterButton active={filter === 'spare'} onclick={() => (filter = 'spare')}>
              {countedLabel('Spare parts', counts.spare, read)}
            </FilterButton>
          {/if}
          {#if counts.consumable > 0}
            <FilterButton active={filter === 'consumable'} onclick={() => (filter = 'consumable')}>
              {countedLabel('Consumables', counts.consumable, read)}
            </FilterButton>
          {/if}
      </FilterGroup>
    </aside>

    <section class="list-section">
      {#if loading}
        <p class="empty">Loading…</p>
      {:else if loadFailed}
        <p class="empty load-failed" role="alert">
          Couldn't load parts — {loadFailed}
        </p>
      {:else if visible.length === 0}
        <p class="empty">No parts match those filters.</p>
      {:else}
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>Part SKU</th>
              <th>Name</th>
              <th>Kind</th>
              <th class="num">On hand</th>
              <th class="num">Allocated</th>
              <th class="num">Reorder pt</th>
              <th class="num">On order</th>
              <th>Status</th>
              <th class="num">Used by</th>
              <th>Bin</th>
            </tr>
          </thead>
          <tbody>
            {#each sortedVisible as r (r.item.part_sku)}
              <tr
                use:rowLink={{
                  onActivate: () => navigate(entityHref('part', r.item.part_sku)),
                  label: `${r.name} (${r.item.part_sku})`,
                }}
              >
                <td class="mono">
                  <Link to={entityHref('part', r.item.part_sku)}>
                    {r.item.part_sku}
                  </Link>
                </td>
                <td>{r.name}</td>
                <td>{r.kind}</td>
                <td class="num">{r.item.on_hand}</td>
                <td class="num">{r.item.allocated}</td>
                <td class="num">{r.item.reorder_point}</td>
                <td class="num">{r.on_order > 0 ? r.on_order : '—'}</td>
                <td><StatusChip value={r.status} tone={stockTone(r.status)} /></td>
                <td class="num">
                  {catalogSkuSet.has(r.item.part_sku) ? '—' : r.used_by}
                </td>
                <td class="mono">{r.item.bin}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  </div>

  {#if department}
    <!-- Apart from the parts table (its own class, not list-section),
         so nothing that reads the stock list reads a packet. -->
    <section class="department-jobs">
      <h2>{departmentName} jobs</h2>
      <DepartmentThirds code={department} />
    </section>
  {/if}
</div>
