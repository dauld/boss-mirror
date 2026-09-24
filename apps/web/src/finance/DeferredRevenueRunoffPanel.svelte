<script lang="ts">
  // ASC 606 step 5 — "Deferred revenue runoff" 12-month projection.
  // Loads /api/ledger/deferred-revenue-runoff anchored on the balance
  // sheet's `as_of`; renders a calendar-month table with the drift row
  // so a reader can see schedules-vs-GL parity at a glance.

  import Section from '@boss/web-kit/ui/Section.svelte';
  import {
    formatUsd,
    loadDeferredRevenueRunoff,
    type DeferredRevenueRunoff,
  } from './ledger';

  let { asOf }: { asOf: string } = $props();

  let data = $state<DeferredRevenueRunoff | null>(null);
  let loading = $state(true);

  $effect(() => {
    const anchor = asOf;
    let cancelled = false;
    loading = true;
    (async () => {
      const d = await loadDeferredRevenueRunoff(anchor || null, 12);
      if (!cancelled) {
        data = d;
        loading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function monthLabel(iso: string): string {
    // `iso` is a first-of-month `YYYY-MM-DD`. Render as "May 2026".
    // Parsing as components avoids timezone-shifted off-by-one drift.
    const [y, m] = iso.split('-').map((p) => Number(p));
    if (!y || !m) return iso;
    const d = new Date(Date.UTC(y, m - 1, 1));
    return d.toLocaleString('en-US', { month: 'short', year: 'numeric', timeZone: 'UTC' });
  }
</script>

<Section title="Deferred revenue runoff">
    {#if loading && !data}
      <p class="empty">Loading runoff projection…</p>
    {:else if !data}
      <p class="empty load-failed" role="alert">Runoff projection unavailable.</p>
    {:else}
      {@const d = data}
      <p class="muted" style="margin:0 0 12px; font-size:13px">
        Forecast of how the current <span class="mono">2200 Deferred Revenue</span>
        balance rolls into recognized revenue over the next {d.horizon_months} months,
        derived from active <span class="mono">revenue_schedules</span>. The
        scheduler posts one journal entry per period on the first open day.
      </p>

      <div class="runoff-summary" style="display:grid; grid-template-columns:repeat(3, 1fr); gap:12px; margin-bottom:12px">
        <div class="stat">
          <div class="stat-label">GL balance (2200)</div>
          <div class="stat-value">{formatUsd(d.deferred_account_balance_cents)}</div>
        </div>
        <div class="stat">
          <div class="stat-label">Schedules remaining</div>
          <div class="stat-value">{formatUsd(d.schedules_remaining_cents)}</div>
        </div>
        <div class="stat">
          <div class="stat-label">Drift (GL − schedules)</div>
          <div class="stat-value" class:drift-warn={d.drift_cents !== 0}>
            {formatUsd(d.drift_cents)}
          </div>
        </div>
      </div>

      {#if d.drift_cents !== 0}
        <div
          role="note"
          style="margin-bottom:12px; padding:8px 12px; border:1px solid var(--busy); background:var(--warn-wash); border-radius:6px; font-size:12px; color:var(--warn)"
        >
          <strong>Drift:</strong>
          the 2200 GL balance and the sum of active schedule remainders
          disagree. Expected during warm-up (schedules seeded, deferred
          revenue not yet posted) or after a manual JE touching 2200.
        </div>
      {/if}

      <table class="tb-table">
        <thead>
          <tr>
            <th style="text-align:left">Month</th>
            <th style="text-align:right">Recognition</th>
          </tr>
        </thead>
        <tbody>
          {#each d.months as m (m.month)}
            <tr>
              <td>{monthLabel(m.month)}</td>
              <td style="text-align:right">{formatUsd(m.amount_cents)}</td>
            </tr>
          {/each}
          {#if d.beyond_horizon_cents !== 0}
            <tr style="border-top:1px solid var(--hairline)">
              <td style="font-style:italic; color:var(--static)">Beyond {d.horizon_months} months</td>
              <td style="text-align:right; font-style:italic; color:var(--static)">
                {formatUsd(d.beyond_horizon_cents)}
              </td>
            </tr>
          {/if}
          <tr style="border-top:2px solid var(--hairline)">
            <td style="font-weight:700">Total schedules remaining</td>
            <td style="text-align:right; font-weight:700">
              {formatUsd(d.schedules_remaining_cents)}
            </td>
          </tr>
        </tbody>
      </table>
    {/if}
</Section>

<style>
  .stat {
    padding: 10px 12px;
    border: 1px solid var(--hairline);
    border-radius: 6px;
    background: var(--ink-raised);
  }
  .stat-label {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--static);
  }
  .stat-value {
    margin-top: 4px;
    font-size: 16px;
    font-weight: 600;
    color: var(--fog);
  }
  .drift-warn {
    color: var(--warn);
  }
  .muted {
    color: var(--static);
  }
</style>
