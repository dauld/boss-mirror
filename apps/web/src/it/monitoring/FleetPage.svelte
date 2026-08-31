<script lang="ts">
  // /it/operate/bottlenecks — every in-flight Job of one Workflow kind,
  // projected onto the Workflow's DAG.
  //
  // The Job page answers "where is THIS Job"; this page answers
  // "where is EVERYTHING of this kind": per-step depth (ready /
  // active), the unassigned claimable pool, which authority-role lens
  // each pile belongs to, and how long the oldest wait has run. A hot
  // node is a deep queue — the algedonic depth signal from
  // queue-visibility Q4 drawn on the map (feedback 9fe2fe66,
  // change 1; thresholds/telemetry are change 2, gated on Q4).
  //
  // Polls on a 10s interval, per the SSE policy's bucket (b): depth
  // is an aggregate a single event does not unambiguously update, so
  // it re-fetches rather than streaming.
  //
  // Steps that do not match the current spec's slugs — pre-migration
  // slug-less rows grouped by title, or steps from older Workflow
  // versions — render in the off-map table below the DAG rather than
  // silently vanishing (the server's COALESCE contract; see
  // boss-views/src/fleet.rs).
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import StepDag, { type DagNode } from '../../jobs/StepDag.svelte';
  import { workflowToDag } from '../../jobs/workflowToDag';
  import { groupByPosition } from '../../jobs/position';
  import { decorateDagNodes, fmtDur } from '../../jobs/decorateDag';
  import type { Job } from '../../jobs/types';
  import { navigate } from '../../router';

  type FleetNode = Readonly<{
    slug: string;
    ready: number;
    active: number;
    unassigned: number;
    by_role: Readonly<Record<string, number>>;
    oldest_ready_wall: string | null;
  }>;
  type Fleet = Readonly<{
    workflow_kind: string;
    open_jobs: number;
    nodes: ReadonlyArray<FleetNode>;
    as_of: string;
  }>;
  type StageStat = Readonly<{
    slug: string;
    completed: number;
    p50_seconds: number;
    p90_seconds: number;
    max_seconds: number;
  }>;
  type SpecStep = Readonly<{
    title: string;
    kind: string;
    ready_when?: string;
    title_template?: string | null;
    terminal?: { outcome: string } | null;
  }>;

  const POLL_MS = 10_000;

  let kinds = $state<ReadonlyArray<string>>([]);
  let kind = $state<string | null>(null);
  let specSteps = $state<ReadonlyArray<SpecStep> | null>(null);
  let fleet = $state<Fleet | null>(null);
  let jobs = $state<ReadonlyArray<Job>>([]);
  let stageStats = $state<ReadonlyArray<StageStat>>([]);
  let selectedNode = $state<string | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Decode once, defensively, at the fetch site — the route-smoke
  // crawl runs every page against an adversarial mock, and a
  // non-array where an array belongs must render as an empty state,
  // not a runtime crash.
  async function loadKinds(): Promise<void> {
    const res = await fetch('/api/workflows');
    if (!res.ok) throw new Error(`workflows: HTTP ${res.status}`);
    const rows: unknown = await res.json();
    kinds = Array.isArray(rows)
      ? [...new Set(rows.map((r) => r?.kind).filter((k) => typeof k === 'string'))].sort()
      : [];
    // Deep-linkable: /it/operate/bottlenecks?kind=wholesale-keg-order.
    const asked = new URLSearchParams(window.location.search).get('kind');
    kind = asked && kinds.includes(asked) ? asked : (kinds[0] ?? null);
  }

  async function loadSpec(k: string): Promise<void> {
    const res = await fetch(`/api/workflows/${encodeURIComponent(k)}`);
    if (!res.ok) throw new Error(`workflow ${k}: HTTP ${res.status}`);
    const spec: unknown = await res.json();
    const steps = (spec as { steps?: unknown } | null)?.steps;
    specSteps = Array.isArray(steps) ? (steps as ReadonlyArray<SpecStep>) : [];
  }

  async function loadFleet(k: string): Promise<void> {
    const res = await fetch(`/api/views/fleet/${encodeURIComponent(k)}`);
    if (!res.ok) throw new Error(`fleet ${k}: HTTP ${res.status}`);
    const raw: unknown = await res.json();
    const f = (raw ?? {}) as Partial<Fleet> & { nodes?: unknown };
    fleet = {
      workflow_kind: typeof f.workflow_kind === 'string' ? f.workflow_kind : k,
      open_jobs: typeof f.open_jobs === 'number' ? f.open_jobs : 0,
      nodes: Array.isArray(f.nodes) ? (f.nodes as Fleet['nodes']) : [],
      as_of: typeof f.as_of === 'string' ? f.as_of : '',
    };
  }

  // The node badges are server counts; the item list under a
  // clicked node comes from the same capped jobs fetch every board
  // uses — the panel says "N of M" when the two disagree.
  async function loadJobs(k: string): Promise<void> {
    const res = await fetch(`/api/jobs?kind=${encodeURIComponent(k)}&status=open&limit=200`);
    if (!res.ok) throw new Error(`jobs: HTTP ${res.status}`);
    const body: unknown = await res.json();
    const data = (body as { data?: unknown } | null)?.data;
    jobs = Array.isArray(data) ? (data as ReadonlyArray<Job>) : [];
  }

  // The flow through the process, not just the live set: completed
  // hops + wall-clock latency per stage over the trailing week, from
  // /api/views/stage-durations. An empty station still shows the
  // process breathing. Enhancement, not dependency — a failed read
  // degrades to live-only.
  async function loadStages(k: string): Promise<void> {
    const res = await fetch(`/api/views/stage-durations/${encodeURIComponent(k)}?days=7`);
    if (!res.ok) {
      stageStats = [];
      return;
    }
    const body: unknown = await res.json();
    const stages = (body as { stages?: unknown } | null)?.stages;
    stageStats = Array.isArray(stages) ? (stages as ReadonlyArray<StageStat>) : [];
  }

  async function switchTo(k: string): Promise<void> {
    kind = k;
    loading = true;
    error = null;
    try {
      selectedNode = null;
      await Promise.all([loadSpec(k), loadFleet(k), loadJobs(k), loadStages(k)]);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    void (async () => {
      try {
        await loadKinds();
        if (kind) await switchTo(kind);
        else {
          loading = false;
          error = 'no Workflows in the registry';
        }
      } catch (e) {
        loading = false;
        error = e instanceof Error ? e.message : String(e);
      }
    })();
    const timer = setInterval(() => {
      if (kind) {
        void loadFleet(kind).catch(() => {});
        void loadJobs(kind).catch(() => {});
        void loadStages(kind).catch(() => {});
      }
    }, POLL_MS);
    return () => clearInterval(timer);
  });

  function badge(n: FleetNode): string {
    const parts: string[] = [];
    if (n.ready > 0) parts.push(`${n.ready} ready`);
    if (n.active > 0) parts.push(`${n.active} active`);
    if (n.unassigned > 0) parts.push(`${n.unassigned} unclaimed`);
    return parts.join(' · ');
  }

  /// Wall-clock age of the oldest still-ready step, against the
  /// server's clock — never the browser's, never sim time.
  function age(n: FleetNode): string {
    if (!n.oldest_ready_wall || !fleet) return '—';
    const ms = Date.parse(fleet.as_of) - Date.parse(n.oldest_ready_wall);
    if (ms < 0) return '—';
    const h = ms / 3_600_000;
    if (h < 1) return `${Math.round(h * 60)}m`;
    if (h < 48) return `${h.toFixed(1)}h`;
    return `${(h / 24).toFixed(1)}d`;
  }

  function roles(n: FleetNode): string {
    const entries = Object.entries(n.by_role);
    if (entries.length === 0) return '—';
    return entries.map(([r, c]) => `${r}: ${c}`).join(', ');
  }

  let bySlug = $derived(new Map((fleet?.nodes ?? []).map((n) => [n.slug, n])));
  let byNode = $derived(groupByPosition(jobs));
  let statBySlug = $derived(new Map(stageStats.map((s) => [s.slug, s])));


  let selectedJobs = $derived(selectedNode ? (byNode.get(selectedNode) ?? []) : []);
  let selectedServerCount = $derived.by(() => {
    if (!selectedNode) return 0;
    const server = bySlug.get(selectedNode);
    return server ? server.ready + server.active : selectedJobs.length;
  });

  /// The spec's DAG with fleet depth decorated on. A node with active
  /// work lights up active; ready-only lights up ready; idle stays
  /// neutral — the DAG reuses the step-status visual language for the
  /// fleet's aggregate state.
  let dag = $derived.by(() => {
    if (!specSteps) return null;
    const { nodes, edges } = workflowToDag(specSteps);
    const decorated: DagNode[] = nodes.map((n) => {
      const f = bySlug.get(n.id);
      if (!f) return n;
      return {
        ...n,
        status: f.active > 0 ? 'active' : f.ready > 0 ? 'ready' : undefined,
        badge: badge(f) || null,
      };
    });
    return { nodes: decorated, edges };
  });

  /// Fleet groups with no home on the current spec's DAG: slug-less
  /// steps grouped by title, and steps of superseded versions whose
  /// slugs the current version dropped.
  let offMap = $derived.by(() => {
    if (!fleet) return [];
    const onMap = new Set((dag?.nodes ?? []).map((n) => n.id));
    return fleet.nodes.filter((n) => !onMap.has(n.slug));
  });

  let inFlight = $derived(
    (fleet?.nodes ?? []).reduce((sum, n) => sum + n.ready + n.active, 0),
  );
</script>

<PageHeader
  title="Bottlenecks"
  subtitle="Where work piles up: per-step depth, completed flow, and wall-clock latency for a Workflow kind"
/>

<div class="fleet-bar">
  <label class="fleet-pick">
    Workflow
    <select
      value={kind ?? ''}
      onchange={(e) => void switchTo((e.target as HTMLSelectElement).value)}
    >
      {#each kinds as k (k)}
        <option value={k}>{k}</option>
      {/each}
    </select>
  </label>
  {#if fleet}
    <span class="fleet-scope">
      {fleet.open_jobs} open · {inFlight} steps in flight · as of {fleet.as_of}
    </span>
  {/if}
</div>

{#if loading}
  <p class="fleet-msg">Reading the fleet…</p>
{:else if error}
  <p class="fleet-msg fleet-err">{error}</p>
{:else if dag}
  <StepDag
    nodes={dag.nodes}
    edges={dag.edges}
    selectedId={selectedNode}
    onNodeClick={(id) => (selectedNode = selectedNode === id ? null : id)}
  />

  {#if selectedNode}
    <section class="fleet-node-items">
      <h3 class="fleet-node-h">
        {selectedNode}
        <span class="fleet-node-n">
          {selectedJobs.length === selectedServerCount
            ? `${selectedJobs.length} here`
            : `${selectedJobs.length} of ${selectedServerCount} shown`}
        </span>
      </h3>
      {#if selectedJobs.length === 0}
        <p class="fleet-msg">Nothing visible at this step (server may count steps of Jobs beyond the 200 shown).</p>
      {:else}
        <ul class="fleet-items">
          {#each selectedJobs as j (j.id)}
            <li>
              <button type="button" class="fleet-item" onclick={() => navigate(`/jobs/${j.id}`)}>
                <span class="fleet-item-pri" data-pri={j.priority ?? 'standard'}>{j.priority ?? 'standard'}</span>
                <span class="fleet-item-title">{j.title}</span>
                <span class="fleet-item-age">{j.opened_on ?? ''}</span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  {/if}

  {@const tableRows = (() => {
    const live = new Set((fleet?.nodes ?? []).map((n) => n.slug));
    const flowOnly = stageStats
      .filter((s) => !live.has(s.slug) && s.completed > 0)
      .map((s) => ({ slug: s.slug, ready: 0, active: 0, unassigned: 0, by_role: {}, oldest_ready_wall: null }));
    return [...(fleet?.nodes ?? []), ...flowOnly];
  })()}
  {#if tableRows.length > 0}
    <table class="fleet-table">
      <thead>
        <tr>
          <th>Step</th>
          <th>Ready</th>
          <th>Active</th>
          <th>Unclaimed</th>
          <th>Role lenses</th>
          <th>Oldest wait</th>
          <th>Done (7d)</th>
          <th>p50 / max</th>
          <th>Expected wait</th>
        </tr>
      </thead>
      <tbody>
        {#each tableRows as n (n.slug)}
          {@const st = statBySlug.get(n.slug)}
          <tr>
            <td>
              {n.slug}
              {#if offMap.includes(n)}
                <span class="fleet-offmap" title="No matching step on the current Workflow version — a slug-less step grouped by title, or a step of a superseded version">off map</span>
              {/if}
            </td>
            <td>{n.ready}</td>
            <td>{n.active}</td>
            <td>{n.unassigned}</td>
            <td>{roles(n)}</td>
            <td>{age(n)}</td>
            <td>{st?.completed ?? 0}</td>
            <td>{st && st.completed > 0 ? `${fmtDur(st.p50_seconds)} / ${fmtDur(st.max_seconds)}` : '—'}</td>
            <td title="Little's law: depth ÷ drain rate over the window — what a new arrival should expect if nothing changes">
              {st && st.completed > 0 && n.ready + n.active > 0
                ? `~${fmtDur(((n.ready + n.active) * 7 * 86400) / st.completed)}`
                : '—'}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else}
    <p class="fleet-msg">Nothing in flight for this kind.</p>
  {/if}
{/if}

<style>
  .fleet-bar {
    display: flex;
    align-items: baseline;
    gap: 16px;
    margin: 12px 0;
  }
  .fleet-pick {
    display: inline-flex;
    align-items: baseline;
    gap: 8px;
    font-size: 13px;
    color: var(--static, #7A838C);
  }
  .fleet-scope {
    font-size: 12px;
    color: var(--static, #7A838C);
  }
  .fleet-msg {
    margin: 24px 0;
    color: var(--static, #7A838C);
  }
  .fleet-err {
    color: var(--err, #e2685c);
  }
  .fleet-table {
    margin-top: 16px;
    border-collapse: collapse;
    font-size: 13px;
  }
  .fleet-table th,
  .fleet-table td {
    text-align: left;
    padding: 6px 14px 6px 0;
    border-bottom: 1px solid var(--hairline, #2A3138);
  }
  .fleet-table th {
    font-weight: 600;
    color: var(--static, #7A838C);
  }
  .fleet-node-items {
    margin-top: 14px;
  }
  .fleet-node-h {
    font-size: 14px;
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .fleet-node-n {
    font-size: 12px;
    font-weight: 400;
    color: var(--static, #7A838C);
  }
  .fleet-items {
    list-style: none;
    margin: 8px 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-width: 720px;
  }
  .fleet-item {
    display: flex;
    align-items: baseline;
    gap: 10px;
    width: 100%;
    text-align: left;
    padding: 7px 12px;
    border: 1px solid var(--hairline, #2A3138);
    border-radius: 6px;
    background: var(--card, var(--ink, #12161C));
    cursor: pointer;
    font: inherit;
    color: inherit;
  }
  .fleet-item-pri {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    color: var(--static, #7A838C);
  }
  .fleet-item-pri[data-pri='urgent'],
  .fleet-item-pri[data-pri='emergency'] {
    color: var(--err, #e2685c);
  }
  .fleet-item-title {
    flex: 1;
    font-size: 13px;
  }
  .fleet-item-age {
    font-size: 12px;
    color: var(--static, #7A838C);
  }
  .fleet-offmap {
    margin-left: 6px;
    font-size: 11px;
    font-weight: 600;
    color: var(--signal, #5FD4A8);
  }
</style>
