<script lang="ts">
  // Live System-Model view — a window into what the tenant is
  // actually doing right now: Jobs in flight (count chips + a
  // recent-jobs feed) plus the per-Workflow step-graph each Job is
  // walking, with a workflow/atlas view toggle.
  //
  // Extracted from LandingPage.svelte so the same live view renders
  // both on the public landing (`/`) and as the System Model
  // perspective root (`/system`). This component owns the live
  // polling + the Workflow registry load; it carries no marketing
  // chrome (hero, CTA) — the host page supplies that.

  import { onMount } from 'svelte';
  import StepDag from '../jobs/StepDag.svelte';
  import { workflowToDag } from '../jobs/workflowToDag';
  import { navigate } from '../router';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import type {
    WorkflowSpec,
    WorkflowStep,
    WorkflowSummary,
    JobLiveSummary,
    JobLiveRow,
  } from './types';
  import {
    atlasLayout as computeAtlasLayout,
    atlasColorFor,
    NODE_W,
    NODE_H,
  } from '../jobs/atlas-layout';

  let kinds = $state<WorkflowSummary[]>([]);
  // Empty until /api/workflows loads, then defaulted to the first
  // kind in the (label-sorted) registry list — no brewery slug
  // baked in. See loadKinds().
  let selectedKind = $state<string>('');
  let spec = $state<WorkflowSpec | null>(null);
  // StepDag nodes + edges derived from the loaded spec.
  let dag = $derived(spec ? workflowToDag(spec.steps) : { nodes: [], edges: [] });
  let renderError = $state<string | null>(null);
  let loading = $state<boolean>(true);
  let activeStep = $state<WorkflowStep | null>(null);

  // View-mode toggle. The default `workflow` view shows the
  // per-Workflow step diagram. The `atlas` view shows the
  // cross-Workflow operating-model overview (every published Workflow
  // grouped by category). Visitors click an atlas node to drill into
  // its workflow.
  type ViewMode = 'workflow' | 'atlas';
  // Default to the workflow view; honor `?atlas` in the URL to
  // open in the System Atlas view instead. The README's
  // `localhost:4443/?atlas` entry-point link relies on this.
  let viewMode = $state<ViewMode>(
    typeof window !== 'undefined' && new URLSearchParams(window.location.search).has('atlas')
      ? 'atlas'
      : 'workflow',
  );
  // Per-kind pulse (kind-wide flash when fresh events of that
  // kind arrive in the event tail). Set per-event in
  // refreshEvents; map<kind → expiry-timestamp-ms>.
  let kindPulseUntil = $state<Record<string, number>>({});
  // Full spec list — populated alongside `kinds` so the atlas
  // view can render step counts without per-kind fetches.
  let allSpecs = $state<WorkflowSpec[]>([]);

  // Live operating-company state. Refreshed once on mount + every
  // 1s after that — turns the static workflow diagram into a
  // window into what the tenant is actually doing right now.
  let live = $state<JobLiveSummary | null>(null);
  let liveError = $state<string | null>(null);
  // Track Job IDs we've already shown so we can flag new arrivals
  // for the slide-in animation. Set-based so the lookup is O(1)
  // per render.
  let seenJobIds = $state<Set<string>>(new Set());
  // Per-kind count snapshot from the prior poll. Powers the pulse
  // animation when a count chip's value changes between polls.
  let priorCounts = $state<Record<string, number>>({});

  type CountRow = { kind: string; count: number; changed: boolean };
  let countRows = $derived<CountRow[]>(
    live
      ? Object.keys(live.counts)
          .sort()
          .map((k) => ({
            kind: k,
            count: live!.counts[k] ?? 0,
            changed: priorCounts[k] !== undefined && priorCounts[k] !== (live!.counts[k] ?? 0),
          }))
      : [],
  );

  // Open jobs of the selected kind, fetched server-side so the
  // right-panel list shows the FULL set for that kind — not just
  // the small `live.recent` sample. The count on the left chip
  // is the server total; the list on the right paginates up to
  // RIGHT_PANEL_LIMIT of those, so the two are reconcilable
  // (chip = total, list = "showing first N").
  const RIGHT_PANEL_LIMIT = 50;
  let filteredRecent = $state<ReadonlyArray<JobLiveRow>>([]);
  let filteredRecentError = $state<string | null>(null);
  let kindTotal = $derived<number>(live ? (live.counts[selectedKind] ?? 0) : 0);

  async function loadFilteredJobs(kind: string): Promise<void> {
    filteredRecentError = null;
    try {
      const r = await fetch(
        `/api/jobs?status=open&kind=${encodeURIComponent(kind)}&limit=${RIGHT_PANEL_LIMIT}`,
      );
      if (!r.ok) {
        filteredRecentError = `HTTP ${r.status}`;
        return;
      }
      const body = (await r.json()) as { data?: ReadonlyArray<JobLiveRow> };
      filteredRecent = body.data ?? [];
    } catch (e) {
      filteredRecentError = e instanceof Error ? e.message : String(e);
    }
  }

  $effect(() => {
    void loadFilteredJobs(selectedKind);
  });

  async function refreshLive() {
    try {
      const r = await fetch('/api/jobs/live');
      if (!r.ok) {
        throw new Error(`GET /api/jobs/live: ${r.status}`);
      }
      const next = await r.json() as JobLiveSummary;
      // Snapshot the prior count map BEFORE swapping `live` so the
      // derived `countRows.changed` flag flips for one render cycle
      // when a chip's value moves. Same pattern for `seenJobIds` —
      // recompute on each poll so the slide-in only fires for
      // genuinely new IDs.
      if (live) {
        priorCounts = { ...live.counts };
      }
      const previouslySeen = new Set(seenJobIds);
      seenJobIds = new Set(next.recent.map((j) => j.id));
      // For the next render we want to highlight Job IDs that JUST
      // arrived (in `next` but not in `previouslySeen`). Stash the
      // delta on `live` via a derived; cheaper than mutating the
      // payload.
      live = next;
      // Briefly mark the changed counts so CSS picks up the pulse
      // class for one render, then clear so a re-poll without
      // changes doesn't re-pulse.
      window.setTimeout(() => { priorCounts = { ...next.counts }; }, 800);
      // Same for newly-arrived job IDs — clear `previouslySeen`
      // tracking after the slide-in completes (~600ms).
      window.setTimeout(() => {
        // The derived `isNew` reads `seenJobIds` minus a fresh
        // baseline; we mark all current rows as seen by leaving
        // `seenJobIds` alone (it already holds `next.recent`).
        // No-op timeout; the seenJobIds state is already correct.
        void previouslySeen;
      }, 600);
      // Stash the previously-seen set on a $state so the template
      // can compare. (Svelte 5 reads through the closure cleanly.)
      _previouslySeen = previouslySeen;
      liveError = null;
    } catch (e) {
      liveError = e instanceof Error ? e.message : String(e);
    }
  }
  // Companion state used by the recent-jobs render to flag new
  // arrivals between polls. Reset to the prior seenJobIds set
  // each time we poll; rendered rows whose id is NOT in this set
  // get the `.is-new` class (slide-in animation).
  let _previouslySeen = $state<Set<string>>(new Set());


  async function loadKinds() {
    try {
      const r = await fetch('/api/workflows');
      if (!r.ok) {
        throw new Error(`GET /api/workflows: ${r.status}`);
      }
      const all: WorkflowSpec[] = await r.json();
      // Stash full specs for the atlas view (step counts,
      // per-category grouping). The picker + summary list still
      // use the trimmed WorkflowSummary shape.
      allSpecs = all;
      kinds = all
        .map((k) => ({ kind: k.kind, label: k.label, category: k.category }))
        .sort((a, b) => a.label.localeCompare(b.label));
      // Default the picker to the first kind in the registry list
      // (label-sorted above). Also re-runs if a previously-selected
      // kind is no longer published.
      if (!kinds.some((k) => k.kind === selectedKind)) {
        selectedKind = kinds[0]?.kind ?? selectedKind;
      }
    } catch (e) {
      renderError = `Couldn't load Workflows: ${e instanceof Error ? e.message : String(e)}`;
    }
  }

  // ===== Atlas view. Groups Workflows by category and lays them out
  // as clickable SVG nodes; a visitor clicks a node to drill into its
  // per-Workflow workflow view. The layout + colours are the shared
  // engine (src/jobs/atlas-layout.ts), also used by the System Atlas
  // page — only this canvas's narrower width differs.
  const ATLAS_CANVAS_W = 1100;

  let atlasLayout = $derived.by(() =>
    allSpecs.length === 0 ? null : computeAtlasLayout(allSpecs, ATLAS_CANVAS_W),
  );

  /** Atlas node click — drill into the workflow view for that kind. */
  function atlasOpenWorkflow(kind: string) {
    selectedKind = kind;
    viewMode = 'workflow';
  }

  /** True if `kind` is currently pulsing (live event landed
   *  for that Workflow in the last ~600ms). Atlas nodes pulse
   *  when their kind sees fresh activity. */
  function atlasNodePulsing(kind: string): boolean {
    const until = kindPulseUntil[kind];
    return typeof until === 'number' && until > Date.now();
  }

  async function loadSpec(kind: string) {
    activeStep = null;
    loading = true;
    renderError = null;
    try {
      const r = await fetch(`/api/workflows/${encodeURIComponent(kind)}`);
      if (!r.ok) {
        throw new Error(`GET /api/workflows/${kind}: ${r.status}`);
      }
      spec = await r.json() as WorkflowSpec;
    } catch (e) {
      renderError = `Couldn't load workflow: ${e instanceof Error ? e.message : String(e)}`;
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    void (async () => {
      await Promise.all([loadKinds(), refreshLive()]);
      await loadSpec(selectedKind);
    })();
    // Refresh the recent-jobs panel + count chips on a 1s tick.
    // The sim daemon advances at hourly granularity (~0.42s real
    // per sim-hour at the 1-sim-year-per-real-hour budget), so 1s
    // catches every ~2.4 sim-hours of activity — close to "live"
    // without thrashing the gateway. The endpoint reads a small
    // recent-jobs window from the projection so the per-poll cost
    // is fixed regardless of total log size.
    const handle = window.setInterval(() => {
      void refreshLive();
    }, 1_000);
    return () => window.clearInterval(handle);
  });

  $effect(() => {
    // When the picker changes, re-fetch + re-render.
    if (selectedKind && spec?.kind !== selectedKind) {
      void loadSpec(selectedKind);
    }
  });
</script>

<div class="live-view">
  {#if live}
    <section class="live-panel">
      <div class="live-summary">
        <div class="live-stat">
          <span class="live-stat-num">{live.open_total}</span>
          <span class="live-stat-label">jobs in flight</span>
        </div>
        <ul class="live-counts">
          {#each countRows as row (row.kind)}
            <li>
              <button
                type="button"
                class="live-count"
                class:active={row.kind === selectedKind}
                class:pulsing={row.changed}
                onclick={() => { selectedKind = row.kind; }}
              >
                <span class="kind-name">{row.kind}</span>
                <span class="kind-count">{row.count}</span>
              </button>
            </li>
          {/each}
        </ul>
      </div>
      <div class="live-feed">
        <h2>
          Open <code>{selectedKind}</code> jobs · most-recent first
          {#if kindTotal > filteredRecent.length}
            <span class="live-feed-subtitle">showing {filteredRecent.length} of {kindTotal}</span>
          {/if}
        </h2>
        {#if filteredRecentError}
          <p class="empty">Couldn't load filtered jobs: {filteredRecentError}</p>
        {:else if filteredRecent.length === 0}
          <p class="empty">
            No open {selectedKind} jobs right now.
          </p>
        {:else}
          <ul class="live-job-list">
            {#each filteredRecent as job (job.id)}
              <li
                class="live-job-row"
                class:is-new={!_previouslySeen.has(job.id)}
              >
                <a
                  class="live-job"
                  href={entityHref('job', job.id)}
                  onclick={(e) => {
                    e.preventDefault();
                    navigate(entityHref('job', job.id));
                  }}
                  title="Open job {job.id}"
                >
                  <span class="live-workflow">{job.kind}</span>
                  <span class="live-job-title">{job.title}</span>
                  <span class="live-job-meta">
                    on <code>{job.subject_kind}:{job.subject_id}</code> · opened {job.opened_on}
                  </span>
                </a>
              </li>
            {/each}
          </ul>
        {/if}
      </div>
    </section>
  {:else if liveError}
    <p class="status error">Couldn't load live state: {liveError}</p>
  {/if}

  <div class="picker-row">
    <div class="view-toggle" role="tablist" aria-label="View mode">
      <button
        type="button"
        role="tab"
        class="view-toggle-btn"
        class:active={viewMode === 'workflow'}
        aria-selected={viewMode === 'workflow'}
        onclick={() => { viewMode = 'workflow'; }}
      >Workflow</button>
      <button
        type="button"
        role="tab"
        class="view-toggle-btn"
        class:active={viewMode === 'atlas'}
        aria-selected={viewMode === 'atlas'}
        onclick={() => { viewMode = 'atlas'; }}
      >Atlas</button>
    </div>
    {#if viewMode === 'workflow'}
      <label for="kind-picker">Workflow:</label>
      <select id="kind-picker" bind:value={selectedKind}>
        {#each kinds as k (k.kind)}
          <option value={k.kind}>{k.label}</option>
        {/each}
      </select>
      {#if spec}
        <span class="kind-meta">
          category <em>{spec.category}</em> · subjects <em>{spec.subject_kinds.join(', ')}</em>
        </span>
      {/if}
    {:else}
      <span class="kind-meta">
        Every Workflow this tenant publishes, grouped by category. Click any to drill into its workflow.
      </span>
    {/if}
  </div>

  <div class="content" class:atlas-mode={viewMode === 'atlas'}>
    <div class="graph-wrap">
      {#if viewMode === 'workflow'}
        {#if loading}
          <p class="status">Loading…</p>
        {:else if renderError}
          <p class="status error">{renderError}</p>
        {:else}
          <div id="landing-graph" class="graph">
            <StepDag
              nodes={dag.nodes}
              edges={dag.edges}
              selectedId={activeStep?.title ?? null}
              onNodeClick={(slug) => {
                activeStep = spec?.steps.find((s) => s.title === slug) ?? null;
              }}
            />
          </div>
        {/if}
      {:else if atlasLayout}
        <div class="atlas-canvas-wrap">
          <svg
            viewBox={`0 0 ${ATLAS_CANVAS_W} ${atlasLayout.height}`}
            class="atlas-canvas"
            role="img"
            aria-label="Tenant Workflow atlas"
            preserveAspectRatio="xMinYMid meet"
          >
            {#each atlasLayout.rows as row (row.category)}
              <text
                x={20}
                y={row.y + NODE_H / 2 + 4}
                class="atlas-track-label"
              >{row.category}</text>
              {#each row.nodes as n (n.kind)}
                {@const c = atlasColorFor(n.category)}
                <g
                  class="atlas-node"
                  class:is-pulsing={atlasNodePulsing(n.kind)}
                  transform={`translate(${n.x}, ${n.y})`}
                  onclick={() => atlasOpenWorkflow(n.kind)}
                  role="button"
                  tabindex="0"
                  aria-label={`Open ${n.label} workflow`}
                  onkeydown={(e) => {
                    if (e.key === 'Enter' || e.key === ' ') {
                      e.preventDefault();
                      atlasOpenWorkflow(n.kind);
                    }
                  }}
                >
                  <rect
                    width={NODE_W}
                    height={NODE_H}
                    rx="6"
                    ry="6"
                    style={`fill: ${c.fill}; stroke: ${c.stroke}; stroke-width: 1.5`}
                  />
                  <text
                    class="atlas-node-label"
                    x={NODE_W / 2}
                    y={26}
                    text-anchor="middle"
                  >{n.label}</text>
                  <text
                    class="atlas-node-sub"
                    x={NODE_W / 2}
                    y={48}
                    text-anchor="middle"
                  >{n.step_count} step{n.step_count === 1 ? '' : 's'}</text>
                </g>
              {/each}
            {/each}
          </svg>
        </div>
      {:else}
        <p class="status">Loading atlas…</p>
      {/if}
    </div>

    {#if viewMode === 'workflow'}
    <aside class="side-panel">
      {#if activeStep}
        <h2>Step: <code>{activeStep.title}</code></h2>
        <p class="title-template"><code>{activeStep.kind}</code></p>
        {#if activeStep.title_template}
          <p class="title-template">{activeStep.title_template}</p>
        {/if}
        {#if activeStep.ready_when}
          <p class="badge">ready_when <code>{activeStep.ready_when}</code></p>
        {/if}
        {#if activeStep.terminal}
          <p class="badge">terminal → <code>{activeStep.terminal.outcome}</code></p>
        {/if}
        {#if activeStep.sign_offs_required && activeStep.sign_offs_required.length > 0}
          <p class="badge">requires sign-off — role{activeStep.sign_offs_required.length > 1 ? 's' : ''}
            <code>{activeStep.sign_offs_required.join(', ')}</code>
          </p>
        {/if}
        {#if activeStep.metadata_defaults && Object.keys(activeStep.metadata_defaults).length > 0}
          <h3>metadata defaults</h3>
          <pre>{JSON.stringify(activeStep.metadata_defaults, null, 2)}</pre>
        {:else}
          <p class="empty">No metadata defaults.</p>
        {/if}
      {:else}
        <p class="empty">Click any step to see its typed metadata schema.</p>
      {/if}
    </aside>
    {/if}
  </div>

  {#if spec?.description}
    <section class="description">
      <h2>About this Workflow</h2>
      <p>{spec.description}</p>
    </section>
  {/if}
</div>

<style>
  .live-view {
    color: #2a1d10;
    font-family: var(--font-display, 'Iowan Old Style', 'Palatino Linotype', Georgia, serif);
  }
  .live-panel {
    display: grid;
    grid-template-columns: minmax(260px, 360px) 1fr;
    gap: 1.5rem;
    margin: 0 0 1rem;
    padding: 1rem;
    background: var(--brew-amber-bg, #fff7e0);
    border: 1px solid var(--brew-amber, #d99b3a);
    border-radius: 8px;
  }
  .live-summary {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .live-stat {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
  }
  .live-stat-num {
    font-family: var(--font-display, 'Fraunces', Georgia, serif);
    font-size: 2.5rem;
    font-weight: 700;
    color: var(--brew-malt-dark, #4a2510);
    line-height: 1;
  }
  .live-stat-label {
    font-size: 0.85rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--brew-malt, #7a3f1f);
  }
  .live-counts {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .live-count {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    width: 100%;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 4px;
    padding: 0.3rem 0.55rem;
    cursor: pointer;
    font: inherit;
    color: #2a1d10;
    transition: background 80ms;
  }
  .live-count:hover {
    background: rgba(217, 155, 58, 0.18);
  }
  .live-count.active {
    background: var(--brew-amber-soft, #f4d790);
    border-color: var(--brew-amber, #d99b3a);
    font-weight: 600;
  }
  .live-count .kind-name {
    font-family: 'Iowa', 'Iowan Old Style', Georgia, serif;
    font-style: italic;
  }
  .live-count .kind-count {
    background: var(--brew-malt, #7a3f1f);
    color: #fff;
    padding: 0.05em 0.5em;
    border-radius: 99px;
    font-size: 0.8rem;
    font-weight: 600;
    transition: background 200ms ease, transform 200ms ease;
  }
  /* Pulse the count chip on every tick that changes its value.
     The .pulsing class flips for one render cycle (~800ms) when
     a count moves between polls; pairs with the 1-second poll
     cadence so the visual reads as "the brewery just did a thing." */
  .live-count.pulsing .kind-count {
    background: var(--brew-amber, #d99b3a);
    transform: scale(1.18);
  }
  /* Slide-in animation for genuinely new Job rows. The
     `_previouslySeen` set in the script gates this — only Jobs
     that arrived in the latest poll get .is-new. Roughly 600ms
     so the visual reads as "this just appeared," then the row
     settles into the static layout. */
  .live-job-row.is-new .live-job {
    animation: live-job-arrive 600ms ease-out;
  }
  @keyframes live-job-arrive {
    0% {
      opacity: 0;
      transform: translateX(-8px);
      border-color: var(--brew-amber, #d99b3a);
      background: var(--brew-amber-bg, #fff7e0);
    }
    60% {
      opacity: 1;
      transform: translateX(0);
    }
    100% {
      opacity: 1;
      transform: translateX(0);
      border-color: #e6d2a8;
      background: #fffaf0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .live-count.pulsing .kind-count { transform: none; transition: none; }
    .live-job-row.is-new .live-job { animation: none; }
  }
  .live-feed h2 {
    font-family: var(--font-display, 'Fraunces', Georgia, serif);
    font-size: 1rem;
    margin: 0 0 0.5rem;
    color: var(--brew-malt-dark, #4a2510);
  }
  .live-job-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-height: 280px;
    overflow-y: auto;
  }
  .live-job {
    display: grid;
    grid-template-columns: 8.5em 1fr;
    grid-template-rows: auto auto;
    gap: 0.1rem 0.6rem;
    width: 100%;
    background: #fffaf0;
    border: 1px solid #e6d2a8;
    border-radius: 4px;
    padding: 0.45rem 0.6rem;
    cursor: pointer;
    text-align: left;
    color: #2a1d10;
    text-decoration: none;
    font: inherit;
  }
  .live-job:hover { border-color: var(--brew-amber, #d99b3a); }
  .live-workflow {
    grid-row: 1 / 3;
    align-self: center;
    font-family: 'Iowa', 'Iowan Old Style', Georgia, serif;
    font-style: italic;
    color: var(--brew-malt, #7a3f1f);
    font-size: 0.85rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .live-job-title {
    font-weight: 600;
    line-height: 1.25;
  }
  .live-job-meta {
    font-size: 0.78rem;
    color: #7a6855;
  }
  .live-job-meta code {
    background: #fff8e9;
    padding: 0 0.25em;
    border-radius: 3px;
    font-size: 0.78rem;
  }
  @media (max-width: 760px) {
    .live-panel {
      grid-template-columns: 1fr;
    }
  }
  .picker-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    margin: 1.5rem 0 0.75rem;
    font-size: 0.95rem;
  }
  .picker-row select {
    padding: 0.35rem 0.5rem;
    border: 1px solid #c5a880;
    border-radius: 4px;
    background: #fff8e9;
    font: inherit;
  }
  .kind-meta {
    color: #7a6855;
    font-size: 0.85rem;
  }
  .kind-meta em {
    font-style: normal;
    color: #2a1d10;
  }
  .content {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 280px;
    gap: 1.5rem;
    align-items: start;
    margin-top: 0.5rem;
  }
  .graph-wrap {
    background: #fff8e9;
    border: 1px solid #c5a880;
    border-radius: 6px;
    padding: 1rem;
    min-height: 300px;
  }
  .graph :global(svg) {
    width: 100%;
    height: auto;
    cursor: pointer;
  }
  .graph :global(.step-title) {
    display: inline-block;
    font-size: 0.78em;
    color: #4a392b;
    font-style: italic;
  }
  .status {
    margin: 0;
    color: #7a6855;
    font-style: italic;
  }
  .status.error {
    color: #8b2b1f;
    font-style: normal;
  }
  .side-panel {
    background: #f4ead2;
    border: 1px solid #c5a880;
    border-radius: 6px;
    padding: 1rem;
    font-family: -apple-system, system-ui, sans-serif;
    font-size: 0.9rem;
    position: sticky;
    top: 1rem;
  }
  .side-panel h2 {
    margin: 0 0 0.5rem;
    font-size: 1rem;
  }
  .side-panel h3 {
    margin: 0.75rem 0 0.25rem;
    font-size: 0.8rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: #7a3f1f;
  }
  .side-panel code {
    background: #fff8e9;
    padding: 0.05em 0.3em;
    border-radius: 3px;
  }
  .side-panel pre {
    background: #fff8e9;
    padding: 0.5rem;
    border-radius: 4px;
    overflow-x: auto;
    font-size: 0.78rem;
    margin: 0.25rem 0 0;
  }
  .badge {
    display: inline-block;
    background: #f8d8a4;
    border: 1px solid #c5a880;
    border-radius: 4px;
    padding: 0.1rem 0.5rem;
    font-size: 0.78rem;
    margin: 0.25rem 0;
  }
  .empty, .title-template {
    color: #7a6855;
    margin: 0.25rem 0;
  }
  /* View-mode toggle. Two-pill button group lifted into the picker
     row; the active mode carries the brewery-amber background for
     visual continuity with the live-count chips above. */
  .view-toggle {
    display: inline-flex;
    border: 1px solid #c5a880;
    border-radius: 999px;
    overflow: hidden;
    margin-right: 0.5rem;
  }
  .view-toggle-btn {
    padding: 0.3rem 0.85rem;
    background: #fff8e9;
    border: none;
    cursor: pointer;
    font: inherit;
    font-size: 0.85rem;
    color: var(--brew-malt, #7a3f1f);
  }
  .view-toggle-btn:hover { background: rgba(217, 155, 58, 0.18); }
  .view-toggle-btn.active {
    background: var(--brew-amber, #d99b3a);
    color: #fff;
    font-weight: 600;
  }

  /* Atlas mode collapses the side-panel column so the canvas can
     stretch full-width. The workflow mode keeps the original
     1fr/280px split. */
  .content.atlas-mode {
    grid-template-columns: minmax(0, 1fr);
  }

  /* Atlas SVG canvas — lifted from
     apps/web/src/it/monitoring/AtlasPage.svelte. Same layout
     constants in the script side; the atlas-* CSS classes here
     mirror the AtlasPage styles. */
  .atlas-canvas-wrap {
    overflow-x: auto;
    padding: 8px 0;
  }
  .atlas-canvas {
    width: 100%;
    min-width: 900px;
    height: auto;
    display: block;
  }
  .atlas-track-label {
    font-size: 13px;
    font-weight: 600;
    fill: var(--brew-malt, #7a3f1f);
    text-transform: uppercase;
    letter-spacing: 0.08em;
    font-family: 'Iowa', 'Iowan Old Style', Georgia, serif;
  }
  .atlas-node {
    cursor: pointer;
    transition: transform 200ms ease;
  }
  .atlas-node:hover rect {
    stroke-width: 2.5px;
  }
  .atlas-node:focus { outline: none; }
  .atlas-node:focus rect {
    stroke-width: 2.5px;
  }
  .atlas-node-label {
    font-size: 13px;
    font-weight: 600;
    fill: #2a1d10;
    font-family: -apple-system, system-ui, sans-serif;
  }
  .atlas-node-sub {
    font-size: 11px;
    fill: #7a6855;
    font-family: -apple-system, system-ui, sans-serif;
  }
  /* Atlas node pulse — flashes when fresh job lifecycle events
     for that Workflow land in the event tail. The amber stroke
     + scale lift reads as "this kind just did something."
     Companion to the per-step-node pulse in workflow mode. */
  .atlas-node.is-pulsing rect {
    stroke: var(--brew-amber, #d99b3a) !important;
    stroke-width: 3px !important;
    filter: drop-shadow(0 0 6px rgba(217, 155, 58, 0.6));
  }
  .atlas-node.is-pulsing {
    animation: atlas-node-pulse 600ms ease-out;
    transform-origin: center;
    transform-box: fill-box;
  }
  @keyframes atlas-node-pulse {
    0%   { transform: scale(1); }
    35%  { transform: scale(1.05); }
    100% { transform: scale(1); }
  }
  @media (prefers-reduced-motion: reduce) {
    .atlas-node { transition: none; }
    .atlas-node.is-pulsing { animation: none; }
  }

  .description {
    margin-top: 1.75rem;
    background: #fff8e9;
    border-left: 3px solid #7a3f1f;
    padding: 0.5rem 1rem;
    font-family: -apple-system, system-ui, sans-serif;
    font-size: 0.9rem;
    color: #4a392b;
    white-space: pre-line;
  }
  .description h2 {
    margin: 0 0 0.5rem;
    font-size: 1rem;
    color: #2a1d10;
  }
  @media (max-width: 760px) {
    .content {
      grid-template-columns: 1fr;
    }
    .side-panel {
      position: static;
    }
  }
</style>
