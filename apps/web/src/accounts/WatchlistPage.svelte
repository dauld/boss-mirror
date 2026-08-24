<script lang="ts">
  // Full churn watchlist — port of apps/web/src/accounts/WatchlistPage.tsx.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import FilterGroup from '@boss/web-kit/ui/FilterGroup.svelte';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import SearchInput from '@boss/web-kit/ui/SearchInput.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import type { Account } from './types';
  import {
    fetchAccountDirectory,
    fetchRiskScores,
    type RiskScoresState,
  } from './riskScores';

  type SortKey =
    | 'score'
    | 'name'
    | 'days_since_last_invoice'
    | 'open_ticket_count'
    | 'days_since_last_note';
  type SortDir = 'asc' | 'desc';
  type Tier = 'all' | 'platinum' | 'gold' | 'silver';
  type Bucket = 'all' | 'high' | 'mid' | 'low';

  let loadState = $state<RiskScoresState>({ kind: 'loading' });
  let accounts = $state<ReadonlyArray<Account>>([]);

  let query = $state('');
  let tier = $state<Tier>('all');
  let bucket = $state<Bucket>('all');
  let sortKey = $state<SortKey>('score');
  let sortDir = $state<SortDir>('desc');

  $effect(() => {
    let cancelled = false;
    (async () => {
      // Both fetches classify their own answer (riskScores.ts), so a
      // shape this page cannot read lands in `error` instead of
      // becoming an undefined array the render trips over — and the
      // directory, which only decorates rows with tier + city, can no
      // longer take the risk scores down with it.
      const [scores, directory] = await Promise.all([
        fetchRiskScores(),
        fetchAccountDirectory(),
      ]);
      if (cancelled) return;
      loadState = scores;
      accounts = directory;
    })();
    return () => {
      cancelled = true;
    };
  });

  let accountById = $derived.by(() => {
    const m = new Map<string, Account>();
    for (const p of accounts) m.set(p.id, p);
    return m;
  });

  function scoreTone(score: number): 'high' | 'mid' | 'low' {
    if (score >= 50) return 'high';
    if (score >= 25) return 'mid';
    return 'low';
  }
  function nullableCompare(a: number | null, b: number | null): number {
    if (a === null && b === null) return 0;
    if (a === null) return -1;
    if (b === null) return 1;
    return a - b;
  }
  function formatDays(d: number | null): string {
    return d === null ? '—' : `${d}d`;
  }

  let rows = $derived(
    loadState.kind === 'ready' ? loadState.scores : [],
  );

  let filtered = $derived(
    rows.filter((r) => {
      const account = accountById.get(r.account_id);
      const accountTier = account?.tier;
      if (tier !== 'all' && accountTier !== tier) return false;
      if (bucket !== 'all' && scoreTone(r.score) !== bucket) return false;
      if (query) {
        const q = query.toLowerCase();
        const hay = `${r.account_name} ${r.top_factor} ${account?.city ?? ''}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    }),
  );

  let sorted = $derived.by(() => {
    const mult = sortDir === 'asc' ? 1 : -1;
    return [...filtered].sort((a, b) => {
      switch (sortKey) {
        case 'name':
          return mult * a.account_name.localeCompare(b.account_name);
        case 'score':
          return mult * (a.score - b.score);
        case 'open_ticket_count':
          return mult * (a.factors.open_ticket_count - b.factors.open_ticket_count);
        case 'days_since_last_invoice':
          return (
            mult *
            nullableCompare(
              a.factors.days_since_last_invoice,
              b.factors.days_since_last_invoice,
            )
          );
        case 'days_since_last_note':
          return (
            mult *
            nullableCompare(
              a.factors.days_since_last_note,
              b.factors.days_since_last_note,
            )
          );
      }
    });
  });

  function bucketCount(b: Exclude<Bucket, 'all'>): number {
    return rows.filter((r) => scoreTone(r.score) === b).length;
  }

  function setSort(k: SortKey): void {
    if (k === sortKey) {
      sortDir = sortDir === 'asc' ? 'desc' : 'asc';
    } else {
      sortKey = k;
      sortDir = k === 'name' ? 'asc' : 'desc';
    }
  }

  function arrowFor(k: SortKey): string {
    if (sortKey !== k) return '';
    return sortDir === 'asc' ? ' ↑' : ' ↓';
  }
</script>

{#if loadState.kind === 'loading'}
  <div class="catalog theme-exec">
    <PageHeader eyebrow="Churn watchlist" title="Loading…" />
  </div>
{:else if loadState.kind === 'error'}
  <div class="catalog theme-exec">
    <PageHeader eyebrow="Churn watchlist" title="Couldn't load watchlist" />
    <p class="empty">{loadState.message}</p>
  </div>
{:else}
  <div class="catalog theme-exec">
    <PageHeader
      eyebrow="Customers"
      title="Churn watchlist"
      subtitle={`${rows.length} accounts scored · ${filtered.length} shown`}
    />

    <div class="catalog-layout">
      <aside class="catalog-filters">
        <FilterGroup label="Search">
            <SearchInput bind:value={query} placeholder="Account, factor, city…" />
        </FilterGroup>

        <FilterGroup label="Risk bucket">
            <FilterButton active={bucket === 'all'} onclick={() => (bucket = 'all')}>
              All ({rows.length})
            </FilterButton>
            <FilterButton active={bucket === 'high'} onclick={() => (bucket = 'high')}>
              High 50+ ({bucketCount('high')})
            </FilterButton>
            <FilterButton active={bucket === 'mid'} onclick={() => (bucket = 'mid')}>
              Mid 25–49 ({bucketCount('mid')})
            </FilterButton>
            <FilterButton active={bucket === 'low'} onclick={() => (bucket = 'low')}>
              Low 0–24 ({bucketCount('low')})
            </FilterButton>
        </FilterGroup>

        <FilterGroup label="Tier">
            <FilterButton active={tier === 'all'} onclick={() => (tier = 'all')}>
              All
            </FilterButton>
            <FilterButton active={tier === 'platinum'} onclick={() => (tier = 'platinum')}>
              Platinum
            </FilterButton>
            <FilterButton active={tier === 'gold'} onclick={() => (tier = 'gold')}>
              Gold
            </FilterButton>
            <FilterButton active={tier === 'silver'} onclick={() => (tier = 'silver')}>
              Silver
            </FilterButton>
        </FilterGroup>
      </aside>

      <section class="list-section">
        {#if sorted.length === 0}
          <p class="empty">No accounts match those filters.</p>
        {:else}
          <table class="data-table data-table-striped risk-table">
            <thead>
              <tr>
                <th style="cursor:pointer; user-select:none" onclick={() => setSort('name')}>
                  Account{arrowFor('name')}
                </th>
                <th
                  class="num"
                  style="cursor:pointer; user-select:none"
                  onclick={() => setSort('score')}
                >
                  Score{arrowFor('score')}
                </th>
                <th>Top factor</th>
                <th
                  class="num"
                  style="cursor:pointer; user-select:none"
                  onclick={() => setSort('days_since_last_invoice')}
                >
                  Days since invoice{arrowFor('days_since_last_invoice')}
                </th>
                <th
                  class="num"
                  style="cursor:pointer; user-select:none"
                  onclick={() => setSort('open_ticket_count')}
                >
                  Open SRs{arrowFor('open_ticket_count')}
                </th>
                <th>Contract</th>
                <th
                  class="num"
                  style="cursor:pointer; user-select:none"
                  onclick={() => setSort('days_since_last_note')}
                >
                  Days since contact{arrowFor('days_since_last_note')}
                </th>
              </tr>
            </thead>
            <tbody>
              {#each sorted as s (s.account_id)}
                <tr>
                  <td>
                    <EntityLink
                      kind="account"
                      id={s.account_id}
                      label={s.account_name}
                      mono={false}
                    />
                  </td>
                  <td class="num">
                    <span class="risk-chip risk-chip-{scoreTone(s.score)}">{s.score}</span>
                  </td>
                  <td>{s.top_factor}</td>
                  <td class="num">{formatDays(s.factors.days_since_last_invoice)}</td>
                  <td class="num">{s.factors.open_ticket_count}</td>
                  <td>
                    {#if s.factors.has_active_contract}
                      <span class="chip chip-active">active</span>
                    {:else}
                      <span class="chip chip-muted">none</span>
                    {/if}
                  </td>
                  <td class="num">{formatDays(s.factors.days_since_last_note)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </section>
    </div>
  </div>
{/if}
