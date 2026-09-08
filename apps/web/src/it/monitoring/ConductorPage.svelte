<script lang="ts">
  // /it/operate/conductor — a live, readable feed of the Conductor's
  // pipeline actions.
  //
  // The Conductor (the `boss train` reconcile loop) works in the
  // background while an operator waits: it boards PR-trains, waits on
  // CI, merges, deploys, and confirms the cluster converged. This page
  // is the window into that activity — a plain-English timeline, newest
  // first, so the wait is legible instead of silent.
  //
  // It is a thin lens over the packets the conductor already writes:
  // the whole read-model is the pure `conductorTimeline` function
  // (conductorActivity.ts, tested); this file fetches, polls, and
  // renders. No new backend — the same `/api/jobs?kind=pr-train` reads
  // the Yard uses. Times are Pacific (the stack stores UTC; David reads
  // PT), converted in the tested pure helpers.
  //
  // Increment 1 of the operations-console initiative.
  import { onMount } from 'svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import {
    conductorTimeline,
    type TimelineAction,
    type TimelineEntry,
  } from './conductorActivity';
  import type { JobLite } from '../yard/yard';

  // The Yard's cadence (YardPage.svelte): a 10s poll. Depth-style
  // aggregates a single event does not unambiguously update, so we
  // re-fetch rather than stream.
  const POLL_MS = 10_000;

  type State =
    | { kind: 'loading' }
    | { kind: 'ready'; entries: readonly TimelineEntry[] }
    | { kind: 'error'; message: string };

  let loadState: State = $state<State>({ kind: 'loading' });
  let refreshing = $state(false);
  let lastUpdated = $state<number | null>(null);
  // A once-a-second tick so "updated Ns ago" stays honest between polls.
  let now = $state(Date.now());

  const agoSec = $derived(
    lastUpdated === null ? null : Math.max(0, Math.round((now - lastUpdated) / 1000)),
  );

  type JobsEnvelope = { data?: JobLite[] };

  // The ship-a-change packets, read best-effort to NAME the cars a
  // train boarded (their branch). It is additive: if this read fails,
  // the boarded rows fall back to a plain count and the page is whole.
  async function carNamesFrom(
    r: Response | null,
  ): Promise<ReadonlyMap<string, string> | undefined> {
    if (!r || !r.ok) return undefined;
    try {
      const ships = ((await r.json()) as JobsEnvelope).data ?? [];
      if (!Array.isArray(ships)) return undefined;
      const m = new Map<string, string>();
      for (const s of ships) {
        const branch = (s.metadata as { branch?: unknown } | null)?.branch;
        m.set(s.id, typeof branch === 'string' && branch !== '' ? branch : s.title);
      }
      return m;
    } catch {
      return undefined;
    }
  }

  // One fetch cycle → the timeline, or a throw the caller renders as
  // the failed state. The pr-train read is load-bearing; the car names
  // are additive.
  async function fetchTimeline(): Promise<readonly TimelineEntry[]> {
    const [tr, sr] = await Promise.all([
      fetch('/api/jobs?kind=pr-train&limit=25'),
      fetch('/api/jobs?kind=ship-a-change&limit=200').catch(() => null),
    ]);
    if (!tr.ok) throw new Error(`pr-train jobs: HTTP ${tr.status}`);
    const raw = ((await tr.json()) as JobsEnvelope).data ?? [];
    const trains = Array.isArray(raw) ? raw : [];
    return conductorTimeline(trains, await carNamesFrom(sr));
  }

  onMount(() => {
    let cancelled = false;
    async function tick(): Promise<void> {
      if (cancelled) return;
      refreshing = true;
      try {
        const entries = await fetchTimeline();
        if (!cancelled) {
          loadState = { kind: 'ready', entries };
          lastUpdated = Date.now();
        }
      } catch (e) {
        if (!cancelled) {
          loadState = { kind: 'error', message: e instanceof Error ? e.message : String(e) };
        }
      } finally {
        if (!cancelled) refreshing = false;
      }
    }
    void tick();
    const poll = setInterval(tick, POLL_MS);
    const clock = setInterval(() => {
      now = Date.now();
    }, 1000);
    return () => {
      cancelled = true;
      clearInterval(poll);
      clearInterval(clock);
    };
  });

  // Entries arrive newest-first; walk them into consecutive PT-date
  // runs so the feed carries date separators without a second sort.
  type DateGroup = Readonly<{ date: string; entries: TimelineEntry[] }>;
  const groups = $derived.by<readonly DateGroup[]>(() => {
    if (loadState.kind !== 'ready') return [];
    const out: DateGroup[] = [];
    for (const e of loadState.entries) {
      const last = out[out.length - 1];
      if (last && last.date === e.datePt) last.entries.push(e);
      else out.push({ date: e.datePt, entries: [e] });
    }
    return out;
  });

  const trainLabel = (e: TimelineEntry): string =>
    e.prNumber !== null ? `Train #${e.prNumber}` : `Train ${e.trainId.slice(0, 8)}`;

  // A small status dot per action: green for forward progress, red for
  // a stop (CI red / cancelled), neutral for the in-between beats. An
  // inline color keeps the dynamic value out of the scoped stylesheet.
  function dotColor(action: TimelineAction): string {
    switch (action) {
      case 'ci-green':
      case 'merged':
      case 'deployed':
      case 'converged':
        return 'var(--ok, #4fb98a)';
      case 'ci-failed':
      case 'cancelled':
        return 'var(--err, #e2685c)';
      case 'boarded':
      case 'ci-verdict':
        return 'var(--static, #7a838c)';
    }
  }

  const updatedText = $derived(
    agoSec === null
      ? 'loading…'
      : refreshing
        ? 'refreshing…'
        : agoSec < 2
          ? 'updated just now'
          : `updated ${agoSec}s ago`,
  );
</script>

<div class="theme-exec conductor-root">
  <div class="exec-header">
    <div>
      <div class="exec-eyebrow">Operations · Conductor</div>
      <h1 class="exec-title">Conductor activity</h1>
      <p class="conductor-lede">
        What the delivery Conductor has been doing — boarding trains,
        waiting on CI, merging, deploying, confirming convergence. Live,
        newest first, in Pacific time.
      </p>
    </div>
    <div class="conductor-status" aria-live="polite">
      <span class="conductor-pulse" class:live={refreshing}></span>
      {updatedText}
    </div>
  </div>

  {#if loadState.kind === 'loading'}
    <p class="conductor-msg">Loading the Conductor's recent activity…</p>
  {:else if loadState.kind === 'error'}
    <p class="conductor-msg conductor-err">
      Could not load Conductor activity — {loadState.message}
    </p>
  {:else if loadState.entries.length === 0}
    <p class="conductor-msg">
      No recent Conductor activity. Trains that board, merge, or deploy
      will appear here as it happens.
    </p>
  {:else}
    <div class="conductor-feed">
      {#each groups as g (g.date)}
        <div class="conductor-date" role="separator">{g.date}</div>
        {#each g.entries as e (e.key)}
          <div class="conductor-row">
            <time class="conductor-time" title={e.at}>{e.timePt}</time>
            <span
              class="conductor-dot"
              style="background: {dotColor(e.action)}"
              aria-hidden="true"
            ></span>
            <div class="conductor-body">
              <div class="conductor-line">
                <a class="conductor-train" href={entityHref('job', e.trainId)}>
                  {trainLabel(e)}
                </a>
                <span class="conductor-action">{e.label}</span>
                {#if e.detail}<span class="conductor-detail">{e.detail}</span>{/if}
                {#if e.prUrl}
                  <a
                    class="conductor-pr"
                    href={e.prUrl}
                    target="_blank"
                    rel="noreferrer"
                    title="open the PR on the forge">PR ↗</a
                  >
                {/if}
              </div>
              <div class="conductor-title" title={e.trainTitle}>{e.trainTitle}</div>
            </div>
          </div>
        {/each}
      {/each}
    </div>
  {/if}
</div>

<style>
  .conductor-root {
    padding: 32px;
    max-width: 960px;
    margin: 0 auto;
  }
  .conductor-lede {
    color: var(--static, #7a838c);
    margin: 8px 0 0;
    max-width: 52ch;
    font-size: 14px;
    line-height: 1.5;
  }
  .conductor-status {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: var(--static, #7a838c);
    white-space: nowrap;
  }
  .conductor-pulse {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--static, #7a838c);
    opacity: 0.5;
  }
  .conductor-pulse.live {
    background: var(--ok, #4fb98a);
    opacity: 1;
  }
  .conductor-msg {
    color: var(--static, #7a838c);
    margin: 24px 0;
    font-size: 14px;
  }
  .conductor-err {
    color: var(--err, #e2685c);
  }
  .conductor-feed {
    margin-top: 8px;
  }
  .conductor-date {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--static, #7a838c);
    margin: 20px 0 6px;
    padding-bottom: 4px;
    border-bottom: 1px solid var(--hairline, #2a3138);
  }
  .conductor-row {
    display: grid;
    grid-template-columns: 68px 12px 1fr;
    align-items: baseline;
    gap: 10px;
    padding: 8px 0;
    border-bottom: 1px solid var(--hairline, #2a3138);
  }
  .conductor-time {
    font-variant-numeric: tabular-nums;
    font-size: 12px;
    color: var(--static, #7a838c);
    text-align: right;
  }
  .conductor-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    align-self: center;
    background: var(--static, #7a838c);
  }
  .conductor-body {
    min-width: 0;
  }
  .conductor-line {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 8px;
    font-size: 14px;
  }
  .conductor-train {
    font-weight: 600;
    color: var(--text, inherit);
    text-decoration: none;
  }
  .conductor-train:hover {
    text-decoration: underline;
  }
  .conductor-action {
    color: var(--text, inherit);
  }
  .conductor-detail {
    font-variant-numeric: tabular-nums;
    color: var(--static, #7a838c);
    font-size: 13px;
  }
  .conductor-pr {
    font-size: 12px;
    color: var(--static, #7a838c);
    text-decoration: none;
  }
  .conductor-pr:hover {
    text-decoration: underline;
  }
  .conductor-title {
    color: var(--static, #7a838c);
    font-size: 12px;
    margin-top: 2px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
