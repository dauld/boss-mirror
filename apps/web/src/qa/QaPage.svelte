<script lang="ts">
  // QA & Compliance — brewery quality + regulatory hub.
  // Pulls live counts from the people + jobs APIs so the page
  // tracks the actual state of the brewery rather than
  // hand-coded copy.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { appNow } from '@boss/web-kit/sim-clock';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { expiringCerts } from '../people/utils';
  import { humanizeClassCode, type Employee } from '../people/types';
  import {
    workflowSurfaces,
    type WorkflowSpec,
  } from '../workflows/workflowTypes';
  import { href } from '../router';
  import { entityHref } from '@boss/web-kit/ui/entity-href';

  type Tab = 'overview' | 'batch-qc' | 'compliance' | 'pm';

  const TABS: ReadonlyArray<{ id: Tab; label: string }> = [
    { id: 'overview', label: 'Overview' },
    { id: 'batch-qc', label: 'Batch QC' },
    { id: 'compliance', label: 'Compliance' },
    { id: 'pm', label: 'Equipment preventive maintenance' },
  ];

  type JobCounts = Record<string, number>;
  type LiveJob = {
    id: string;
    kind: string;
    title: string;
    status: string;
    priority: string;
    opened_on: string | null;
  };

  let roster = $state<Employee[]>([]);
  let counts = $state<JobCounts>({});
  let recentJobs = $state<LiveJob[]>([]);
  // Kind-slugs whose Workflow declares `metadata.surfaces ⊇ ['qa']`.
  // Discovered from /api/workflows so this page tracks the registry
  // instead of hardcoding brewery slugs. Drives both the open-job
  // counts and the recent-jobs feed below.
  let qaKinds = $state<Set<string>>(new Set());
  /// Non-null when any of the page's primary reads failed — rendered
  /// instead of zeroed counts and empty rosters, which an outage is
  /// not (packet 3fba9c35, the false-empty sweep).
  let loadFailed = $state<string | null>(null);
  let loading = $state(true);
  let tab = $state<Tab>('overview');

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const [pResp, sResp, lResp, kResp] = await Promise.all([
          fetch('/api/people'),
          fetch('/api/jobs/summary?status=open'),
          fetch('/api/jobs/live'),
          fetch('/api/workflows'),
        ]);
        const down = [pResp, sResp, lResp, kResp].find((x) => !x.ok);
        const pBody = pResp.ok ? ((await pResp.json()) as Employee[]) : [];
        const sBody = sResp.ok ? await sResp.json() : { counts: {} };
        const lBody = lResp.ok ? await lResp.json() : { recent: [] };
        const kBody = kResp.ok ? ((await kResp.json()) as WorkflowSpec[]) : [];
        if (!cancelled) {
          roster = pBody;
          counts = (sBody.counts ?? {}) as JobCounts;
          recentJobs = (lBody.recent ?? []) as LiveJob[];
          qaKinds = new Set(
            kBody
              .filter((k) => workflowSurfaces(k).includes('qa'))
              .map((k) => k.kind),
          );
          loadFailed = down ? `HTTP ${down.status}` : null;
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

  // Compliance — cert expiry rollups by horizon.
  let expiring30 = $derived(expiringCerts(30, roster));
  let expiring60 = $derived(expiringCerts(60, roster));
  let expiring90 = $derived(expiringCerts(90, roster));

  let sortedExpiring = $derived(
    [...expiring90].sort((a, b) => {
      const da = new Date(a.cert.expires_on!).getTime();
      const db = new Date(b.cert.expires_on!).getTime();
      return da - db;
    }),
  );

  // Cert volume by issuing body — rough fidelity check on the
  // compliance program ("are we tracking enough certs?").
  let certsByBody = $derived.by(() => {
    const m = new Map<string, number>();
    for (const e of roster) {
      for (const c of e.certifications ?? []) {
        m.set(c.issuing_body, (m.get(c.issuing_body) ?? 0) + 1);
      }
    }
    return [...m.entries()].sort((a, b) => b[1] - a[1]);
  });

  // Total active certs vs. total active employees — a quick
  // staffing-coverage signal (a brewery this size should land
  // close to 1.0+ certs per active employee).
  let totalCerts = $derived(
    roster.reduce((acc, e) => acc + (e.certifications?.length ?? 0), 0),
  );
  let totalActive = $derived(roster.filter((e) => e.status === 'active').length);

  // Job-side numbers — open Jobs of the QA-surfaced kinds. Summed
  // across every kind the registry flags for the QA page, so the
  // figure tracks the published workflow set rather than two baked-in
  // brewery slugs.
  let openQaJobs = $derived(
    [...qaKinds].reduce((acc, k) => acc + (counts[k] ?? 0), 0),
  );

  // Lab roster — staff who own QC.
  let labStaff = $derived(
    roster.filter(
      (e) => e.role === 'lab-tech' || e.role === 'head-brewer',
    ),
  );

  // Latest 8 QA jobs in flight (qaKinds-driven feed).
  let recentQaJobs = $derived(
    recentJobs.filter((j) => qaKinds.has(j.kind)).slice(0, 8),
  );

  function daysUntil(iso: string): number {
    return Math.floor(
      (new Date(iso).getTime() - appNow().getTime()) / (1000 * 60 * 60 * 24),
    );
  }
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="QA & Compliance"
    title="Quality Assurance"
    subtitle={`${expiring30.length} certs expiring within 30 days · ${openQaJobs} QA jobs open`}
  />

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

  {#if loading}
    <p class="empty">Loading…</p>
  {:else if loadFailed}
    <p class="empty load-failed" role="alert">
      Couldn't load QA data — {loadFailed}
    </p>
  {:else if tab === 'overview'}
    <div
      class="tab-content"
      style="display:flex; flex-wrap:wrap; gap:16px; padding:16px 0"
    >
      <Section title="QA jobs — in flight">
          <dl class="kv">
            <dt>Open QA jobs</dt>
            <dd><strong>{openQaJobs}</strong></dd>
            <dt>Lab + Head-Brewer staff</dt>
            <dd><strong>{labStaff.length}</strong></dd>
          </dl>
          <p class="prose" style="margin-top:8px">
            Quality work rides on the Workflows this tenant flags for
            QA — gravity, pH, IBU, ABV, sensory passes and equipment
            walkdowns, all tracked as Steps on those Jobs.
            <Link to={href('/ux/jobs?status=open')}>
              View open jobs →
            </Link>
          </p>
      </Section>

      <Section title="Compliance — certifications">
          <dl class="kv">
            <dt>Total certs tracked</dt><dd><strong>{totalCerts}</strong></dd>
            <dt>Coverage / active employee</dt>
            <dd><strong>{totalActive > 0 ? (totalCerts / totalActive).toFixed(1) : '0'}</strong></dd>
            <dt>Expiring ≤ 30 days</dt>
            <dd>
              <strong style={expiring30.length > 0 ? 'color:var(--warn)' : ''}>{expiring30.length}</strong>
            </dd>
            <dt>Expiring ≤ 60 days</dt><dd><strong>{expiring60.length}</strong></dd>
            <dt>Expiring ≤ 90 days</dt><dd><strong>{expiring90.length}</strong></dd>
          </dl>
      </Section>

      <Section title="Equipment — preventive maintenance program">
          <dl class="kv">
            <dt>QA workflows published</dt><dd><strong>{qaKinds.size}</strong></dd>
          </dl>
          <p class="prose" style="margin-top:8px">
            Quarterly + annual walkdowns on fermenters, brite tanks,
            heat exchanger, canning line, glycol chiller — tracked as
            QA-surfaced Jobs alongside batch quality control.
            <Link to={href('/ux/jobs?status=open')}>
              View open jobs →
            </Link>
          </p>
      </Section>

    </div>

  {:else if tab === 'batch-qc'}
    <section class="list-section" style="padding:16px 0">
      <p class="prose" style="margin-bottom:12px">
        Brewery QC is a step on every quality-bearing Job — gravity,
        pH, IBU, ABV, and a sensory pass before the cellar handoff.
        <strong>{openQaJobs}</strong> QA jobs are currently in flight
        across the brewhouse.
        <Link to={href('/ux/jobs?status=open')}>
          Open all jobs →
        </Link>
      </p>

      {#if recentQaJobs.length > 0}
        <h3 class="section-title">Most recent in flight</h3>
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>Batch</th>
              <th>Title</th>
              <th>Priority</th>
              <th>Opened</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>
            {#each recentQaJobs as j (j.id)}
              <tr>
                <td class="mono">
                  <Link to={entityHref('job', j.id)}>
                    {j.id.slice(0, 8)}
                  </Link>
                </td>
                <td>{j.title}</td>
                <td>{j.priority}</td>
                <td>{j.opened_on ?? '—'}</td>
                <td>{j.status}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}

      <h3 class="section-title" style="margin-top:24px">QC staffing</h3>
      {#if labStaff.length === 0}
        <p class="empty">No lab-tech or head-brewer staff on record.</p>
      {:else}
        <table class="data-table data-table-striped">
          <thead>
            <tr><th>Name</th><th>Role</th><th>Location</th><th class="num">Certs</th></tr>
          </thead>
          <tbody>
            {#each labStaff as e (e.id)}
              <tr>
                <td>
                  <Link to={entityHref('employee', e.id)}>
                    {e.name}
                  </Link>
                </td>
                <td>{humanizeClassCode(e.role)}</td>
                <td>{e.location}</td>
                <td class="num">{e.certifications?.length ?? 0}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>

  {:else if tab === 'compliance'}
    <section class="list-section" style="padding:16px 0">
      <h3 class="section-title">Certs by issuing body</h3>
      {#if certsByBody.length === 0}
        <p class="empty">No certifications on record.</p>
      {:else}
        <table class="data-table data-table-striped" style="margin-bottom:24px">
          <thead>
            <tr><th>Issuing body</th><th class="num">Certs</th></tr>
          </thead>
          <tbody>
            {#each certsByBody as [body, n] (body)}
              <tr>
                <td>{body}</td>
                <td class="num">{n}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}

      <h3 class="section-title">Expiring within 90 days</h3>
      {#if sortedExpiring.length === 0}
        <p class="empty">No certifications expiring within 90 days.</p>
      {:else}
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>Employee</th>
              <th>Certification</th>
              <th>Issuing body</th>
              <th>Expires</th>
              <th class="num">Days until expiry</th>
            </tr>
          </thead>
          <tbody>
            {#each sortedExpiring as row, i (`${row.employee.id}-${row.cert.name}-${i}`)}
              {@const d = daysUntil(row.cert.expires_on!)}
              <tr>
                <td>
                  <Link to={entityHref('employee', row.employee.id)}>
                    {row.employee.name}
                  </Link>
                </td>
                <td>{row.cert.name}</td>
                <td>{row.cert.issuing_body}</td>
                <td>{row.cert.expires_on}</td>
                <td class="num">
                  {#if d <= 30}
                    <strong style="color:var(--warn)">{d}d</strong>
                  {:else}
                    {d}d
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>

  {:else if tab === 'pm'}
    <section class="list-section" style="padding:16px 0">
      <p class="prose" style="margin-bottom:12px">
        <strong>{openQaJobs}</strong> QA jobs are open.
        Quarterly fermenter walkdowns, annual mash-tun rebuild,
        glycol chiller service, canning-line preventive — all
        tracked as QA-surfaced Jobs alongside batch quality control.
        <Link to={href('/ux/jobs?status=open')}>
          Open all jobs →
        </Link>
      </p>

      {#if recentQaJobs.length > 0}
        <h3 class="section-title">Most recent in flight</h3>
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>Job</th>
              <th>Title</th>
              <th>Priority</th>
              <th>Opened</th>
            </tr>
          </thead>
          <tbody>
            {#each recentQaJobs as j (j.id)}
              <tr>
                <td class="mono">
                  <Link to={entityHref('job', j.id)}>
                    {j.id.slice(0, 8)}
                  </Link>
                </td>
                <td>{j.title}</td>
                <td>{j.priority}</td>
                <td>{j.opened_on ?? '—'}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  {/if}
</div>

<style>
  .section-title {
    font-size: 0.95rem;
    font-weight: 600;
    margin: 0 0 8px;
    color: var(--static);
  }
</style>
