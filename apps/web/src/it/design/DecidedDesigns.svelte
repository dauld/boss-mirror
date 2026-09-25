<script lang="ts">
  // The `decided` panel of /it/design — the page's WORKING and OUT.
  //
  // Until 2026-09-24 the page rendered only its IN: a design left it the
  // moment its review completed, so four decided on the night of
  // 2026-09-23 were at `fold` with nothing showing they were in flight,
  // and the 34 settled that day showed nowhere (backlog 08372fdb, from
  // page audit 79db48ae). The set is a STATION, `design-decided`, not a
  // filter written here — the lens rule this page already follows: the
  // registry row defines the queue, the page renders it. A panel fetches
  // its own data (StationLens in boss-jobs), so this read is the panel's
  // and its failure is the panel's, painted where the rows would be.
  import Section from '@boss/web-kit/ui/Section.svelte';
  import {
    DECIDED_STATION,
    decidedRows,
    foldLabel,
    relTime,
    stationQueuePath,
    type DecidedRow,
  } from './designLens';

  type Panel =
    | { kind: 'loading' }
    | { kind: 'failed'; why: string }
    | {
        kind: 'ready';
        working: readonly DecidedRow[];
        settled: readonly DecidedRow[];
        windowDays: number | null;
      };

  let panel = $state<Panel>({ kind: 'loading' });

  // Also the failure line's Retry (backlog 3bbb194a): it re-runs this
  // panel's read and nothing else, so it says it is loading again first
  // rather than leaving the old failure standing while the read is out.
  async function load(): Promise<void> {
    panel = { kind: 'loading' };
    try {
      // decidedRows throws on a 200 that is not the envelope (67825067),
      // and that throw is this panel's failure line, not its empty one.
      const resp = await fetch(stationQueuePath(DECIDED_STATION));
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const body: unknown = await resp.json();
      const days = (body as { terminal_window_days?: unknown } | null)?.terminal_window_days;
      panel = {
        kind: 'ready',
        ...decidedRows(body),
        windowDays: typeof days === 'number' ? days : null,
      };
    } catch (e) {
      panel = { kind: 'failed', why: e instanceof Error ? e.message : String(e) };
    }
  }

  $effect(() => {
    void load();
  });

  const settledTitle = $derived(
    panel.kind === 'ready'
      ? panel.windowDays === null
        ? `Settled (${panel.settled.length})`
        : `Settled in the last ${panel.windowDays} days (${panel.settled.length})`
      : 'Settled',
  );
</script>

{#if panel.kind === 'loading'}
  <p class="empty">Loading the decided designs…</p>
{:else if panel.kind === 'failed'}
  <div class="decided-failed">
    <p class="load-failed" role="alert">Could not read the decided designs: {panel.why}</p>
    <button
      class="btn btn-sm"
      type="button"
      aria-label="Retry the decided designs"
      onclick={() => void load()}
    >
      Retry
    </button>
  </div>
{:else}
  <Section title={`Decided, being folded (${panel.working.length})`} wide>
    {#if panel.working.length === 0}
      <p class="empty">Nothing decided is waiting to be folded.</p>
    {:else}
      <table class="decided-table">
        <thead>
          <tr>
            <th>Packet</th>
            <th>Decided</th>
            <th>Fold</th>
          </tr>
        </thead>
        <tbody>
          {#each panel.working as row (row.id)}
            <tr>
              <td><a href={`/jobs/${row.id}`}><strong>{row.title}</strong></a></td>
              <td class="decided-when">{row.decided_on ? relTime(row.decided_on) : '—'}</td>
              <td class="decided-status">{foldLabel(row.fold_status)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </Section>

  <Section title={settledTitle} wide>
    {#if panel.settled.length === 0}
      <p class="empty">Nothing settled in this window.</p>
    {:else}
      <table class="decided-table">
        <thead>
          <tr>
            <th>Packet</th>
            <th>Settled</th>
            <th>Outcome</th>
            <th>Folded into</th>
          </tr>
        </thead>
        <tbody>
          {#each panel.settled as row (row.id)}
            <tr>
              <td><a href={`/jobs/${row.id}`}><strong>{row.title}</strong></a></td>
              <td class="decided-when">{row.closed_on ? relTime(row.closed_on) : '—'}</td>
              <td class="decided-status">{row.outcome ?? '—'}</td>
              <td class="decided-into">{row.folded_into ?? '—'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </Section>
{/if}

<style>
  .decided-table {
    width: 100%;
    border-collapse: collapse;
  }
  .decided-table th,
  .decided-table td {
    text-align: left;
    padding: 8px 12px;
    border-bottom: 1px solid var(--hairline);
    vertical-align: top;
    font-variant-numeric: tabular-nums;
  }
  /* Same instrument-text column labels as the review queue above. */
  .decided-table th {
    font-family: var(--font-mono);
    font-size: 11px;
    font-weight: 400;
    letter-spacing: var(--ls-nav);
    text-transform: uppercase;
    color: var(--static);
  }
  .decided-table tr:last-child td {
    border-bottom: none;
  }
  .decided-table a {
    color: inherit;
    text-decoration: none;
  }
  .decided-table a:hover {
    text-decoration: underline;
  }
  .decided-status {
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: var(--ls-label);
    text-transform: uppercase;
    color: var(--static);
    white-space: nowrap;
  }
  .decided-when {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--static);
    white-space: nowrap;
  }
  .decided-into {
    font-size: 13px;
    color: var(--static);
    overflow-wrap: anywhere;
  }
  .empty {
    color: var(--static);
    margin: 12px 0;
    line-height: 1.5;
  }
  /* The same line-and-Retry layout as the review queue's failure above. */
  .decided-failed {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 12px;
    margin: 12px 0;
  }
  .decided-failed p {
    flex: 1 1 24ch;
    margin: 0;
  }
</style>
