<script lang="ts">
  // Full churn watchlist — port of apps/web/src/accounts/WatchlistPage.tsx.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { RiskScoreListSchema } from './schemas';
  import FilterGroup from '@boss/web-kit/ui/FilterGroup.svelte';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import SearchInput from '@boss/web-kit/ui/SearchInput.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import OverflowBanner from '@boss/web-kit/ui/OverflowBanner.svelte';
  import SortHeader from '@boss/web-kit/ui/SortHeader.svelte';
  import { createSortState } from '@boss/web-kit/ui/sort-state.svelte';
  import type { Account } from './types';
  import { fetchAccountsPage } from './api';
  import { isCapped, type Paged } from '../data/paginated';
  import { loadingRead, readStateOf, type ReadState } from '../data/readState';
  import { loadClasses, classesFor } from '@boss/web-kit/session/classes.svelte';
  import { tierAdmits, tierBuckets, type TierFilter } from './tiers';

  type RiskFactors = {
    days_since_last_invoice: number | null;
    open_ticket_count: number;
    has_active_contract: boolean;
    days_since_last_note: number | null;
  };

  type RiskScore = {
    account_id: string;
    account_name: string;
    score: number;
    top_factor: string;
    factors: RiskFactors;
  };

  type SortKey =
    | 'score'
    | 'name'
    | 'days_since_last_invoice'
    | 'open_ticket_count'
    | 'days_since_last_note';
  type Bucket = 'all' | 'high' | 'mid' | 'low';

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'denied' }
    | { kind: 'ready'; scores: ReadonlyArray<RiskScore> };

  let loadState: LoadState = $state<LoadState>({ kind: 'loading' });
  let accounts = $state<Account[]>([]);
  // The accounts directory is the ONLY source of each row's tier and
  // city, so its outcome is kept beside the rows (backlog 3122f14a;
  // page audit 08b0c4f8 GAP 7, 2026-09-23). It was read as "names
  // only" and a failure was dropped: every tier then read unknown, any
  // Tier button but All emptied the table under "No accounts match
  // those filters.", and a city search missed — all without a word.
  let directoryRead = $state<ReadState>(loadingRead);
  let directoryPage = $state<Paged<Account> | null>(null);

  let query = $state('');
  let tier = $state<TierFilter>({ kind: 'all' });
  let bucket = $state<Bucket>('all');
  // The shared sort (libs/web-kit sort.ts) was extracted FROM this
  // page's hand-rolled sortKey / sortDir / setSort / arrowFor and never
  // adopted back, so its headers stayed `<th onclick>` with no tabindex
  // or key handling and a keyboard could not sort (backlog 8c5664ea;
  // page audit 08b0c4f8 GAP 13). SortHeader carries tabindex,
  // Enter/Space and aria-sort; the order is unchanged — a name opens
  // A to Z, a number largest first, and a null sorts below every value.
  const sort = createSortState<SortKey>({ key: 'score', dir: 'desc' }, (k) =>
    k === 'name' ? 'asc' : 'desc',
  );

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [rResp, pPaged] = await Promise.all([
          fetch('/api/people/accounts/risk-scores?limit=200&min_score=0'),
          fetchAccountsPage(),
        ]);
        // A REFUSAL IS NOT AN EMPTY WATCHLIST. The server answers a
        // role without broad account access 403 (it used to answer
        // `200 {accounts: []}`, painted as "No accounts match those
        // filters." — backlog 3f0cdca8, page audit 08b0c4f8 GAP 5), so
        // the page says it may not show this rather than nothing at risk.
        if (rResp.status === 403) {
          if (!cancelled) loadState = { kind: 'denied' };
          return;
        }
        if (!rResp.ok) throw new Error(`${rResp.status}`);
        // PARSE, DO NOT CAST. This read `(await rResp.json()) as {
        // accounts: RiskScore[] }`, which the compiler trusts and the
        // runtime does not: a payload without `accounts` made `scores`
        // undefined and `rows.length` threw "Cannot read properties of
        // undefined (reading 'length')" (feedback 2fe1c8c1). That is the
        // exact failure class data/parseResponse.ts was written for, and
        // this was one of its unconverted call sites.
        //
        // A WRONG SHAPE IS AN ERROR, NOT AN EMPTY LIST. Falling back to
        // `[]` would render "0 accounts scored" over a broken backend —
        // a confident, wrong answer, which is worse than saying so.
        const parsed = RiskScoreListSchema.safeParse(await rResp.json());
        if (!parsed.success) throw new Error('unexpected risk-score payload');
        // NO CAST. `as RiskScore[]` let an optional `factors` in the
        // schema meet an unguarded `s.factors.*` below and compile
        // (backlog 4b981df2); without it, the schema must produce the
        // RiskScore this page reads, or svelte-check refuses.
        if (!cancelled) loadState = { kind: 'ready', scores: parsed.data.accounts };
        // A failed directory read does not fail the page — the scores
        // are its own read and they answered — but it is SAID, and the
        // filters that need it stand down (below). A capped one is said
        // too: tier and city are known only for the accounts it held.
        if (!cancelled) {
          directoryRead = readStateOf(pPaged);
          if (pPaged.kind === 'ready') {
            accounts = [...pPaged.page.data];
            directoryPage = pPaged.page;
          }
        }
      } catch (e) {
        if (!cancelled) loadState = { kind: 'error', message: String(e) };
      }
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
  function formatDays(d: number | null): string {
    return d === null ? '—' : `${d}d`;
  }

  let rows = $derived(
    loadState.kind === 'ready' ? loadState.scores : [],
  );

  let filtered = $derived(
    rows.filter((r) => {
      const account = accountById.get(r.account_id);
      if (!tierAdmits(tier, account ? account.tier : undefined)) return false;
      if (bucket !== 'all' && scoreTone(r.score) !== bucket) return false;
      if (query) {
        const q = query.toLowerCase();
        const hay = `${r.account_name} ${r.top_factor} ${account?.city ?? ''}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    }),
  );

  let sorted = $derived(
    sort.sorted(filtered, {
      name: (r) => r.account_name,
      score: (r) => r.score,
      days_since_last_invoice: (r) => r.factors.days_since_last_invoice,
      open_ticket_count: (r) => r.factors.open_ticket_count,
      days_since_last_note: (r) => r.factors.days_since_last_note,
    }),
  );

  // The Tier buttons come from the (account, tier) Classes, plus No
  // tier when an account has none (backlog 1be37454; page audit
  // 08b0c4f8 GAP 11). A hand-written Platinum / Gold / Silver trio hid
  // any tier a tenant added, and left the untiered account reachable
  // only under All. Counted over the scored rows whose account the
  // directory returned — a row it did not return has an unknown tier,
  // not an absent one, so it counts under All alone.
  $effect(() => {
    void loadClasses('account');
  });
  let tierButtons = $derived(
    tierBuckets(
      rows.flatMap((r) => {
        const a = accountById.get(r.account_id);
        return a ? [a.tier] : [];
      }),
      classesFor('account', 'tier'),
    ),
  );

  // With the directory dark every tier is unknown, so a Tier button
  // could only empty the table and the city is not there to search:
  // the filter keeps All alone and the search stops offering the city.
  let directoryFailed = $derived(directoryRead.kind === 'failed');

  function bucketCount(b: Exclude<Bucket, 'all'>): number {
    return rows.filter((r) => scoreTone(r.score) === b).length;
  }

</script>

{#if loadState.kind === 'loading'}
  <div class="catalog theme-exec">
    <PageHeader eyebrow="Churn watchlist" title="Loading…" />
  </div>
{:else if loadState.kind === 'error'}
  <div class="catalog theme-exec">
    <PageHeader eyebrow="Churn watchlist" title="Couldn't load watchlist" />
    <!-- The shared failure marker (sweep c3e4edcc, backlog 94d5d2d2). -->
    <p class="empty load-failed" role="alert">{loadState.message}</p>
  </div>
{:else if loadState.kind === 'denied'}
  <div class="catalog theme-exec">
    <PageHeader eyebrow="Churn watchlist" title="Not shown to your role" />
    <p class="empty">
      The churn watchlist carries financial and churn signals for every account, so it is shown
      only to roles with broad account access. This says nothing about whether any account is at
      risk.
    </p>
  </div>
{:else}
  <div class="catalog theme-exec">
    <PageHeader
      eyebrow="Customers"
      title="Churn watchlist"
      subtitle={`${rows.length} accounts scored · ${filtered.length} shown`}
    />

    {#if isCapped(directoryPage)}
      <OverflowBanner
        showing={accounts.length}
        total={directoryPage!.total}
        noun="accounts in the directory"
        hint="Tier and city are known only for those: a scored account past them shows under All alone, and its city is not searched."
      />
    {/if}

    <div class="catalog-layout">
      <aside class="catalog-filters">
        <FilterGroup label="Search">
            <SearchInput
              bind:value={query}
              placeholder={directoryFailed ? 'Account, factor…' : 'Account, factor, city…'}
            />
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
            <FilterButton active={tier.kind === 'all'} onclick={() => (tier = { kind: 'all' })}>
              All
            </FilterButton>
            {#if directoryFailed}
              <p class="empty">Tiers unknown — the accounts directory did not load.</p>
            {:else}
              {#each tierButtons as b (b.code ?? '')}
                <FilterButton
                  active={tier.kind === 'code' && tier.code === b.code}
                  onclick={() => (tier = { kind: 'code', code: b.code })}
                >
                  {b.label} ({b.count})
                </FilterButton>
              {/each}
            {/if}
        </FilterGroup>
      </aside>

      <section class="list-section">
        {#if directoryRead.kind === 'failed'}
          <p class="empty load-failed" role="alert">
            Couldn't load the accounts directory — {directoryRead.error}. Tier and city are unknown:
            the Tier filter shows All alone and search matches account and factor only.
          </p>
        {/if}
        {#if sorted.length === 0}
          <p class="empty">No accounts match those filters.</p>
        {:else}
          <table class="data-table data-table-striped risk-table">
            <thead>
              <tr>
                <SortHeader {sort} key="name">Account</SortHeader>
                <SortHeader {sort} key="score" num={true}>Score</SortHeader>
                <th>Top factor</th>
                <SortHeader {sort} key="days_since_last_invoice" num={true}>
                  Days since invoice
                </SortHeader>
                <SortHeader {sort} key="open_ticket_count" num={true}>Open SRs</SortHeader>
                <th>Contract</th>
                <SortHeader {sort} key="days_since_last_note" num={true}>
                  Days since contact
                </SortHeader>
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
