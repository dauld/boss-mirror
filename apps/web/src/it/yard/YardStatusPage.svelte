<script lang="ts">
  // Yard status — "what is the yard doing, and why?" answered from live
  // system-of-record data (the-cluster-is-the-system.md Phase 0). Where
  // the Train Yard board watches trains move, this page answers the
  // operational question an operator used to SSH for: where does each
  // train sit, and if one is stuck, WHY. The block reason that used to
  // live buried in a step's metadata (four hours wedged on 2026-09-02)
  // is a first-class line here, and the boarding predicate is rendered
  // from the LIVE cadence rows — no threshold is baked into this page.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import StatusChip from '@boss/web-kit/ui/StatusChip.svelte';
  import { formatRelative } from '@boss/web-kit/ui/date';
  import type { Remote } from '../../data/remote';
  import {
    blockLabel,
    fetchYardStatus,
    journeyText,
    phaseLabel,
    trainTone,
    type YardStatus,
  } from './yard-status';

  let status = $state<Remote<YardStatus>>({ kind: 'loading' });
  // One clock for every relative stamp on the page, taken when the data
  // arrived — formatRelative takes `now` explicitly, no hidden wallclock.
  let loadedAt = $state<Date>(new Date());

  async function refresh(): Promise<void> {
    status = await fetchYardStatus();
    loadedAt = new Date();
  }

  onMount(() => {
    void refresh();
    // 15s: fast enough that a newly-stuck train shows promptly, slow
    // enough for a guest-safe read.
    const t = setInterval(() => void refresh(), 15_000);
    return () => clearInterval(t);
  });

  const outcomeTone = (o: string): 'ok' | 'warn' | 'muted' =>
    o === 'arrived' ? 'ok' : o === 'cancelled' ? 'warn' : 'muted';
</script>

<div class="ys-root">
  <PageHeader
    eyebrow="IT · Forge line"
    title="Yard status"
    subtitle="Where every train sits, and — when one is stuck — why. Computed from the system of record: trains, the dock, the live cadence rules, the delivery policy."
  />

  {#if status.kind === 'loading'}
    <p class="ys-quiet">Reading the yard…</p>
  {:else if status.kind === 'failed'}
    <p class="ys-fail">
      The yard did not answer: {status.error}. This page refuses to guess — an
      unreachable read is not an empty yard.
    </p>
  {:else}
    {@const s = status.data}

    <!-- 00 — IN FLIGHT: where each train sits, and the block if stuck -->
    <div class="ys-section">00 — IN FLIGHT</div>
    {#if s.trains.length === 0}
      <p class="ys-quiet">No trains in flight.</p>
    {:else}
      <div class="ys-trains">
        {#each s.trains as t (t.id)}
          <div class="ys-train" class:blocked={!!t.block}>
            <div class="ys-train-head">
              <span class="ys-train-title">{t.title}</span>
              <StatusChip value={phaseLabel(t.phase)} tone={trainTone(t)} />
              {#if t.car_count > 0}
                <span class="ys-cars">{t.car_count} car{t.car_count === 1 ? '' : 's'}</span>
              {/if}
              {#if t.pr_url}
                <a class="ys-pr" href={t.pr_url} target="_blank" rel="noreferrer">PR ↗</a>
              {/if}
            </div>
            {#if t.at_step}
              <div class="ys-at">at: {t.at_step}</div>
            {/if}
            {#if t.block}
              <!-- The buried fact, surfaced. -->
              <div class="ys-block">{blockLabel(t.block)}</div>
              {#if t.block.kind === 'deploy-blocked' && t.block.since}
                <div class="ys-block-since">
                  blocked since {formatRelative(t.block.since, loadedAt)}
                </div>
              {/if}
              {#if t.block.kind === 'stalled'}
                <div class="ys-block-since">
                  stalled since {formatRelative(t.block.since, loadedAt)}
                </div>
              {/if}
            {/if}
          </div>
        {/each}
      </div>
    {/if}

    <!-- 01 — THE DOCK + the boarding predicate, from the live registry -->
    <div class="ys-section">01 — THE DOCK</div>
    <p class="ys-boarding" title="Read from the live cadence_rules — not a constant in this page.">
      {s.boarding.summary}
    </p>
    {#if s.dock.length === 0}
      <p class="ys-quiet">No cars parked and ready to board.</p>
    {:else}
      <table class="ys-table">
        <thead>
          <tr><th>car</th><th>branch</th><th>parked</th></tr>
        </thead>
        <tbody>
          {#each s.dock as c (c.id)}
            <tr>
              <td>{c.title}</td>
              <td class="ys-mono">{c.branch ?? '—'}</td>
              <td class="ys-mono ys-dim">{c.parked_since}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    <!-- 02 — RECENT: the last few trains and how they ended -->
    <div class="ys-section">02 — RECENT TRAINS</div>
    {#if s.recent.length === 0}
      <p class="ys-quiet">No trains have closed recently.</p>
    {:else}
      <table class="ys-table">
        <thead>
          <tr><th>train</th><th>outcome</th><th>journey</th></tr>
        </thead>
        <tbody>
          {#each s.recent as r (r.id)}
            <tr>
              <td>{r.title}</td>
              <td><StatusChip value={r.outcome} tone={outcomeTone(r.outcome)} /></td>
              <td class="ys-mono ys-dim">{journeyText(r.journey_seconds)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    <!-- 03 — STRANDED: green gates no car claims (cheap signal) -->
    {#if s.stranded.length > 0}
      <div class="ys-section">03 — STRANDED GREENS</div>
      <p class="ys-quiet">
        Gated green, never parked — so never on the dock, so they cannot board.
        Rescue (rebase + re-gate) or drop; never rebuild blind.
      </p>
      <ul class="ys-stranded">
        {#each s.stranded as g (g.branch)}
          <li class="ys-mono">{g.branch}</li>
        {/each}
      </ul>
    {/if}

    <!-- The thresholds this yard enforces, named from the policy row. -->
    <div class="ys-footnote">
      {#if s.policy.stall_hours != null || s.policy.max_red_trains != null}
        Policy (train-conductor):
        {#if s.policy.stall_hours != null}a train stalls after {s.policy.stall_hours}h without
          progress{/if}{#if s.policy.stall_hours != null && s.policy.max_red_trains != null};
        {/if}{#if s.policy.max_red_trains != null}a car holds after {s.policy.max_red_trains} red
          trains{/if}.
      {:else}
        No delivery policy is configured; the conductor uses its compiled fallbacks.
      {/if}
    </div>
  {/if}
</div>

<style>
  .ys-root { padding: 0 32px 32px; }
  .ys-section {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px; letter-spacing: var(--ls-eyebrow, 0.3em);
    color: var(--signal, #5fd4a8); margin: 28px 0 8px;
    display: flex; align-items: center; gap: 12px;
  }
  .ys-section::after { content: ''; flex: 1; border-top: 1px solid var(--hairline, #2a3138); }
  .ys-quiet { color: var(--static, #7a838c); font-size: 13px; }
  .ys-fail {
    color: var(--warn, #d9a441);
    border: 1px solid var(--warn, #d9a441);
    padding: 8px 12px; font-size: 13px;
  }
  .ys-trains { display: flex; flex-direction: column; gap: 10px; }
  .ys-train {
    border: 1px solid var(--hairline, #2a3138);
    padding: 10px 12px; display: flex; flex-direction: column; gap: 4px;
  }
  .ys-train.blocked { border-color: var(--err, #d9534f); }
  .ys-train-head { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .ys-train-title { font-weight: 600; }
  .ys-cars { color: var(--static, #7a838c); font-size: 12px; }
  .ys-pr {
    font-family: var(--font-mono, ui-monospace, monospace); font-size: 12px;
    color: var(--signal, #5fd4a8); text-decoration: none; margin-left: auto;
  }
  .ys-at { color: var(--static, #7a838c); font-size: 13px; }
  .ys-block {
    color: var(--err, #d9534f); font-weight: 600; font-size: 13px;
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .ys-block-since { color: var(--static, #7a838c); font-size: 12px; }
  .ys-boarding {
    font-size: 13px; color: var(--ink, inherit);
    border-left: 2px solid var(--signal, #5fd4a8); padding-left: 10px; margin: 8px 0;
  }
  .ys-table { width: 100%; border-collapse: collapse; font-size: 13px; }
  .ys-table th {
    text-align: left; font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase;
    color: var(--static, #7a838c); font-weight: 400;
    border-bottom: 1px solid var(--hairline, #2a3138); padding: 4px 12px 4px 0;
  }
  .ys-table td { padding: 6px 12px 6px 0; border-bottom: 1px solid var(--hairline, #2a3138); }
  .ys-mono { font-family: var(--font-mono, ui-monospace, monospace); }
  .ys-dim { color: var(--static, #7a838c); }
  .ys-stranded { list-style: none; padding: 0; margin: 6px 0; display: flex; flex-direction: column; gap: 4px; }
  .ys-stranded li { color: var(--warn, #d9a441); font-size: 13px; }
  .ys-footnote { color: var(--static, #7a838c); font-size: 12px; margin-top: 24px; max-width: 70ch; }
</style>
