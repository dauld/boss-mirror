<script lang="ts">
  // Assets list (kanban + table). Scope-reduced:
  // kanban rail + filterable list, same endpoints (/api/assets/summary,
  // /api/assets). The full page has a warranty-expiring + KB
  // panel that we're deferring to phase 2.

  import { navigate, href } from '../router';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { rowLink } from '@boss/web-kit/ui/RowLink';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import OverflowBanner from '@boss/web-kit/ui/OverflowBanner.svelte';
  import { fetchPaged, isCapped, type Paged } from '../data/paginated';
  import { getLabel } from '@boss/web-kit/session/manifest.svelte';
  import type { Asset, AssetsSummary, AssetLifecyclePhase } from './types';

  const PHASE_ORDER: ReadonlyArray<AssetLifecyclePhase> = [
    'received', 'triaging', 'refurbing', 'qa', 'ready',
    'shipped', 'installed', 'out-for-service', 'decommissioned',
  ];
  const PHASE_LABEL: Record<AssetLifecyclePhase, string> = {
    registered: 'Registered',
    received: 'Received',
    triaging: 'In triage',
    refurbing: 'Refurb',
    qa: 'QA',
    ready: 'Ready',
    shipped: 'Shipped',
    installed: 'Installed',
    'out-for-service': 'In service',
    decommissioned: 'Decom.',
  };

  let devicesPage = $state<Paged<Asset> | null>(null);
  let summary = $state<AssetsSummary | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  let phaseFilter = $state<AssetLifecyclePhase | 'all'>('all');
  let query = $state('');

  let devices = $derived(devicesPage?.data ?? []);

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const [devPaged, sumResp] = await Promise.all([
          fetchPaged<Asset>('/api/assets?limit=500'),
          fetch('/api/assets/summary'),
        ]);
        if (devPaged.kind === 'failed') throw new Error(devPaged.error);
        if (!sumResp.ok) throw new Error(`summary HTTP ${sumResp.status}`);
        const sumBody = (await sumResp.json()) as AssetsSummary;
        if (!cancelled) {
          devicesPage = devPaged.page;
          summary = sumBody;
          loading = false;
        }
      } catch (e) {
        if (!cancelled) {
          error = e instanceof Error ? e.message : String(e);
          loading = false;
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let phaseCounts = $derived(
    new Map((summary?.phase_counts ?? []).map((r) => [r.phase, r.count])),
  );
  let totalDevices = $derived(summary?.total_systems ?? devices.length);
  let installedCount = $derived(phaseCounts.get('installed') ?? 0);

  let visible = $derived(
    devices.filter((d) => {
      if (phaseFilter !== 'all' && d.phase !== phaseFilter) return false;
      if (query) {
        const q = query.toLowerCase();
        const hay = `${d.asset_id} ${d.sku ?? ''}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    }),
  );
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow={getLabel('nav.assets_label', 'Assets')}
    title={`${totalDevices.toLocaleString()} ${getLabel('assets.page_title', 'tracked assets')}`}
    subtitle={`${installedCount.toLocaleString()} installed · ${(summary?.open_tickets_total ?? 0).toLocaleString()} open tickets · ${(summary?.warranty_expiring_30d ?? 0).toLocaleString()} warranties expiring (30d)`}
  />

  {#if isCapped(devicesPage)}
    <OverflowBanner
      showing={devices.length}
      total={devicesPage!.total}
      noun="devices loaded for the list below"
      hint="Kanban + per-row counts only see this window. Narrow by phase or search to drill into the rest."
    />
  {/if}

  <section
    class="kanban"
    style="grid-template-columns: repeat({PHASE_ORDER.length}, 1fr)"
  >
    {#each PHASE_ORDER as phase (phase)}
      <div class="kanban-col" data-stage={phase}>
        <div class="kanban-head">
          <div class="kanban-label">{PHASE_LABEL[phase]}</div>
          <div class="kanban-count">
            {(phaseCounts.get(phase) ?? 0).toLocaleString()}
          </div>
        </div>
      </div>
    {/each}
  </section>

  <div class="catalog-layout" style="margin-top: 24px">
    <aside class="catalog-filters">
      <div class="filter-group">
        <div class="filter-label">Search</div>
        <input
          type="search"
          bind:value={query}
          placeholder="Serial, sku…"
          class="search-input"
        />
      </div>
      <div class="filter-group">
        <div class="filter-label">Phase</div>
        <button
          type="button"
          class="filter-button {phaseFilter === 'all' ? 'filter-button-active' : ''}"
          aria-pressed={phaseFilter === 'all'}
          onclick={() => (phaseFilter = 'all')}
        >
          All ({totalDevices.toLocaleString()})
        </button>
        {#each PHASE_ORDER as phase (phase)}
          {@const count = phaseCounts.get(phase) ?? 0}
          {#if count > 0}
            <button
              type="button"
              class="filter-button {phaseFilter === phase ? 'filter-button-active' : ''}"
              aria-pressed={phaseFilter === phase}
              onclick={() => (phaseFilter = phase)}
            >
              {PHASE_LABEL[phase]} ({count.toLocaleString()})
            </button>
          {/if}
        {/each}
      </div>
    </aside>

    <section class="list-section">
      {#if loading}
        <p class="empty">Loading…</p>
      {:else if error}
        <p class="empty">Couldn't load assets: {error}</p>
      {:else if visible.length === 0}
        <p class="empty">{getLabel('assets.empty_state', 'No assets match.')}</p>
      {:else}
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>BOSS ID</th>
              <th>SKU</th>
              <th>Phase</th>
              <th>Holder</th>
              <th>Warranty</th>
              <th class="num">{getLabel('assets.tickets_label', 'Open SRs')}</th>
              <th>Last event</th>
            </tr>
          </thead>
          <tbody>
            {#each visible as d (d.asset_id)}
              <tr
                use:rowLink={{
                  onActivate: () => navigate(entityHref('asset', d.asset_id)),
                  label: `Asset ${d.asset_id}`,
                }}
              >
                <td class="mono">
                  <Link to={entityHref('asset', d.asset_id)}>{d.asset_id}</Link>
                </td>
                <td class="mono">{d.sku ?? '—'}</td>
                <td>{PHASE_LABEL[d.phase]}</td>
                <td class="mono">{d.holder_id ?? '—'}</td>
                <td>{d.warranty_through ?? 'out'}</td>
                <td class="num">{d.open_ticket_count}</td>
                <td>{d.last_event_at}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  </div>
</div>
