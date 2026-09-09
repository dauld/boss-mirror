<script lang="ts">
  // The Marshalling Yard — the UPSTREAM third of the operator surface
  // (packet babcb6cd; design settled on 2d4a5a8b).
  //
  // The Train Yard is the last third of a car's life. This is the
  // first: what is waiting, on whom, for how long — and the question
  // David actually asked on 2026-09-09, which a depth board cannot
  // answer: "now that the cars move consistently I cannot see where
  // upstream bottlenecks might be FORMING."
  //
  // Forming is a rate, so every siding here carries arrivals and
  // departures over a wall-clock window beside its depth, and the page
  // opens by NAMING THE CONSTRAINT — the one queue that would take
  // longest to clear at the rate it is actually being worked. One queue
  // always is; naming it is the point.
  //
  // NO NUMBER THIS SURFACE MAKES UP. A station whose membership the
  // log cannot be attributed to (the dock's Job-metadata clause, a
  // per-actor watchlist) states the clause that blinded it and is never
  // eligible to be called the constraint. A failed read renders as a
  // failure, never as an empty yard.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { href, navigate } from '../../router';
  import type { Remote } from '../../data/remote';
  import {
    constraintOf,
    drainHours,
    joinSidings,
    loadFlow,
    loadStations,
    loadWaits,
    longestWaits,
    parseQueueAge,
    waitText,
    whyNotMoving,
    type Siding,
    type StationFlowEnvelope,
    type StationLoadRow,
  } from './marshalling';

  // Offered windows. A day is the default: the shortest span in which
  // every queue on this network has had a chance to be worked once, so
  // anything shorter reports "not draining" about a quiet night.
  const WINDOWS = [24, 72, 168] as const;
  const WINDOW_LABEL: Readonly<Record<number, string>> = { 24: '24 h', 72: '3 d', 168: '7 d' };
  /** How many of the longest waits the table shows. */
  const WAIT_ROWS = 12;

  let windowHours = $state<number>(24);
  let load = $state<Remote<ReadonlyArray<StationLoadRow>>>({ kind: 'loading' });
  let flow = $state<Remote<StationFlowEnvelope>>({ kind: 'loading' });
  let waits = $state<Remote<ReturnType<typeof parseQueueAge>>>({ kind: 'loading' });

  async function refresh(): Promise<void> {
    // The three reads are independent, so they go out together.
    const [l, f, w] = await Promise.all([loadStations(), loadFlow(windowHours), loadWaits()]);
    load = l;
    flow = f;
    waits = w;
  }

  onMount(() => {
    void refresh();
    // 60s. A queue's rate is measured over hours; polling it faster
    // would re-scan the log for a figure that cannot have moved.
    const t = setInterval(() => void refresh(), 60_000);
    return () => clearInterval(t);
  });

  function pick(h: number): void {
    windowHours = h;
    void refresh();
  }

  const sidings = $derived.by<ReadonlyArray<Siding>>(() =>
    load.kind === 'ready' && flow.kind === 'ready'
      ? joinSidings(load.data, flow.data)
      : [],
  );
  const holding = $derived(sidings.filter((s) => s.depth > 0));
  const clear = $derived(sidings.filter((s) => s.depth === 0));
  const constraint = $derived(constraintOf(sidings, windowHours));
  const blind = $derived(sidings.filter((s) => s.flow.kind === 'unavailable'));
  const longest = $derived(
    waits.kind === 'ready' ? longestWaits(waits.data.waits, WAIT_ROWS) : [],
  );

  function clearText(s: Siding): string {
    if (s.flow.kind !== 'counted') return '—';
    const h = drainHours(s.depth, s.flow.served, windowHours);
    return h === null ? 'not draining' : `${h.toFixed(0)} h`;
  }

  function netText(s: Siding): string {
    if (s.flow.kind !== 'counted') return '—';
    const n = s.flow.net;
    return n > 0 ? `+${n}` : `${n}`;
  }

  const openPacket = (jobId: string) => navigate(`/jobs/${jobId}`);
</script>

<div class="my-root">
  <PageHeader
    eyebrow="IT · Upstream · Before the dock"
    title="Marshalling Yard"
    subtitle="What waits, on whom, for how long — and which queue is the constraint right now. Depth says what is waiting; the rate beside it says whether a bottleneck is forming."
  />

  <div class="my-controls" role="group" aria-label="Measurement window">
    <span class="my-controls-label">window</span>
    {#each WINDOWS as h (h)}
      <button
        type="button"
        aria-pressed={windowHours === h}
        onclick={() => pick(h)}>{WINDOW_LABEL[h]}</button
      >
    {/each}
    {#if flow.kind === 'ready' && flow.data.asOf}
      <span class="my-asof">read {flow.data.asOf}</span>
    {/if}
  </div>

  {#if load.kind === 'failed'}
    <p class="my-fail">
      The station load did not answer: {load.error}. An unreachable read is not an
      empty yard, so this page shows nothing rather than a clear one.
    </p>
  {:else if flow.kind === 'failed'}
    <p class="my-fail">
      The station flow did not answer: {flow.error}. Depth without a rate cannot say
      whether anything is forming, so the constraint is not named.
    </p>
  {:else if load.kind === 'loading' || flow.kind === 'loading'}
    <p class="my-quiet">Reading the network…</p>
  {:else}
    <!-- 00 — THE CONSTRAINT. One queue always is; this names it. -->
    <div class="my-section">00 — THE CONSTRAINT</div>
    <div class="my-constraint" class:named={constraint.kind === 'station'}>
      {#if constraint.kind === 'station'}
        <div class="my-constraint-name">{constraint.station}</div>
      {:else}
        <div class="my-constraint-name muted">No queue can be named</div>
      {/if}
      <div class="my-constraint-why">{constraint.because}</div>
    </div>

    <!-- 01 — SIDINGS. Depth, then the rate, then why it is not moving. -->
    <div class="my-section">01 — SIDINGS HOLDING WORK</div>
    {#if holding.length === 0}
      <p class="my-quiet">Every watched station is clear.</p>
    {:else}
      <table class="my-table">
        <thead>
          <tr>
            <th>Station</th>
            <th class="num">Depth</th>
            <th class="num">Oldest</th>
            <th class="num">Arrived</th>
            <th class="num">Served</th>
            <th class="num">Net</th>
            <th class="num">Clears in</th>
            <th>Why it is not moving</th>
          </tr>
        </thead>
        <tbody>
          {#each holding as s (s.station)}
            <tr class:constraint={constraint.kind === 'station' && constraint.station === s.station}>
              <td class="mono">{s.station}</td>
              <td class="num">
                {s.depth}{#if s.wipLimit !== null}<span class="dim"> / {s.wipLimit}</span>{/if}
              </td>
              <td class="num dim">{s.oldestAgeDays === null ? '—' : `${s.oldestAgeDays} d`}</td>
              <td class="num">{s.flow.kind === 'counted' ? s.flow.arrived : '—'}</td>
              <td class="num" class:stalled={s.flow.kind === 'counted' && s.flow.served === 0}>
                {s.flow.kind === 'counted' ? s.flow.served : '—'}
              </td>
              <td class="num">{netText(s)}</td>
              <td class="num">{clearText(s)}</td>
              <td class="why">{whyNotMoving(s, windowHours)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    <!-- 02 — THE WAITS. Step-level, wall clock, click opens the packet. -->
    <div class="my-section">02 — LONGEST-WAITING OBLIGATIONS</div>
    {#if waits.kind === 'failed'}
      <p class="my-fail">The queue-age lens did not answer: {waits.error}.</p>
    {:else if waits.kind === 'loading'}
      <p class="my-quiet">Reading the obligations…</p>
    {:else if longest.length === 0}
      <p class="my-quiet">Nothing is outstanding.</p>
    {:else}
      <table class="my-table">
        <thead>
          <tr>
            <th class="num">Waiting</th>
            <th>Obligation</th>
            <th>Packet</th>
            <th>On whom</th>
          </tr>
        </thead>
        <tbody>
          {#each longest as w (w.jobId + w.stepTitle)}
            <tr>
              <td class="num">{waitText(w)}</td>
              <td>{w.stepTitle}</td>
              <td>
                <a
                  href={href(`/jobs/${w.jobId}`)}
                  onclick={(e) => {
                    e.preventDefault();
                    openPacket(w.jobId);
                  }}>{w.jobTitle}</a
                >
                <span class="dim mono"> {w.jobKind}</span>
              </td>
              <td class:nobody={w.assigneeId === null}>{w.assigneeId ?? 'nobody'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    <!-- 03 — WHAT THIS PAGE CANNOT COUNT. Stated, not omitted. -->
    {#if blind.length > 0}
      <div class="my-section">03 — RATE NOT COUNTABLE</div>
      <ul class="my-blind">
        {#each blind as s (s.station)}
          <li>
            <span class="mono">{s.station}</span>
            <span class="dim">
              — {s.flow.kind === 'unavailable' ? s.flow.reason : ''}</span
            >
          </li>
        {/each}
      </ul>
    {/if}

    {#if clear.length > 0}
      <div class="my-section">04 — CLEAR</div>
      <p class="my-clear">{clear.map((s) => s.station).join(' · ')}</p>
    {/if}

    <p class="my-footnote">
      Depth and the oldest packet's age come from <span class="mono">/api/stations/load</span>;
      arrivals and departures from <span class="mono">/api/stations/flow</span>, counted over
      the window from the log's own <span class="mono">step.ready</span> /
      <span class="mono">step.done</span> transitions on their WALL-CLOCK write instant — never
      event time, which is sim-authoritative here. "Oldest" is the oldest member packet's age
      since it opened, which over-reports time in this queue; the per-obligation wait below it
      is the step-level figure. Nothing on this page is estimated: a queue whose rate could not
      be counted says so.
    </p>
  {/if}
</div>

<style>
  .my-root { padding: 0 32px 32px; }
  .my-section {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px; letter-spacing: var(--ls-eyebrow, 0.3em);
    color: var(--signal, #5fd4a8); margin: 28px 0 8px;
    display: flex; align-items: center; gap: 12px;
  }
  .my-section::after { content: ''; flex: 1; border-top: 1px solid var(--hairline, #2a3138); }
  .my-quiet { color: var(--static, #7a838c); font-size: 13px; }
  .my-fail {
    color: var(--warn, #d9a441);
    border: 1px solid var(--warn, #d9a441);
    padding: 8px 12px; font-size: 13px;
  }
  .my-controls { display: flex; align-items: center; gap: 6px; margin: 8px 0 0; flex-wrap: wrap; }
  .my-controls-label {
    font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: 0.1em; text-transform: uppercase; color: var(--static, #7a838c);
  }
  .my-controls button {
    font: inherit; font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: 0.1em; text-transform: uppercase; background: transparent;
    color: var(--static, #7a838c); border: 1px solid var(--hairline, #2a3138);
    padding: 3px 10px; cursor: pointer;
  }
  .my-controls button[aria-pressed='true'] {
    color: var(--signal, #5fd4a8); border-color: var(--signal, #5fd4a8);
  }
  .my-asof {
    margin-left: auto; font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px; color: var(--static, #7a838c);
  }
  .my-constraint {
    border: 1px solid var(--hairline, #2a3138); border-left-width: 3px;
    padding: 10px 12px; display: flex; flex-direction: column; gap: 4px;
  }
  .my-constraint.named { border-left-color: var(--err, #d9534f); }
  .my-constraint-name {
    font-family: var(--font-mono, ui-monospace, monospace); font-weight: 600; font-size: 15px;
  }
  .my-constraint-name.muted { color: var(--static, #7a838c); font-weight: 400; }
  .my-constraint-why { color: var(--static, #7a838c); font-size: 13px; }
  .my-table { width: 100%; border-collapse: collapse; font-size: 13px; }
  .my-table th {
    text-align: left; font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase;
    color: var(--static, #7a838c); font-weight: 400;
    border-bottom: 1px solid var(--hairline, #2a3138); padding: 4px 12px 4px 0;
  }
  .my-table td { padding: 6px 12px 6px 0; border-bottom: 1px solid var(--hairline, #2a3138); }
  .my-table th.num, .my-table td.num {
    text-align: right; font-family: var(--font-mono, ui-monospace, monospace);
    font-variant-numeric: tabular-nums; white-space: nowrap;
  }
  .my-table tr.constraint td { background: color-mix(in srgb, var(--err, #d9534f) 8%, transparent); }
  .my-table tbody tr:hover td { background: color-mix(in srgb, currentColor 4%, transparent); }
  .my-table td.stalled { color: var(--err, #d9534f); }
  .my-table td.nobody { color: var(--err, #d9534f); }
  .my-table td.why { color: var(--static, #7a838c); }
  .my-table a { color: inherit; }
  .mono { font-family: var(--font-mono, ui-monospace, monospace); }
  .dim { color: var(--static, #7a838c); }
  .my-blind { list-style: none; padding: 0; margin: 6px 0; display: flex; flex-direction: column; gap: 4px; }
  .my-blind li { font-size: 13px; }
  .my-clear {
    font-family: var(--font-mono, ui-monospace, monospace); font-size: 12px;
    color: var(--static, #7a838c); line-height: 1.7;
  }
  .my-footnote { color: var(--static, #7a838c); font-size: 12px; margin-top: 24px; max-width: 78ch; }
</style>
