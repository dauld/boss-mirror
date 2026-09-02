<script lang="ts">
  // Marketing Asset KB list.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { formatDate } from '@boss/web-kit/ui/date';
  import FilterGroup from '@boss/web-kit/ui/FilterGroup.svelte';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import SearchInput from '@boss/web-kit/ui/SearchInput.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import SortHeader from '@boss/web-kit/ui/SortHeader.svelte';
  import { createSortState } from '@boss/web-kit/ui/sort-state.svelte';
  import { type MarketingAsset } from './types';
  import { loadClasses, classesFor } from '@boss/web-kit/session/classes.svelte';
  import { href, navigate } from '../router';

  // Kind labels + the filter rail come from the Class registry
  // (subject_kind='marketing-asset', member_attribute='kind') — no
  // hardcoded option list.
  $effect(() => {
    void loadClasses('marketing-asset');
  });
  let kindRows = $derived(classesFor('marketing-asset', 'kind'));
  let kindLabel = $derived(
    new Map(kindRows.map((c): [string, string] => [c.code, c.display_name])),
  );
  let kindOptions = $derived<ReadonlyArray<string>>([
    'all',
    ...kindRows.map((c) => c.code),
  ]);

  let kind = $state<string>('all');
  let includeRetired = $state(false);
  let query = $state('');
  let assets = $state<MarketingAsset[]>([]);
  /// Non-null when the load failed — rendered instead of the empty
  /// state, so an outage never reads as "no marketing assets yet"
  /// (packet 3fba9c35, the false-empty sweep).
  let loadFailed = $state<string | null>(null);
  let loading = $state(true);

  $effect(() => {
    const k = kind;
    const r = includeRetired;
    let cancelled = false;
    loading = true;
    (async () => {
      const qs = new URLSearchParams();
      if (k !== 'all') qs.set('kind', k);
      if (r) qs.set('include_retired', 'true');
      qs.set('limit', '500');
      try {
        const resp = await fetch(`/api/catalog/marketing-assets?${qs.toString()}`);
        if (resp.ok) {
          const body = (await resp.json()) as MarketingAsset[];
          if (!cancelled) {
            assets = Array.isArray(body) ? body : [];
            loadFailed = null;
          }
        } else {
          if (!cancelled) loadFailed = `HTTP ${resp.status}`;
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

  let visible = $derived.by(() => {
    if (!query) return assets;
    const q = query.toLowerCase();
    return assets.filter((a) => {
      const hay = [
        a.id,
        a.title,
        a.description ?? '',
        ...a.tags,
        ...a.linked_device_skus,
        ...a.linked_campaign_ids,
      ]
        .join(' ')
        .toLowerCase();
      return hay.includes(q);
    });
  });

  // CAR-4: previously rendered in API arrival order. Freshest-updated
  // first is the new landing order; the count columns default
  // descending.
  type SortKey = 'title' | 'kind' | 'tags' | 'linked' | 'owner' | 'updated';
  const DESC_FIRST: ReadonlyArray<SortKey> = ['tags', 'linked', 'updated'];
  const sort = createSortState<SortKey>({ key: 'updated', dir: 'desc' }, (k) =>
    DESC_FIRST.includes(k) ? 'desc' : 'asc',
  );
  let visibleSorted = $derived(
    sort.sorted(visible, {
      title: (a) => a.title,
      kind: (a) => a.kind,
      tags: (a) => a.tags.length,
      linked: (a) =>
        a.linked_device_skus.length + a.linked_account_ids.length + a.linked_campaign_ids.length,
      owner: (a) => a.owner_id,
      updated: (a) => a.updated_at,
    }),
  );
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Know"
    title={`Marketing assets (${assets.length}${loading ? '…' : ''})`}
    subtitle="Photos, videos, decks, one-pagers, templates, and brand files."
  />

  <div class="catalog-layout">
    <aside class="catalog-filters">
      <FilterGroup label="Search">
          <SearchInput bind:value={query} placeholder="Title, tag, SKU, campaign…" />
      </FilterGroup>
      <FilterGroup label="Kind">
          {#each kindOptions as k (k)}
            <FilterButton active={kind === k} onclick={() => (kind = k)}>
              {k === 'all' ? 'All' : (kindLabel.get(k) ?? k)}
            </FilterButton>
          {/each}
      </FilterGroup>
      <FilterGroup label="Status">
          <FilterButton active={!includeRetired} onclick={() => (includeRetired = false)}>
            Active only
          </FilterButton>
          <FilterButton active={includeRetired} onclick={() => (includeRetired = true)}>
            Include retired
          </FilterButton>
      </FilterGroup>
    </aside>

    <section class="list-section">
      {#if loading && assets.length === 0}
        <p class="empty">Loading…</p>
      {:else if loadFailed}
        <p class="empty load-failed" role="alert">
          Couldn't load marketing assets — {loadFailed}
        </p>
      {:else if visible.length === 0}
        <p class="empty">
          {assets.length === 0 ? 'No marketing assets yet.' : 'No assets match those filters.'}
        </p>
      {:else}
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <SortHeader {sort} key="title">Asset</SortHeader>
              <SortHeader {sort} key="kind">Kind</SortHeader>
              <SortHeader {sort} key="tags">Tags</SortHeader>
              <SortHeader {sort} key="linked">Linked</SortHeader>
              <SortHeader {sort} key="owner">Owner</SortHeader>
              <SortHeader {sort} key="updated">Updated</SortHeader>
            </tr>
          </thead>
          <tbody>
            {#each visibleSorted as a (a.id)}
              {@const retired = Boolean(a.retired_at)}
              {@const linkedCount = a.linked_device_skus.length + a.linked_account_ids.length + a.linked_campaign_ids.length}
              <tr
                style={`cursor:pointer; opacity:${retired ? 0.55 : 1}`}
                onclick={() => navigate(href(`/ux/marketing-assets/${encodeURIComponent(a.id)}`))}
              >
                <td>
                  <a
                    href={href(`/ux/marketing-assets/${encodeURIComponent(a.id)}`)}
                    onclick={(e) => e.stopPropagation()}
                  >
                    {a.title}
                  </a>
                  {#if retired}
                    <span class="chip" style="margin-left:6px">RETIRED</span>
                  {/if}
                </td>
                <td>{a.kind ? (kindLabel.get(a.kind) ?? a.kind) : '—'}</td>
                <td style="color:var(--static); font-size:12px">
                  {a.tags.length > 0 ? a.tags.slice(0, 4).join(', ') : '—'}
                  {#if a.tags.length > 4} +{a.tags.length - 4}{/if}
                </td>
                <td style="color:var(--static); font-size:12px">
                  {linkedCount > 0 ? `${linkedCount} ${linkedCount === 1 ? 'link' : 'links'}` : '—'}
                </td>
                <td>
                  {#if a.owner_id}
                    <EntityLink kind="employee" id={a.owner_id} />
                  {:else}
                    <span style="color:var(--static)">—</span>
                  {/if}
                </td>
                <td style="color:var(--static); font-size:12px">{formatDate(a.updated_at)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  </div>
</div>
