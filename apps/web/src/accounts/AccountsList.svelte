<script lang="ts">
  // Port of apps/web/src/accounts/AccountsList.tsx.
  //
  // Fetches accounts + devices + service jobs (scoped to the /jobs
  // field-service filter) and renders a filterable table with
  // per-row device + open-ticket aggregates.
  //
  // Territory scoping for sales-rep/sales-mgr is kept in spirit but
  // simplified: no canSeeWorkOf() port yet — the full scoping helper
  // is deferred to the session rewrite tracked in phase-1 trade-ins.

  import { navigate, href } from '../router';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import { formatMoney } from '@boss/web-kit/ui/money';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import FilterGroup from '@boss/web-kit/ui/FilterGroup.svelte';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import SearchInput from '@boss/web-kit/ui/SearchInput.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import OverflowBanner from '@boss/web-kit/ui/OverflowBanner.svelte';
  import TierChip from './TierChip.svelte';
  import type { Asset, Job, Account } from './types';
  import type { Invoice } from '../finance/types';
  import { fetchPaged, isCapped, type Paged } from '../data/paginated';
  import { moduleEnabled } from '@boss/web-kit/session/manifest.svelte';

  type Tier = Account['tier'] | 'all';

  let accounts = $state<Account[]>([]);
  let devicesPage = $state<Paged<Asset> | null>(null);
  let jobsPage = $state<Paged<Job> | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  let tier = $state<Tier>('all');
  let stateFilter = $state<string>('all');
  let query = $state('');

  // Per-account device counts and open-ticket counts are computed
  // client-side from these fetches. When the support module is
  // disabled the field-service queue does not exist, so the jobs
  // fetch is skipped entirely (no point spending the round-trip on
  // a guaranteed-empty result).
  const supportOn = $derived(moduleEnabled('support'));

  let invoicesPage = $state<Paged<Invoice> | null>(null);

  let devices = $derived(devicesPage?.data ?? []);
  let jobs = $derived(jobsPage?.data ?? []);
  let invoices = $derived(invoicesPage?.data ?? []);

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const includeJobs = supportOn;
        const [pResp, dPaged, jPaged, iPaged] = await Promise.all([
          fetch('/api/people/accounts'),
          // `/api/assets` — `/api/assets/systems` was the fleet-era
          // path; it has no route and fell through to
          // `/api/assets/{asset_id}` → 404, so this list rendered
          // empty since the rename.
          fetchPaged<Asset>('/api/assets?limit=1000'),
          includeJobs
            ? fetchPaged<Job>('/api/jobs?kind=field-service&limit=5000')
            : Promise.resolve(null),
          // Open AR — pull invoices, filter client-side to unpaid.
          // Bounded at 10k; the OverflowBanner below surfaces
          // truncation if a tenant blows past it.
          fetchPaged<Invoice>('/api/commerce/invoices?limit=10000'),
        ]);
        if (!pResp.ok) throw new Error(`accounts HTTP ${pResp.status}`);
        const pBody = await pResp.json();
        if (!cancelled) {
          accounts = Array.isArray(pBody) ? pBody : (pBody.data ?? []);
          // Device/ticket/AR columns are secondary joins — a failed
          // side-load degrades those columns, it does not fail the
          // account list itself.
          devicesPage = dPaged.kind === 'ready' ? dPaged.page : null;
          jobsPage = jPaged && jPaged.kind === 'ready' ? jPaged.page : null;
          invoicesPage = iPaged.kind === 'ready' ? iPaged.page : null;
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

  let rows = $derived(
    accounts.map((c) => {
      const accountDevices = devices.filter((d) => d.account_id === c.id);
      const openTickets = jobs.filter(
        (j) =>
          j.subject.subject_kind === 'account' &&
          j.subject.id === c.id &&
          j.status !== 'closed' &&
          j.status !== 'cancelled',
      ).length;
      const openArCents = invoices
        .filter((i) => i.account_id === c.id && i.paid_on == null)
        .reduce((sum, i) => sum + (i.amount_cents ?? 0), 0);
      return {
        account: c,
        deviceCount: accountDevices.length,
        activeDevices: accountDevices.filter((d) => d.phase === 'installed').length,
        openTickets,
        openArCents,
      };
    }),
  );

  let states = $derived(
    [...new Set(accounts.map((c) => c.state).filter((s): s is string => s !== null))].sort(),
  );

  let visible = $derived(
    rows.filter((r) => {
      if (tier !== 'all' && r.account.tier !== tier) return false;
      if (stateFilter !== 'all' && r.account.state !== stateFilter) return false;
      if (query) {
        const q = query.toLowerCase();
        const hay = `${r.account.name ?? ''} ${r.account.director ?? ''} ${r.account.city ?? ''}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    }),
  );

  // Tenant-shaping: hide columns whose values are zero for every
  // row. The brewery has equipment assets, but none of them are
  // attached to a wholesale account (devices are owned by the
  // brewery itself) — so a "devices exist in assets" check would
  // render an Equipment column full of zeros. Check per-row counts
  // instead. Same applies to Open SRs.
  let hasAnyDevices = $derived(
    rows.some((r) => r.deviceCount > 0),
  );
  let hasAnyServiceJobs = $derived(
    supportOn && rows.some((r) => r.openTickets > 0),
  );
  let hasAnyOpenAr = $derived(
    rows.some((r) => r.openArCents > 0),
  );
  let subtitleLine = $derived(
    [
      hasAnyDevices
        ? `${(devicesPage?.total ?? 0).toLocaleString()} installed devices`
        : null,
      hasAnyServiceJobs
        ? `${(jobsPage?.total ?? 0).toLocaleString()} service jobs`
        : null,
    ]
      .filter((s): s is string => s !== null)
      .join(' · ') || undefined,
  );

  const TIERS: ReadonlyArray<'platinum' | 'gold' | 'silver'> = [
    'platinum', 'gold', 'silver',
  ];

  function cap(s: string): string {
    return s ? s[0]!.toUpperCase() + s.slice(1) : s;
  }
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Customers"
    title={`${accounts.length} accounts`}
    subtitle={subtitleLine}
    motif="tap"
  />

  {#if isCapped(devicesPage)}
    <OverflowBanner
      showing={devices.length}
      total={devicesPage!.total}
      noun="installed devices loaded for per-account rollups"
      hint="Per-row device counts may undercount; raise the cap or filter."
    />
  {/if}
  {#if isCapped(jobsPage)}
    <OverflowBanner
      showing={jobs.length}
      total={jobsPage!.total}
      noun="service jobs loaded for per-account rollups"
      hint="Per-row open-ticket counts may undercount; raise the cap or filter."
    />
  {/if}

  <div class="catalog-layout">
    <aside class="catalog-filters">
      <FilterGroup label="Search">
          <SearchInput bind:value={query} placeholder="Account, doctor, city…" />
      </FilterGroup>

      <FilterGroup label="Tier">
          <FilterButton active={tier === 'all'} onclick={() => (tier = 'all')}>
            All ({rows.length})
          </FilterButton>
          {#each TIERS as t (t)}
            <FilterButton active={tier === t} onclick={() => (tier = t)}>
                {cap(t)} ({rows.filter((r) => r.account.tier === t).length})
            </FilterButton>
          {/each}
      </FilterGroup>

      <FilterGroup label="State">
          <FilterButton active={stateFilter === 'all'} onclick={() => (stateFilter = 'all')}>
            All states
          </FilterButton>
          {#each states as s (s)}
            <FilterButton active={stateFilter === s} onclick={() => (stateFilter = s)}>
                {s} ({rows.filter((r) => r.account.state === s).length})
            </FilterButton>
          {/each}
      </FilterGroup>
    </aside>

    <section class="list-section">
      {#if loading}
        <p class="empty">Loading…</p>
      {:else if error}
        <p class="empty">Couldn't load accounts: {error}</p>
      {:else if visible.length === 0}
        <p class="empty">No accounts match those filters.</p>
      {:else}
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>Account</th>
              <th>Tier</th>
              <th>Location</th>
              <th>Primary contact</th>
              {#if hasAnyDevices}<th class="num">Equipment</th>{/if}
              {#if hasAnyServiceJobs}<th class="num">Open SRs</th>{/if}
              {#if hasAnyOpenAr}<th class="num">Open AR</th>{/if}
              <th>Customer since</th>
            </tr>
          </thead>
          <tbody>
            {#each visible as r (r.account.id)}
              {@const to = entityHref('account', r.account.id)}
              <tr
                class="data-table-row-link"
                onclick={() => navigate(to)}
              >
                <td>
                  <strong>
                    <EntityLink
                      kind="account"
                      id={r.account.id}
                      label={r.account.name}
                      mono={false}
                    />
                  </strong>
                </td>
                <td><TierChip tier={r.account.tier} /></td>
                <td>{r.account.city ?? '—'}, {r.account.state ?? '—'}</td>
                <td class="prose-cell">{r.account.director ?? '—'}</td>
                {#if hasAnyDevices}<td class="num">{r.deviceCount}</td>{/if}
                {#if hasAnyServiceJobs}
                  <td class="num">
                    {#if r.openTickets > 0}
                      <strong>{r.openTickets}</strong>
                    {:else}
                      0
                    {/if}
                  </td>
                {/if}
                {#if hasAnyOpenAr}
                  <td class="num">
                    {#if r.openArCents > 0}
                      <strong>{formatMoney({ amount_cents: r.openArCents, currency: 'USD' }, { precision: 'whole' })}</strong>
                    {:else}
                      —
                    {/if}
                  </td>
                {/if}
                <td>{r.account.customer_since ?? '—'}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  </div>
</div>
