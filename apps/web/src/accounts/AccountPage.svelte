<script lang="ts">
  // Unified Account Detail View.
  //
  // Surfaces the five load-bearing questions above the fold:
  // activity, next-best-actions, at-a-glance, SLA stats, owners.
  // Deep-dive tabs ride below.
  //
  // Port of apps/web/src/accounts/AccountPage.tsx. Sub-panels
  // (ActivityTimeline, AccountTeamPanel, NotesPanel) live in sibling
  // components; everything else stays inline.

  import { onMount } from 'svelte';
  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { appNow } from '@boss/web-kit/sim-clock';
  import { formatMoney } from '@boss/web-kit/ui/money';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import FileAttachments from '../content/FileAttachments.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import TierChip from './TierChip.svelte';
  import ActivityTimeline from './ActivityTimeline.svelte';
  import KnowledgeBaseView from '../kb/KnowledgeBaseView.svelte';
  import AccountTeamPanel from './AccountTeamPanel.svelte';
  import NotesPanel from './NotesPanel.svelte';
  import { loadAccountBundle, loadContracts, AccountSchemaError } from './api';
  import type { AccountBundle, NextAction } from './types';
  import { shortId } from '../data/ids';
  import { href } from '../router';

  let { accountId } = $props<{ accountId: string }>();

  type BundleState =
    | { kind: 'loading' }
    | { kind: 'not-found' }
    | { kind: 'schema-error'; reason: string }
    | { kind: 'ready'; bundle: AccountBundle };

  let bundleState: BundleState = $state<BundleState>({ kind: 'loading' });
  let tab = $state<TabKey>('overview');

  $effect(() => {
    const pid = accountId;
    let cancelled = false;
    bundleState = { kind: 'loading' };
    (async () => {
      try {
        const result = await loadAccountBundle(pid);
        if (cancelled) return;
        if (result === 'not-found') {
          bundleState = { kind: 'not-found' };
        } else {
          bundleState = { kind: 'ready', bundle: result };
        }
      } catch (e) {
        if (cancelled) return;
        if (e instanceof AccountSchemaError) {
          bundleState = { kind: 'schema-error', reason: e.message };
        } else {
          // Network errors etc. already get swallowed inside the
          // bundle loader's fetchPaged null-fallback, so this branch
          // primarily catches unexpected runtime exceptions.
          bundleState = {
            kind: 'schema-error',
            reason: e instanceof Error ? e.message : String(e),
          };
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  // -----------------------------------------------------------
  // Tabs
  // -----------------------------------------------------------

  type TabKey = 'overview' | 'devices' | 'tickets' | 'finance' | 'shipments' | 'knowledge';

  // Tab ids are stable; labels stay tenant-neutral. The
  // `Jobs` tab spans every Workflow on the account (not just
  // service tickets), so it renders for any tenant that runs
  // Jobs at all. Industry-shaped tabs (Equipment, Deliveries)
  // get gated on data presence below — see `visibleTabs`.
  const ALL_TABS: ReadonlyArray<{ id: TabKey; label: string }> = [
    { id: 'overview', label: 'Overview' },
    { id: 'devices', label: 'Equipment' },
    { id: 'tickets', label: 'Jobs' },
    { id: 'finance', label: 'Finance' },
    { id: 'shipments', label: 'Deliveries' },
    { id: 'knowledge', label: 'Knowledge' },
  ];

  // -----------------------------------------------------------
  // Contracts — small side fetch for the summary panel.
  // -----------------------------------------------------------

  let contracts = $state<ReadonlyArray<{ id: string; end_date: string }>>([]);
  let contractsLoaded = $state(false);

  $effect(() => {
    const pid = accountId;
    let cancelled = false;
    contractsLoaded = false;
    (async () => {
      const rows = await loadContracts(pid);
      if (!cancelled) {
        contracts = rows;
        contractsLoaded = true;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  // -----------------------------------------------------------
  // Helpers
  // -----------------------------------------------------------

  function iconFor(sev: NextAction['severity']): string {
    if (sev === 'critical') return '🚨';
    if (sev === 'warning') return '⚡';
    return '→';
  }

  function glance(cents: number): string {
    return formatMoney({ amount_cents: cents, currency: 'USD' }, { precision: 'compact' });
  }

  function dollars(cents: number): string {
    return formatMoney({ amount_cents: cents, currency: 'USD' });
  }
</script>

{#if bundleState.kind === 'loading'}
  <div class="catalog theme-exec">
    <p class="empty">Loading account…</p>
  </div>
{:else if bundleState.kind === 'not-found'}
  <div class="catalog theme-exec">
    <PageHeader eyebrow="Account" title="Not found" subtitle={accountId} />
  </div>
{:else if bundleState.kind === 'schema-error'}
  <div class="catalog theme-exec">
    <PageHeader
      eyebrow="Account"
      title="Server returned an unexpected payload shape"
      subtitle={accountId}
    />
    <p class="empty">
      One of the account bundle's sub-endpoints returned a body
      that didn't match the expected schema. Details:
    </p>
    <pre class="empty">{bundleState.reason}</pre>
  </div>
{:else}
  {@const b = bundleState.bundle}
  {@const account = b.account}
  {@const devices = b.devices}
  {@const invoices = b.invoices}
  {@const jobs = b.jobs}
  {@const shipments = b.shipments}
  {@const activeDevices = devices.filter((d) => d.phase !== 'decommissioned').length}
  {@const inRefurb = devices.filter((d) => ['received', 'triaging', 'refurbing', 'qa'].includes(d.phase)).length}
  {@const openTickets = devices.reduce((s, d) => s + d.open_ticket_count, 0)}
  <!-- Tenant-aware tab filter — drop industry-shaped tabs
       when the account has no data of that shape. Brewery
       accounts never have devices/shipments; UDS accounts
       always do. Keeps the tab strip honest instead of
       showing an empty deep-dive panel. -->
  {@const visibleTabs = ALL_TABS.filter((t) => {
    if (t.id === 'devices') return devices.length > 0;
    if (t.id === 'shipments') return shipments.length > 0;
    return true;
  })}
  {@const ytdStart = `${appNow().getFullYear()}-01-01`}
  {@const ytdRevCents = invoices
    .filter((i) => i.status === 'paid' && (i.paid_on ?? '') >= ytdStart)
    .reduce((s, i) => s + i.amount_cents, 0)}
  {@const pastDueCents = invoices.filter((i) => i.status === 'past-due').reduce((s, i) => s + i.amount_cents, 0)}
  {@const openJobs = jobs.filter((j) => j.status !== 'closed' && j.status !== 'cancelled').length}
  {@const bySku = (() => {
    const m = new Map<string, number>();
    for (const d of devices) {
      if (d.phase === 'decommissioned') continue;
      m.set(d.sku, (m.get(d.sku) ?? 0) + 1);
    }
    return [...m.entries()].sort((a, b) => b[1] - a[1]);
  })()}

  <div class="account-page theme-exec">
    <Breadcrumb to={href('/ux/accounts')}>
      ← All accounts
    </Breadcrumb>
    <PageHeader
      eyebrow={`Account · ${account.tier ?? 'untiered'}`}
      title={account.name ?? account.id}
      subtitle={`${account.director ?? '—'} · ${account.city ?? '—'}, ${account.state ?? '—'}${account.customer_since ? ` · Customer since ${account.customer_since.slice(0, 7)}` : ''}`}
    />

    <div class="subject-actions">
      <a
        class="action-btn"
        href={href(`/ux/jobs?new=1&subject_kind=account&subject_id=${encodeURIComponent(account.id)}`)}
      >
        + Create a Job for this account
      </a>
    </div>

    {#if b.caps.devices.capped || b.caps.invoices.capped || b.caps.jobs.capped || b.caps.shipments.capped}
      <div class="account-cap-note" role="status">
        Some cross-entity rollups on this page are computed from a capped sample. The actual totals are:
        {#if b.caps.devices.capped}
          <strong>{b.caps.devices.total.toLocaleString()}</strong> devices (showing {devices.length.toLocaleString()}){' · '}
        {/if}
        {#if b.caps.invoices.capped}
          <strong>{b.caps.invoices.total.toLocaleString()}</strong> invoices (showing {invoices.length.toLocaleString()}){' · '}
        {/if}
        {#if b.caps.jobs.capped}
          <strong>{b.caps.jobs.total.toLocaleString()}</strong> jobs (showing {jobs.length.toLocaleString()}){' · '}
        {/if}
        {#if b.caps.shipments.capped}
          <strong>{b.caps.shipments.total.toLocaleString()}</strong> shipments (showing {shipments.length.toLocaleString()})
        {/if}
      </div>
    {/if}

    <!-- At-a-glance strip — 3 always-on stats (jobs, paid,
         past-due) plus equipment + open cases when devices exist
         on the account. Brewery accounts have no devices →
         3-stat strip; used-device-shop accounts → 5-stat strip.
         Same code, no per-tenant gate. -->
    <div class="pp-glance">
      {#if devices.length > 0}
        <div class="pp-glance-stat">
          <div class="pp-glance-label">Equipment</div>
          <div class="pp-glance-value">{activeDevices}</div>
          {#if inRefurb > 0}
            <div class="pp-glance-sub">{inRefurb} in service</div>
          {/if}
        </div>
        <div class="pp-glance-stat">
          <div class="pp-glance-label">Open cases</div>
          <div class="pp-glance-value">{openTickets}</div>
        </div>
      {/if}
      <div class="pp-glance-stat">
        <div class="pp-glance-label">Open jobs</div>
        <div class="pp-glance-value">{openJobs}</div>
      </div>
      <div class="pp-glance-stat">
        <div class="pp-glance-label">Paid YTD</div>
        <div class="pp-glance-value">{glance(ytdRevCents)}</div>
      </div>
      <div class="pp-glance-stat {pastDueCents > 0 ? 'pp-glance-stat-warn' : 'pp-glance-stat-muted'}">
        <div class="pp-glance-label">Past-due</div>
        <div class="pp-glance-value">{glance(pastDueCents)}</div>
      </div>
    </div>

    <!-- Next-best-actions. -->
    <Section title="What you should do today" wide>
        {#if b.nextActions.length === 0}
          <p class="empty">Nothing needs attention right now.</p>
        {:else}
          <div class="pp-actions">
            {#each b.nextActions as a, i (i)}
              <div class="pp-action pp-action-{a.severity ?? 'info'}">
                <span class="pp-action-icon">{iconFor(a.severity)}</span>
                <span class="pp-action-title">{a.title}</span>
                {#if a.detail}<span class="pp-action-detail">{a.detail}</span>{/if}
                {#if a.link}
                  <Link to={a.link} className="pp-action-link">
                    →
                  </Link>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
    </Section>

    <!-- Two-column grid: timeline left, context right. -->
    <div class="pp-grid">
      <div class="pp-grid-main">
        <ActivityTimeline {accountId} />
      </div>
      <div class="pp-grid-aside">
        <!-- Devices summary — hidden when the account has no
             devices on it. Brewery accounts never do; used-
             device-shop accounts always do. -->
        {#if devices.length > 0}
          <Section title={`Equipment (${devices.length})`}>
              {#if bySku.length === 0}
                <p class="empty">No devices.</p>
              {:else}
                <ul class="pp-sku-list">
                  {#each bySku.slice(0, 6) as [sku, count] (sku)}
                    <li>
                      <Link to={href(`/ux/catalog/${sku}`)} className="mono">
                        {sku}
                      </Link>
                      <span class="pp-sku-count">{count}</span>
                    </li>
                  {/each}
                </ul>
              {/if}
          </Section>
        {/if}

        <!-- Contracts summary -->
        <Section title={`Active contracts (${contracts.length})`}>
            {#if !contractsLoaded}
              <p class="empty">Loading…</p>
            {:else if contracts.length === 0}
              <p class="empty">No active agreements.</p>
            {:else}
              <ul class="pp-contract-list">
                {#each contracts.slice(0, 5) as c (c.id)}
                  <li>
                    <EntityLink kind="agreement" id={c.id} />
                    <span class="pp-contract-end">ends {c.end_date}</span>
                  </li>
                {/each}
              </ul>
            {/if}
        </Section>

        <AccountTeamPanel {accountId} team={b.team} />
        <NotesPanel {accountId} notes={b.notes} />

        <Section title="Attachments">
          <FileAttachments targetKind="subject" targetId={accountId} />
        </Section>
      </div>
    </div>

    <!-- Deep-dive tabs. -->
    <div style="margin-top: 32px">
      <div class="pp-tabs" role="tablist">
        {#each visibleTabs as t (t.id)}
          <button
            type="button"
            role="tab"
            aria-selected={tab === t.id}
            class="pp-tab {tab === t.id ? 'pp-tab-active' : ''}"
            onclick={() => (tab = t.id)}
          >
            {t.label}
          </button>
        {/each}
      </div>
      <div class="pp-tab-panel">
        {#if tab === 'overview'}
          <div class="tab-grid">
            <Section title="Account">
                <dl class="kv">
                  <dt>Tier</dt><dd><TierChip tier={account.tier} /></dd>
                  <dt>Director</dt><dd>{account.director ?? '—'}</dd>
                  <dt>Location</dt><dd>{account.city}, {account.state}</dd>
                  <dt>Customer since</dt><dd>{account.customer_since}</dd>
                </dl>
            </Section>
            {#if devices.length > 0}
              <!-- Equipment summary — only renders when the
                   tenant runs device assets against this
                   account. Brewery accounts don't, so the
                   section disappears for empty rosters
                   instead of showing "0/0/0". -->
              <Section title="Equipment">
                  <dl class="kv">
                    <dt>Total units</dt><dd>{devices.length}</dd>
                    <dt>Active</dt><dd>{devices.filter((d) => d.phase !== 'decommissioned').length}</dd>
                    <dt>In service queue</dt><dd>{devices.filter((d) => ['received', 'triaging', 'refurbing', 'qa'].includes(d.phase)).length}</dd>
                  </dl>
              </Section>
            {/if}
            <Section title="Financials">
                <dl class="kv">
                  <dt>Total invoices</dt><dd>{invoices.length}</dd>
                  <dt>Past-due count</dt><dd>{invoices.filter((i) => i.status === 'past-due').length}</dd>
                  <dt>Paid YTD</dt><dd>{dollars(ytdRevCents)}</dd>
                </dl>
            </Section>
            <Section title="Work">
                <dl class="kv">
                  <dt>Total jobs</dt><dd>{jobs.length}</dd>
                  <dt>Open</dt><dd>{jobs.filter((j) => j.status !== 'closed' && j.status !== 'cancelled').length}</dd>
                </dl>
            </Section>
          </div>
        {:else if tab === 'devices'}
          {#if devices.length === 0}
            <p class="empty">No devices.</p>
          {:else}
            <table class="data-table data-table-striped">
              <thead>
                <tr><th>BOSS ID</th><th>SKU</th><th>Phase</th><th>Warranty</th><th>Installed</th></tr>
              </thead>
              <tbody>
                {#each devices as d (d.asset_id)}
                  <tr>
                    <td>
                      <Link to={entityHref('asset', d.asset_id)} className="mono">
                        {d.asset_id}
                      </Link>
                    </td>
                    <td class="mono">{d.sku}</td>
                    <td>{d.phase}</td>
                    <td>{d.warranty_through ?? '—'}</td>
                    <td>{d.first_seen}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        {:else if tab === 'tickets'}
          {#if jobs.length === 0}
            <p class="empty">No jobs.</p>
          {:else}
            {@const sortedJobs = [...jobs].sort((a, b) => b.opened_on.localeCompare(a.opened_on))}
            <table class="data-table data-table-striped">
              <thead>
                <tr><th>Job</th><th>Kind</th><th>Status</th><th>Opened</th><th>Title</th></tr>
              </thead>
              <tbody>
                {#each sortedJobs as j (j.id)}
                  <tr>
                    <td>
                      <Link to={entityHref('job', j.id)} className="mono">
                        {shortId(j.id)}
                      </Link>
                    </td>
                    <td>{j.kind}</td>
                    <td>{j.status}</td>
                    <td>{j.opened_on}</td>
                    <td>{j.title}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        {:else if tab === 'finance'}
          {#if invoices.length === 0}
            <p class="empty">No invoices.</p>
          {:else}
            {@const sortedInv = [...invoices].sort((a, b) => b.issued_on.localeCompare(a.issued_on))}
            <table class="data-table data-table-striped">
              <thead>
                <tr><th>Invoice</th><th>Status</th><th class="num">Amount</th><th>Issued</th><th>Due</th></tr>
              </thead>
              <tbody>
                {#each sortedInv as i (i.id)}
                  <tr>
                    <td>
                      <Link to={entityHref('invoice', i.id)} className="mono">
                        {i.id}
                      </Link>
                    </td>
                    <td>{i.status}</td>
                    <td class="num">{dollars(i.amount_cents)}</td>
                    <td>{i.issued_on}</td>
                    <td>{i.due_on}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        {:else if tab === 'shipments'}
          {#if shipments.length === 0}
            <p class="empty">No shipments.</p>
          {:else}
            <table class="data-table data-table-striped">
              <thead>
                <tr><th>Shipment</th><th>Status</th><th>Origin</th><th>Destination</th><th>ETA</th></tr>
              </thead>
              <tbody>
                {#each shipments as s (s.id)}
                  <tr>
                    <td class="mono"><EntityLink kind="shipment" id={s.id} /></td>
                    <td>{s.status}</td>
                    <td>{s.origin}</td>
                    <td>{s.destination}</td>
                    <td>{s.expected_delivery ?? '—'}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        {:else if tab === 'knowledge'}
          <KnowledgeBaseView entityKind="account" entityId={account.id} />
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .account-cap-note {
    padding: 8px 12px;
    background: #fff7ed;
    border: 1px solid #fdba74;
    border-radius: 6px;
    font-size: 13px;
    color: #7c2d12;
    margin: 8px 0 0 0;
  }
</style>
