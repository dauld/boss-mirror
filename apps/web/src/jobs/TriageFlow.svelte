<script lang="ts">
  // TriageFlow — the Workflow-shaped triage surface (65fa5a1c).
  //
  // The TriageBoard answers "what did triage decide": flat columns
  // keyed on the fork's options. This answers the question David
  // actually asked — "where is everything, along the Workflow" — and
  // makes the routing choices *edges*: the DAG's routing edges carry
  // their parsed `ready_when` condition (workflowToDag), so clicking
  // triage→build with an item selected completes that item's fork
  // step with `disposition = "build"`. Selecting an edge IS the
  // decision; there is no separate move.
  //
  // Per-step depth badges come from `/api/views/fleet/{kind}` — the
  // server-truth aggregate — while the item cards under a node come
  // from the same open-jobs fetch the board uses. The two can
  // disagree under the 200-job cap; the node shows the server count
  // and the panel says "N of M shown" when they differ, rather than
  // pretending the cap doesn't exist.
  //
  // Every fetch decodes defensively at the call site: the route-smoke
  // crawl runs this surface against an adversarial mock, and garbage
  // must render as an empty state, not a crash.
  // A BOARD, NOT A PAGE (design e765b3fc, car N3, 2026-09-25): its two
  // mount sites — the feedback and backlog boards — are station panels on
  // the Department Map, under the map's own page heading and the
  // station's, so the board heads itself as a section of that panel
  // rather than as a second page title.
  import StepDag, { type DagEdge, type DagNode } from './StepDag.svelte';
  import { workflowToDag } from './workflowToDag';
  import { type Fork, readFork } from './fork';
  import { currentStep, groupByPosition, positionOf } from './position';
  import type { Job } from './types';

  type Props = Readonly<{
    kind: string;
    title: string;
    subtitle?: string;
  }>;
  let {
    kind,
    title,
    subtitle = 'Click a step to see its queue; with an item selected, click an outgoing edge to route it.',
  }: Props = $props();

  type FleetNode = Readonly<{
    slug: string;
    ready: number;
    active: number;
    unassigned: number;
  }>;

  let specSteps = $state<ReadonlyArray<unknown> | null>(null);
  let fork = $state<Fork | null>(null);
  let jobs = $state<ReadonlyArray<Job>>([]);
  let fleetNodes = $state<ReadonlyArray<FleetNode>>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let selectedNode = $state<string | null>(null);
  let selectedJobId = $state<string | null>(null);

  async function load(background = false): Promise<void> {
    // Same silent-refresh rule as TriageBoard (feedback 15c6004e).
    if (!background) loading = true;
    error = null;
    try {
      const [specRes, jobsRes, fleetRes] = await Promise.all([
        fetch(`/api/workflows/${encodeURIComponent(kind)}`),
        fetch(`/api/jobs?kind=${encodeURIComponent(kind)}&status=open&limit=200`),
        fetch(`/api/views/fleet/${encodeURIComponent(kind)}`),
      ]);
      if (!specRes.ok) throw new Error(`workflow ${kind}: HTTP ${specRes.status}`);
      const spec: unknown = await specRes.json();
      const steps = (spec as { steps?: unknown } | null)?.steps;
      specSteps = Array.isArray(steps) ? steps : [];
      fork = readFork(spec);

      if (!jobsRes.ok) throw new Error(`jobs: HTTP ${jobsRes.status}`);
      const jobsBody: unknown = await jobsRes.json();
      const data = (jobsBody as { data?: unknown } | null)?.data;
      jobs = Array.isArray(data) ? (data as ReadonlyArray<Job>) : [];

      // Fleet counts are an enhancement, not a dependency — a failed
      // aggregate read degrades to client-side counts.
      if (fleetRes.ok) {
        const f: unknown = await fleetRes.json();
        const nodes = (f as { nodes?: unknown } | null)?.nodes;
        fleetNodes = Array.isArray(nodes) ? (nodes as ReadonlyArray<FleetNode>) : [];
      } else {
        fleetNodes = [];
      }
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    void kind;
    void load();
  });

  // The queue moves without this page's involvement (agents route
  // items, steps complete downstream) — re-fetch on a poll so the
  // map reflects it. SSE-policy bucket (b): this is an aggregate
  // surface; 15s is the operator-attention cadence, and a routing
  // action already reloads immediately.
  $effect(() => {
    const t = setInterval(() => {
      if (!busy && !loading) void load(true);
    }, 15_000);
    return () => clearInterval(t);
  });

  let byNode = $derived(groupByPosition(jobs));

  let fleetBySlug = $derived(new Map(fleetNodes.map((n) => [n.slug, n])));

  let dag = $derived.by(() => {
    if (!specSteps) return null;
    const { nodes, edges } = workflowToDag(specSteps as never);
    const decorated: DagNode[] = nodes.map((n) => {
      const server = fleetBySlug.get(n.id);
      const local = byNode.get(n.id)?.length ?? 0;
      const depth = server ? server.ready + server.active : local;
      return {
        ...n,
        status: depth > 0 ? ((server?.active ?? 0) > 0 ? 'active' : 'ready') : undefined,
        badge: depth > 0 ? `${depth} waiting` : null,
      };
    });
    return { nodes: decorated, edges };
  });

  let selectedJobs = $derived(selectedNode ? (byNode.get(selectedNode) ?? []) : []);
  let selectedServerCount = $derived.by(() => {
    if (!selectedNode) return 0;
    const server = fleetBySlug.get(selectedNode);
    return server ? server.ready + server.active : selectedJobs.length;
  });
  let selectedJob = $derived(
    selectedJobId ? (jobs.find((j) => j.id === selectedJobId) ?? null) : null,
  );

  function selectNode(id: string): void {
    selectedNode = id;
    const list = byNode.get(id) ?? [];
    selectedJobId = list.length === 1 ? list[0]!.id : null;
  }

  /// The edge click IS the routing decision. Only meaningful when the
  /// selected item is sitting at the edge's origin and the item's
  /// current step is the one the condition writes to.
  async function routeAlong(edge: DagEdge): Promise<void> {
    if (!edge.condition || busy) return;
    const j = selectedJob;
    if (!j) {
      error = 'Select an item first — an edge routes the selected item.';
      return;
    }
    if (positionOf(j) !== edge.from) {
      error = `The selected item is not at "${edge.from}".`;
      return;
    }
    const step = currentStep(j);
    if (!step) return;
    busy = true;
    error = null;
    try {
      // PUT overlays top-level fields and replaces metadata wholesale
      // — merge with the existing keys (authority_role lives there;
      // see TriageBoard's patchStep for the incident this prevents).
      const r = await fetch(`/api/jobs/${j.id}/steps/${step.id}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          status: 'completed',
          metadata: { ...(step.metadata ?? {}), [edge.condition.field]: edge.condition.value },
        }),
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
      selectedJobId = null;
      await load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<header class="tf-head">
  <h3 class="tf-title">{title}</h3>
  {#if subtitle}<p class="tf-sub">{subtitle}</p>{/if}
</header>

{#if loading}
  <p class="tf-msg">Reading the queue…</p>
{:else if error && !dag}
  <p class="tf-msg tf-err">{error}</p>
{:else if dag}
  {#if error}<p class="tf-msg tf-err">{error}</p>{/if}
  <StepDag
    nodes={dag.nodes}
    edges={dag.edges}
    selectedId={selectedNode}
    onNodeClick={selectNode}
    onEdgeClick={(e) => void routeAlong(e)}
  />

  {#if selectedNode}
    <section class="tf-queue">
      <h3 class="tf-queue-h">
        {selectedNode}
        <span class="tf-queue-n">
          {selectedJobs.length === selectedServerCount
            ? `${selectedJobs.length} waiting`
            : `${selectedJobs.length} of ${selectedServerCount} shown`}
        </span>
      </h3>
      {#if selectedJobs.length === 0}
        <p class="tf-msg">Nothing waiting at this step.</p>
      {:else}
        <ul class="tf-items">
          {#each selectedJobs as j (j.id)}
            <li>
              <button
                type="button"
                class="tf-item"
                class:selected={selectedJobId === j.id}
                onclick={() => (selectedJobId = selectedJobId === j.id ? null : j.id)}
              >
                <span class="tf-item-pri" data-pri={j.priority ?? 'standard'}>{j.priority ?? 'standard'}</span>
                <span class="tf-item-title">{j.title}</span>
                <span class="tf-item-age">{j.opened_on ?? ''}</span>
              </button>
            </li>
          {/each}
        </ul>
        {#if selectedJob && fork && positionOf(selectedJob) === selectedNode}
          {@const routes = dag.edges.filter((e) => e.from === selectedNode && e.condition)}
          {#if routes.length > 0}
            <p class="tf-hint">
              Route “{selectedJob.title}”: click an outgoing edge on the map
              {#if routes.length > 0}
                — or here:
                {#each routes as e (e.to)}
                  <button
                    type="button"
                    class="tf-route"
                    disabled={busy}
                    onclick={() => void routeAlong(e)}
                  >{e.label}</button>
                {/each}
              {/if}
            </p>
          {/if}
        {/if}
      {/if}
    </section>
  {:else}
    <p class="tf-msg">Click a step on the map to open its queue.</p>
  {/if}
{/if}

<style>
  .tf-head { margin: 0 0 12px; }
  .tf-title {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 12px;
    font-weight: 400;
    letter-spacing: var(--ls-eyebrow);
    text-transform: uppercase;
    color: var(--signal);
  }
  .tf-sub { margin: 4px 0 0; font-size: 13px; color: var(--static); max-width: 72ch; }
  .tf-msg {
    margin: 16px 0;
    color: var(--static);
  }
  .tf-err {
    color: var(--err);
  }
  .tf-queue {
    margin-top: 18px;
  }
  .tf-queue-h {
    font-size: 14px;
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .tf-queue-n {
    font-size: 12px;
    font-weight: 400;
    color: var(--static);
  }
  .tf-items {
    list-style: none;
    margin: 8px 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-width: 720px;
  }
  .tf-item {
    display: flex;
    align-items: baseline;
    gap: 10px;
    width: 100%;
    text-align: left;
    padding: 8px 12px;
    border: 1px solid var(--hairline);
    border-radius: 6px;
    background: var(--card);
    cursor: pointer;
  }
  .tf-item.selected {
    border-color: var(--signal);
  }
  .tf-item-pri {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    color: var(--static);
  }
  .tf-item-pri[data-pri='urgent'],
  .tf-item-pri[data-pri='emergency'] {
    color: var(--err);
  }
  .tf-item-title {
    flex: 1;
    font-size: 13px;
  }
  .tf-item-age {
    font-size: 12px;
    color: var(--static);
  }
  .tf-hint {
    margin-top: 10px;
    font-size: 13px;
    color: var(--static);
  }
  .tf-route {
    margin-left: 6px;
    padding: 3px 10px;
    border: 1px solid var(--signal);
    border-radius: 999px;
    background: transparent;
    color: var(--signal);
    font-size: 12px;
    font-weight: 600;
    cursor: pointer;
  }
  .tf-route:disabled {
    opacity: 0.5;
  }
</style>
