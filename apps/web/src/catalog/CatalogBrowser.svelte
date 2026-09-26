<script lang="ts">
  // Catalog browser — port of apps/web/src/catalog/CatalogBrowser.tsx.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import FilterGroup from '@boss/web-kit/ui/FilterGroup.svelte';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import SearchInput from '@boss/web-kit/ui/SearchInput.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { formatMoney } from '@boss/web-kit/ui/money';
  import {
    CATEGORY_LABEL,
    type DeviceModel,
    type DeviceCategory,
  } from './types';
  import { href } from '../router';
  import { getLabel } from '@boss/web-kit/session/manifest.svelte';

  let catalog = $state<DeviceModel[]>([]);
  /// Non-null when the load failed — rendered instead of the empty
  /// state, so an outage never reads as an empty catalog (packet
  /// 3fba9c35, the false-empty sweep).
  let loadFailed = $state<string | null>(null);
  let loading = $state(true);
  let categoryFilter = $state<DeviceCategory | 'all'>('all');
  let useCaseFilter = $state<string | 'all'>('all');
  let query = $state('');

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const r = await fetch('/api/catalog/models');
        if (r.ok) {
          const body = await r.json();
          if (!cancelled) {
            catalog = Array.isArray(body) ? body : (body.data ?? []);
            loadFailed = null;
          }
        } else {
          if (!cancelled) loadFailed = `HTTP ${r.status}`;
        }
      } catch (e) {
        if (!cancelled) loadFailed = e instanceof Error ? e.message : String(e);
      }
      if (!cancelled) loading = false;
    })();
    return () => {
      cancelled = true;
    };
  });

  let allCategories = $derived([
    ...new Set(catalog.map((d) => d.category)),
  ] as DeviceCategory[]);
  let allUseCases = $derived([
    ...new Set(catalog.flatMap((d) => d.commerce.use_cases)),
  ]);

  let visible = $derived(
    catalog.filter((d) => {
      if (categoryFilter !== 'all' && d.category !== categoryFilter) return false;
      if (useCaseFilter !== 'all' && !d.commerce.use_cases.includes(useCaseFilter)) {
        return false;
      }
      if (
        query &&
        !`${d.name} ${d.manufacturer} ${d.commerce.tagline}`.toLowerCase().includes(query.toLowerCase())
      ) {
        return false;
      }
      return true;
    }),
  );
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Knowledge Base"
    title={`${catalog.length} ${getLabel('catalog.page_title', 'catalog systems')}`}
    subtitle="Shared reference across the operating model"
  />

  <div class="catalog-layout">
    <aside class="catalog-filters">
      <FilterGroup label="Search">
          <SearchInput bind:value={query} placeholder="Name or tagline…" label="Search the catalog" />
      </FilterGroup>

      <FilterGroup label="Category">
          <FilterButton
            active={categoryFilter === 'all'}
            onclick={() => (categoryFilter = 'all')}
          >
            All ({catalog.length})
          </FilterButton>
          {#each allCategories as c (c)}
            <FilterButton
              active={categoryFilter === c}
              onclick={() => (categoryFilter = c)}
            >
                {CATEGORY_LABEL[c]} ({catalog.filter((d) => d.category === c).length})
            </FilterButton>
          {/each}
      </FilterGroup>

      <FilterGroup label="Use case">
          <FilterButton
            active={useCaseFilter === 'all'}
            onclick={() => (useCaseFilter = 'all')}
          >
            All
          </FilterButton>
          {#each allUseCases as u (u)}
            <FilterButton
              active={useCaseFilter === u}
              onclick={() => (useCaseFilter = u)}
            >
              {u}
            </FilterButton>
          {/each}
      </FilterGroup>
    </aside>

    <section class="catalog-grid">
      {#if loading && catalog.length === 0}
        <p class="empty">Loading catalog…</p>
      {:else if loadFailed}
        <p class="empty load-failed" role="alert">
          Couldn't load the catalog — {loadFailed}
        </p>
      {:else if visible.length === 0}
        <p class="empty">{getLabel('catalog.empty_state', 'No devices match those filters.')}</p>
      {:else}
        {#each visible as d (d.sku)}
          <Link className="catalog-card" to={href(`/ux/catalog/${d.sku}`)}>
              <div class="catalog-card-tile" data-category={d.category}>
                <div class="catalog-card-chip">{CATEGORY_LABEL[d.category]}</div>
              </div>
              <div class="catalog-card-body">
                <div class="catalog-card-name">{d.name}</div>
                <div class="catalog-card-tagline">{d.commerce.tagline}</div>
                <div class="catalog-card-meta">
                  <span class="catalog-card-price">
                    {formatMoney({ amount_cents: d.commerce.list_price_new_cents, currency: d.commerce.currency })}
                  </span>
                  <span class="catalog-card-year">{d.model_year}</span>
                </div>
                <div class="catalog-card-indications">
                  {#each d.commerce.use_cases.slice(0, 3) as u (u)}
                    <span class="chip">{u}</span>
                  {/each}
                  {#if d.commerce.use_cases.length > 3}
                    <span class="chip chip-muted">
                      +{d.commerce.use_cases.length - 3}
                    </span>
                  {/if}
                </div>
              </div>
          </Link>
        {/each}
      {/if}
    </section>
  </div>
</div>
