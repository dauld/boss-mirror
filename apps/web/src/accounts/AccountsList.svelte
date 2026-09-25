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
  import { rowLink } from '@boss/web-kit/ui/RowLink';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import { formatMoney } from '@boss/web-kit/ui/money';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import FilterGroup from '@boss/web-kit/ui/FilterGroup.svelte';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import SearchInput from '@boss/web-kit/ui/SearchInput.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import OverflowBanner from '@boss/web-kit/ui/OverflowBanner.svelte';
  import TierChip from './TierChip.svelte';
  import type { Asset, Job, Account, AccountOpenAr } from './types';
  import { fetchPaged, isCapped, type Paged } from '../data/paginated';
  import { fetchAccountsPage } from './api';
  import { moduleEnabled } from '@boss/web-kit/session/manifest.svelte';
  import { okRead, readStateOf, type ReadState } from '../data/readState';
  import { loadClasses, classesFor } from '@boss/web-kit/session/classes.svelte';
  import { tierAdmits, tierBuckets, type TierFilter } from './tiers';

  let accounts = $state<Account[]>([]);
  let accountsPage = $state<Paged<Account> | null>(null);
  let devicesPage = $state<Paged<Asset> | null>(null);
  let jobsPage = $state<Paged<Job> | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  let tier = $state<TierFilter>({ kind: 'all' });
  let stateFilter = $state<string>('all');
  let query = $state('');

  // Per-account device counts and open-ticket counts are computed
  // client-side from these fetches. When the support module is
  // disabled the department has no surface here, so the jobs fetch is
  // skipped entirely (no point spending the round-trip on a
  // guaranteed-empty result).
  //
  // The ticket read asks for the SUPPORT DEPARTMENT's packets. It
  // asked for `kind=field-service` until 2026-09-22 (backlog
  // 423a531d) — a workflow one tenant's seed bundle authors, which
  // this instance does not publish — so the Tickets column was 0 for every account. Being a
  // contributing read inside a Promise.all rather than the page's own
  // list, its emptiness folded into a wider view and nothing looked
  // wrong, which is why it outlived the page that shared the defect.
  const supportOn = $derived(moduleEnabled('support'));

  let openArPage = $state<Paged<AccountOpenAr> | null>(null);
  // What each secondary read did. Their `failed` arms were dropped on
  // the floor (`dPaged.kind === 'ready' ? dPaged.page : null`), and the
  // tenant-shaping below hides an all-zero column — so a failed read
  // removed its column without a word, the paint of "no account has
  // any" (backlogs 223ebcd6 and e30ee8b9). The column still goes, since
  // there is nothing true to put in it, but the page now says why.
  let devicesRead = $state<ReadState>(okRead);
  let jobsRead = $state<ReadState>(okRead);
  let openArRead = $state<ReadState>(okRead);

  let devices = $derived(devicesPage?.data ?? []);
  let jobs = $derived(jobsPage?.data ?? []);
  let openArByAccount = $derived(
    new Map((openArPage?.data ?? []).map((r) => [r.account_id, r.open_ar_cents])),
  );

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const includeJobs = supportOn;
        const [pPaged, dPaged, jPaged, arPaged] = await Promise.all([
          // The directory itself is enveloped since backlog 2d1d298e
          // (2026-09-23) — it was an unbounded bare array, so this list
          // could not say when it was incomplete.
          fetchAccountsPage(),
          // `/api/assets` — `/api/assets/systems` was the fleet-era
          // path; it has no route and fell through to
          // `/api/assets/{asset_id}` → 404, so this list rendered
          // empty since the rename.
          fetchPaged<Asset>('/api/assets?limit=1000'),
          includeJobs
            ? fetchPaged<Job>('/api/jobs?department=support&limit=5000')
            : Promise.resolve(null),
          // Open AR — the service's per-account sum over every invoice
          // still owed. This read was `/api/commerce/invoices?limit=10000`
          // summed here, and its comment promised an OverflowBanner that
          // never existed; the service clamps that list to 1,000 rows,
          // so past a thousand invoices the money figure was short in
          // silence (backlog 5257bfa9). An aggregate is one row per
          // owing account and is never truncated.
          fetchPaged<AccountOpenAr>('/api/commerce/open-ar'),
        ]);
        if (pPaged.kind === 'failed') throw new Error(pPaged.error);
        if (!cancelled) {
          accountsPage = pPaged.page;
          accounts = [...pPaged.page.data];
          // Device/ticket/AR columns are secondary joins — a failed
          // side-load degrades those columns, it does not fail the
          // account list itself.
          devicesPage = dPaged.kind === 'ready' ? dPaged.page : null;
          jobsPage = jPaged && jPaged.kind === 'ready' ? jPaged.page : null;
          openArPage = arPaged.kind === 'ready' ? arPaged.page : null;
          devicesRead = readStateOf(dPaged);
          jobsRead = jPaged ? readStateOf(jPaged) : okRead;
          openArRead = readStateOf(arPaged);
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
      const openArCents = openArByAccount.get(c.id) ?? 0;
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
      if (!tierAdmits(tier, r.account.tier)) return false;
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

  // Each failed secondary read, with the column it takes away.
  let failedColumns = $derived(
    [
      { what: 'installed devices', read: devicesRead, column: 'Equipment' },
      { what: 'service jobs', read: jobsRead, column: 'Open SRs' },
      { what: 'open receivables', read: openArRead, column: 'Open AR' },
    ].flatMap((f) => (f.read.kind === 'failed' ? [{ ...f, error: f.read.error }] : [])),
  );

  // The Tier buttons come from the (account, tier) Classes, plus No
  // tier when an account has none — the watchlist's tiers.ts, reused
  // (backlog d2c9e79f, after 1be37454 fixed the same trio there). A
  // hand-written Platinum / Gold / Silver hid any tier a tenant added
  // by one Class row, and left the untiered sponsor reachable only
  // under All. Counted over every row, as the other filters are.
  $effect(() => {
    void loadClasses('account');
  });
  let tierButtons = $derived(
    tierBuckets(
      rows.map((r) => r.account.tier),
      classesFor('account', 'tier'),
    ),
  );
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Customers"
    title={error ? 'Accounts' : `${accounts.length} accounts`}
    subtitle={error
      ? // "0 accounts" above the failure line read as an empty book
        // (sweep c3e4edcc). The count is unknown, so say so.
        'Account count unknown — the read failed'
      : subtitleLine}
  />

  {#if isCapped(accountsPage)}
    <OverflowBanner
      showing={accounts.length}
      total={accountsPage!.total}
      noun="accounts"
      hint="The list and its filters cover only the accounts loaded."
    />
  {/if}
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
          <FilterButton active={tier.kind === 'all'} onclick={() => (tier = { kind: 'all' })}>
            All ({rows.length})
          </FilterButton>
          {#each tierButtons as b (b.code ?? '')}
            <FilterButton
              active={tier.kind === 'code' && tier.code === b.code}
              onclick={() => (tier = { kind: 'code', code: b.code })}
            >
                {b.label} ({b.count})
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
      {#if !loading && !error}
        {#each failedColumns as f (f.what)}
          <p class="empty load-failed" role="alert">
            Couldn't load {f.what} — {f.error}. The {f.column} column is not shown: its counts are unknown, not zero.
          </p>
        {/each}
      {/if}
      {#if loading}
        <p class="empty">Loading…</p>
      {:else if error}
        <p class="empty load-failed" role="alert">Couldn't load accounts: {error}</p>
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
              <tr use:rowLink={{ onActivate: () => navigate(to), label: `Account ${r.account.name}` }}>
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
