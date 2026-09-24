<script lang="ts">
  // Cockpit — oriented around HOW THE SIMULATOR ENGAGES THE PUBLIC API.
  //
  // The simulator drives the company AS the workforce, entirely through
  // the public HTTP API (x-boss-user: automation:sim + x-sim-origin). It
  // touches the system no other way. This page reads the daemon's own
  // telemetry — what it's calling, how much, how fast — plus the
  // audit-log tail (what landed):
  //
  //   GET /simulator/api/telemetry (poll ~2s) → boss-simulator → the
  //       brewery-sim daemon's localhost control server.
  //   GET /api/events/tail         (poll ~3s) → the audit-log tail.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import ActorCoveragePanel from './ActorCoveragePanel.svelte';
  import type { SimTelemetry, ActorActivity, AuditEntry, ClockNow } from './types';

  const TELEMETRY_POLL_MS = 2_000;
  const CLOCK_POLL_MS = 2_000;
  const EVENTS_POLL_MS = 3_000;
  const EVENTS_LIMIT = 40;

  let tele = $state<SimTelemetry | null>(null);
  let teleError = $state<string | null>(null);

  // The clock (sim time / warp / paused) comes from clock-api, which owns
  // it — NOT the daemon telemetry, which is only up while the daemon ticks.
  // So these readouts stay correct even while the daemon is stopped (e.g.
  // mid seed-rebuild). clock-api isn't gateway-proxied, so boss-simulator
  // proxies it at /simulator/api/clock.
  let clock = $state<ClockNow | null>(null);
  let clockError = $state<string | null>(null);

  let events = $state<ReadonlyArray<AuditEntry>>([]);
  let eventsError = $state<string | null>(null);

  // Actor panels — how the sim engages the API, grouped by who's acting.
  // The sim already models its actors: the workforce (by role) + the named
  // counterparty chains (which decode to Account / Vendor / Bank) + the
  // Environment (world generation + materialization).
  const ACTOR_KINDS: ReadonlyArray<{ kind: ActorActivity['kind']; title: string; sub: string }> = [
    { kind: 'employee', title: 'Employee Actors', sub: 'the workforce — by role' },
    { kind: 'account', title: 'Account Actors', sub: 'customers — orders, payments, defaults' },
    { kind: 'vendor', title: 'Vendor Actors', sub: 'suppliers — fulfilment, invoicing, delivery' },
    { kind: 'bank', title: 'Bank', sub: 'payment settlement' },
    { kind: 'environment', title: 'Environment', sub: 'demand injected + end-of-day materialization' },
  ];

  // Elapsed sim-days since the daemon started — the calls/sim-day rate base.
  function simDaysBetween(from: string | null, to: string | null | undefined): number {
    if (!from || !to) return 0;
    const a = Date.parse(`${from}T00:00:00Z`);
    const b = Date.parse(`${to}T00:00:00Z`);
    if (Number.isNaN(a) || Number.isNaN(b)) return 0;
    return Math.max(0, (b - a) / 86_400_000);
  }
  // Authoritative current sim date — clock-api's `now` (it owns the clock),
  // falling back to the daemon's telemetry only if clock-api is unreachable.
  let currentSimDate = $derived(clock ? clock.now.slice(0, 10) : (tele?.cadence.sim_date ?? null));
  let elapsedSimDays = $derived(tele ? simDaysBetween(tele.started_sim_date, currentSimDate) : 0);
  function ratePerDay(calls: number): string {
    if (elapsedSimDays <= 0) return '—';
    const r = calls / elapsedSimDays;
    return `${r >= 10 ? r.toFixed(0) : r.toFixed(1)}/day`;
  }

  // The realism signal: how many distinct actors are behind a rollup, so
  // "200 shipping-clerk calls" reads as "from 1 person" vs "from 50". Only
  // the workforce is a per-Subject actor population today — each counterparty
  // is a single named chain (one process), not a set of distinct accounts /
  // vendors, so those rollups show no distinct count until the actors are
  // sourced per-Subject from the model. Empty noun = no label shown.
  const DISTINCT_NOUN: Record<ActorActivity['kind'], string> = {
    employee: 'person',
    account: 'account',
    vendor: 'vendor',
    bank: '',
    environment: '',
  };
  function distinctLabel(kind: ActorActivity['kind'], n: number): string {
    const noun = DISTINCT_NOUN[kind];
    // `!(n > 0)` (not `n <= 0`) so a missing/undefined count during a
    // daemon↔SPA version skew degrades to no label rather than throwing.
    if (!noun || !(n > 0)) return '';
    if (n === 1) return `1 ${noun}`;
    const plural = noun === 'person' ? 'people' : `${noun}s`;
    return `${n.toLocaleString()} ${plural}`;
  }

  // Actors grouped into the five panels, busiest-first within each. Empty
  // panels are hidden.
  let actorGroups = $derived(
    tele
      ? ACTOR_KINDS.map((k) => ({
          ...k,
          actors: [...(tele!.actors ?? [])]
            .filter((a) => a.kind === k.kind)
            .sort((a, b) => b.calls - a.calls),
        })).filter((g) => g.actors.length > 0)
      : [],
  );

  // Recent ticks, newest first.
  let recentRows = $derived(tele ? [...tele.recent].reverse() : []);

  async function refreshTelemetry(): Promise<void> {
    try {
      const r = await fetch('/simulator/api/telemetry', { headers: { accept: 'application/json' } });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      tele = (await r.json()) as SimTelemetry;
      teleError = null;
    } catch (e) {
      teleError = e instanceof Error ? e.message : String(e);
    }
  }

  async function refreshClock(): Promise<void> {
    try {
      const r = await fetch('/simulator/api/clock', { headers: { accept: 'application/json' } });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      clock = (await r.json()) as ClockNow;
      clockError = null;
    } catch (e) {
      clockError = e instanceof Error ? e.message : String(e);
    }
  }

  async function refreshEvents(): Promise<void> {
    try {
      const r = await fetch(`/api/events/tail?limit=${EVENTS_LIMIT}`, {
        headers: { accept: 'application/json' },
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      events = (await r.json()) as ReadonlyArray<AuditEntry>;
      eventsError = null;
    } catch (e) {
      eventsError = e instanceof Error ? e.message : String(e);
    }
  }

  function fmtTime(iso: string): string {
    const d = new Date(iso);
    if (isNaN(d.getTime())) return iso;
    return (
      d.toISOString().slice(0, 10) +
      ' ' +
      String(d.getUTCHours()).padStart(2, '0') +
      ':' +
      String(d.getUTCMinutes()).padStart(2, '0')
    );
  }

  onMount(() => {
    void refreshTelemetry();
    void refreshClock();
    void refreshEvents();
    const t = window.setInterval(() => void refreshTelemetry(), TELEMETRY_POLL_MS);
    const c = window.setInterval(() => void refreshClock(), CLOCK_POLL_MS);
    const e = window.setInterval(() => void refreshEvents(), EVENTS_POLL_MS);
    return () => {
      window.clearInterval(t);
      window.clearInterval(c);
      window.clearInterval(e);
    };
  });
</script>

<PageHeader
  eyebrow="Simulator · Public-API engagement"
  title="Cockpit"
  subtitle="How the simulator is driving the company — entirely through the public API, as the workforce."
/>

<!-- Clock — authoritative sim time / warp / paused from clock-api, which
     owns the clock. Rendered independently of the daemon telemetry so it
     stays live even while the daemon is stopped (e.g. mid seed-rebuild),
     when the rest of this page can't load. -->
{#if clock}
  <div class="cadence clock-strip">
    {#if currentSimDate}<span class="cad"><b>{currentSimDate}</b> sim date</span>{/if}
    {#if clock.warp_factor}<span class="cad">warp ×{clock.warp_factor}</span>{/if}
    <span class="badge" class:paused={clock.paused}>{clock.paused ? 'Paused' : 'Running'}</span>
    {#if clock.restart_in_progress}<span class="cad muted">rebuilding…</span>{/if}
  </div>
{:else if clockError}
  <p class="status error">Couldn't reach clock-api: {clockError}</p>
{/if}

{#if teleError}
  <p class="status error">Couldn't reach the simulator daemon: {teleError}</p>
  <p class="status">The daemon exposes its telemetry on a localhost-only control port; this is
    expected if the simulator isn't running (e.g. a non-demo deployment).</p>
{:else if !tele}
  <p class="status">Loading telemetry…</p>
{:else}
  <!-- Identity + tick cadence: who the sim is on the API, and its tick
       rhythm. (Sim time / warp / paused are clock-api's — the strip above.) -->
  <div class="identity">
    <div class="identity-main">
      <span class="id-label">Acting as</span>
      <code class="id-actor">{tele.actor}</code>
      <span class="id-role">{tele.role}</span>
      <span class="id-sep">·</span>
      <span class="id-via">via the public API</span>
      <code class="id-base">{tele.api_base}</code>
    </div>
    <div class="cadence">
      {#if tele.cadence.days_per_tick && tele.cadence.tick_interval_seconds}
        <span class="cad">{tele.cadence.days_per_tick}d / {tele.cadence.tick_interval_seconds}s per tick</span>
      {/if}
      <span class="cad muted">tick #{tele.tick_count.toLocaleString()}</span>
    </div>
  </div>

  <div class="cockpit-grid">
    <div class="cockpit-col">
      <Section title="Workforce execution">
        <p class="point-sub">
          <code>PUT /api/jobs/&#123;id&#125;/steps</code> — claim &amp; complete assigned work
        </p>
        <div class="stat">
          <span class="stat-num">{tele.workforce.completed.toLocaleString()}</span>
          <span class="stat-label">steps completed</span>
        </div>
        <dl class="kv">
          <dt>Claimed</dt>
          <dd>{tele.workforce.claimed.toLocaleString()}</dd>
          <dt>Deferred (short stock)</dt>
          <dd>{tele.workforce.deferred.toLocaleString()}</dd>
          <dt>Check-ins</dt>
          <dd>{tele.workforce.checkins.toLocaleString()}</dd>
          <dt class:err={tele.workforce.errors > 0}>Errors</dt>
          <dd class:err={tele.workforce.errors > 0}>{tele.workforce.errors.toLocaleString()}</dd>
        </dl>
      </Section>

      <Section title="Recent ticks">
        <p class="point-sub">Per-tick engagement (newest first)</p>
        {#if recentRows.length === 0}
          <p class="status">No ticks yet.</p>
        {:else}
          <ul class="tick-list">
            {#each recentRows as t (t.tick)}
              <li class="tick-row">
                <span class="tick-date">{t.sim_date ?? '—'}</span>
                <span class="tick-deltas">
                  <span class="d done">+{t.completed} done</span>
                  <span class="d claim">+{t.claimed} claimed</span>
                  {#if t.deferred > 0}<span class="d defer">{t.deferred} deferred</span>{/if}
                  {#if t.errors > 0}<span class="d err">{t.errors} err</span>{/if}
                </span>
              </li>
            {/each}
          </ul>
        {/if}
      </Section>

      <Section title="Audit log tail">
        <p class="point-sub">The audit log as the sim's calls land (GET /api/events/tail)</p>
        {#if eventsError}
          <p class="status error">Couldn't load events: {eventsError}</p>
        {:else if events.length === 0}
          <p class="status">No events yet.</p>
        {:else}
          <ul class="event-list">
            {#each events as ev (ev.event_id)}
              <li class="event-row">
                <span class="event-kind" title={ev.source}>{ev.kind}</span>
                <span class="event-time">{fmtTime(ev.timestamp)}</span>
                <span class="event-source">{ev.source}</span>
              </li>
            {/each}
          </ul>
        {/if}
      </Section>
    </div>

    <div class="cockpit-col">
      <!-- Roster coverage first: whether the sim is driving the whole
           brewery is the panel an operator must not have to dig for. -->
      <ActorCoveragePanel coverage={tele.actor_coverage} />

      <Section title="API engagement by actor" wide>
        <p class="point-sub">
          Calls the sim makes to the public API, by who's acting — cumulative count, distinct people
          per workforce role, + per-sim-day rate (errors flagged)
        </p>
        {#if actorGroups.length === 0}
          <p class="status">No API calls yet.</p>
        {:else}
          {#each actorGroups as group (group.kind)}
            <div class="actor-group">
              <h4 class="actor-group-title">
                {group.title}<span class="actor-group-sub">{group.sub}</span>
              </h4>
              {#each group.actors as a (a.label)}
                {@const dl = distinctLabel(group.kind, a.distinct)}
                <div class="actor">
                  <div class="actor-head">
                    <span class="actor-label">{a.label}</span>
                    <span class="actor-meta">
                      <span class="actor-calls">{a.calls.toLocaleString()}</span>
                      {#if dl}<span class="actor-distinct">{dl}</span>{/if}
                      <span class="actor-rate">{ratePerDay(a.calls)}</span>
                      {#if a.errors > 0}<span class="actor-err">{a.errors.toLocaleString()} err</span>{/if}
                    </span>
                  </div>
                  <ul class="endpoint-list">
                    {#each a.endpoints as e (e.endpoint)}
                      <li class="endpoint-row">
                        <code class="endpoint-name">{e.endpoint}</code>
                        <span class="endpoint-nums">
                          <span class="endpoint-calls">{e.calls.toLocaleString()}</span>
                          {#if e.errors > 0}<span class="endpoint-err">{e.errors}✗</span>{/if}
                        </span>
                      </li>
                    {/each}
                  </ul>
                </div>
              {/each}
            </div>
          {/each}
        {/if}
      </Section>
    </div>
  </div>
{/if}

<style>
  .identity {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    padding: 12px 16px;
    margin-bottom: 20px;
    background: var(--brew-cream);
    border: 1px solid #e6d2a8;
    border-radius: 8px;
  }
  .identity-main {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 6px;
    font-size: 0.9rem;
  }
  .id-label {
    color: #7a6855;
  }
  .id-actor {
    font-weight: 700;
    color: var(--brew-malt-dark);
    background: rgba(217, 155, 58, 0.16);
    padding: 0.05em 0.4em;
    border-radius: 4px;
  }
  .id-role {
    font-size: 0.78rem;
    color: var(--brew-malt);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .id-sep {
    color: #c9b896;
  }
  .id-via {
    color: #7a6855;
  }
  .id-base {
    color: #7a6855;
    font-size: 0.82rem;
  }
  .cadence {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 12px;
    font-size: 0.85rem;
    color: var(--brew-malt);
  }
  /* The clock strip stands on its own under the header (clock-api-sourced,
     independent of the daemon telemetry below it). */
  .clock-strip {
    margin: 4px 0 18px;
    padding: 9px 14px;
    border: 1px solid #ece7df;
    border-radius: 8px;
    background: #fbf9f5;
  }
  .cad b {
    color: var(--brew-malt-dark);
  }
  .cad.muted {
    color: #a8a29e;
  }
  .cockpit-grid {
    display: grid;
    grid-template-columns: minmax(280px, 380px) 1fr;
    gap: 24px;
    align-items: start;
  }
  .cockpit-col {
    display: flex;
    flex-direction: column;
    gap: 16px;
    min-width: 0;
  }
  .point-sub {
    margin: 0 0 10px;
    font-size: 0.78rem;
    color: #7a6855;
  }
  .point-sub code {
    font-size: 0.74rem;
  }
  .stat {
    display: flex;
    align-items: baseline;
    gap: 8px;
    margin-bottom: 12px;
  }
  .stat-num {
    font-family: var(--font-display);
    font-size: 2.4rem;
    font-weight: 700;
    color: var(--brew-malt-dark);
    line-height: 1;
  }
  .stat-label {
    font-size: 0.8rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--brew-malt);
  }
  .kv {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 4px 16px;
    margin: 0;
    font-size: 0.9rem;
  }
  .kv dt {
    color: #78716c;
  }
  .kv dd {
    margin: 0;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    text-align: right;
  }
  .kv dt.err,
  .kv dd.err {
    color: #8b2b1f;
  }
  .endpoint-list,
  .tick-list,
  .event-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .actor-group {
    margin-bottom: 14px;
  }
  .actor-group-title {
    margin: 0 0 6px;
    font-size: 0.8rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--brew-malt-dark);
    border-bottom: 1px solid #e6d2a8;
    padding-bottom: 3px;
  }
  .actor-group-sub {
    text-transform: none;
    letter-spacing: 0;
    font-weight: 400;
    color: #a8a29e;
    margin-left: 8px;
    font-size: 0.78rem;
  }
  .actor {
    margin: 0 0 8px;
  }
  .actor-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 8px;
    padding: 2px 0;
  }
  .actor-label {
    font-weight: 600;
    color: var(--brew-malt-dark);
  }
  .actor-meta {
    display: flex;
    align-items: baseline;
    gap: 10px;
    font-variant-numeric: tabular-nums;
    font-size: 0.85rem;
  }
  .actor-calls {
    font-weight: 600;
    color: var(--brew-malt-dark);
  }
  .actor-rate {
    color: var(--brew-malt);
  }
  .actor-distinct {
    color: var(--brew-malt);
    background: rgba(217, 155, 58, 0.12);
    padding: 0 0.4em;
    border-radius: 3px;
    white-space: nowrap;
  }
  .actor-err {
    color: #8b2b1f;
    font-weight: 600;
  }
  .endpoint-list {
    gap: 1px;
    margin-left: 10px;
  }
  .endpoint-row {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 8px;
    padding: 2px 6px;
    border-radius: 3px;
  }
  .endpoint-row:nth-child(odd) {
    background: rgba(217, 155, 58, 0.06);
  }
  .endpoint-name {
    color: var(--brew-malt);
    font-size: 0.76rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .endpoint-nums {
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-variant-numeric: tabular-nums;
    font-size: 0.8rem;
    flex-shrink: 0;
  }
  .endpoint-calls {
    font-weight: 600;
    color: var(--brew-malt-dark);
  }
  .endpoint-err {
    color: #8b2b1f;
  }
  .tick-list {
    gap: 2px;
    max-height: 300px;
    overflow-y: auto;
  }
  .tick-row {
    display: grid;
    grid-template-columns: 6.5em 1fr;
    gap: 0.5rem;
    align-items: baseline;
    padding: 3px 8px;
    border-bottom: 1px solid #f0e6cf;
    font-size: 0.82rem;
  }
  .tick-date {
    color: #7a6855;
    font-variant-numeric: tabular-nums;
  }
  .tick-deltas {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .d {
    font-variant-numeric: tabular-nums;
  }
  .d.done {
    color: #166534;
    font-weight: 600;
  }
  .d.claim {
    color: var(--brew-malt);
  }
  .d.defer {
    color: #92400e;
  }
  .d.err {
    color: #8b2b1f;
    font-weight: 600;
  }
  .event-list {
    gap: 1px;
    max-height: 300px;
    overflow-y: auto;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.78rem;
  }
  .event-row {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 0 0.5rem;
    padding: 3px 8px;
    border-bottom: 1px solid #f0e6cf;
  }
  .event-kind {
    grid-row: 1;
    color: var(--brew-malt-dark);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .event-time {
    grid-row: 1;
    grid-column: 2;
    color: #a8a29e;
    text-align: right;
  }
  .event-source {
    grid-row: 2;
    grid-column: 1 / -1;
    color: #a8957a;
    font-size: 0.72rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badge {
    display: inline-block;
    background: #dcfce7;
    color: #166534;
    border: 1px solid #86efac;
    border-radius: 4px;
    padding: 1px 8px;
    font-size: 0.78rem;
    font-weight: 600;
  }
  .badge.paused {
    background: #fee2e2;
    color: #991b1b;
    border-color: #fca5a5;
  }
  .status {
    margin: 0 0 6px;
    color: #7a6855;
    font-style: italic;
  }
  .status.error {
    color: #8b2b1f;
    font-style: normal;
  }
  @media (max-width: 820px) {
    .cockpit-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
