<script lang="ts">
  // v2 step view for the Job detail page: the selected step's surface
  // WIDE on the left, the workflow narrow on the right (David,
  // 7d63af73 — "put the workflow graph narrow on the right and the
  // custom UX wide on the left on the same vertical").
  //
  // The rail is a different rendering rather than a squeezed graph.
  // StepDag lays out at a fixed pixel width and scrolls rather than
  // reflows — a user-feedback packet's graph is 1,314px — so putting
  // the canvas itself in a ~320px column would show a quarter of a
  // diagram behind a scrollbar. See StepRail.
  //
  // v2 has no tiers — the graph's edges come from each step's resolved
  // upstream dependencies (`blocked_by`), not a `sort_order` bucket.

  import StepSurface from '../steps/StepSurface.svelte';
  import StepDag, { type DagNode, type DagEdge } from './StepDag.svelte';
  import StepRail, { type RailNode } from './StepRail.svelte';
  import type { StepStatus } from './types';

  // Matches StepSurface's StepData shape so the same object passes
  // through cleanly when a node is selected.
  type Step = {
    id: string;
    kind: string;
    title: string;
    status: StepStatus;
    assignee_id: string | null;
    sort_order: number;
    sign_offs_required?: string[];
    sign_offs?: {
      authority_id: string;
      role: string;
      stamped_at: string;
      shape_hash: string;
    }[];
    metadata: Record<string, unknown>;
    notes: string | null;
    blocked_by?: string[];
  };

  type Props = {
    steps: Step[];
    jobId: string;
    onUpdate: () => void;
  };
  let { steps, jobId, onUpdate }: Props = $props();

  let pickedId = $state<string | null>(null);

  // Resolved selection: the explicitly-clicked step, else the natural
  // focus — the in-flight step, then the next ready one, then the first.
  let selected = $derived.by(() => {
    const explicit = steps.find((s) => s.id === pickedId);
    if (explicit) return explicit;
    return (
      steps.find((s) => s.status === 'active') ??
      steps.find((s) => s.status === 'ready') ??
      steps[0] ??
      null
    );
  });

  let nodes: DagNode[] = $derived(
    [...steps]
      .sort((a, b) => (a.sort_order ?? 0) - (b.sort_order ?? 0))
      .map((s) => ({ id: s.id, title: s.title, kind: s.kind, status: s.status })),
  );

  // Edges from resolved upstream deps. Filter to declared steps so a
  // dangling blocker id can't draw an edge to nowhere.
  let edges: DagEdge[] = $derived(
    steps.flatMap((s) =>
      (s.blocked_by ?? [])
        .filter((b) => steps.some((x) => x.id === b))
        .map((b) => ({ from: b, to: s.id })),
    ),
  );

  /// Longest-path layering, the same rule StepDag places columns by —
  /// so the rail reads top-to-bottom in the order the canvas reads
  /// left-to-right, and steps sharing a layer are a fork's branches.
  /// The `visiting` guard is defensive: the viability lint proves the
  /// graph acyclic, and a malformed edge set must not hang the UI.
  let railNodes: RailNode[] = $derived.by(() => {
    const parents = new Map<string, string[]>(steps.map((s) => [s.id, []]));
    for (const e of edges) parents.get(e.to)?.push(e.from);
    const layer = new Map<string, number>();
    const visiting = new Set<string>();
    const depth = (id: string): number => {
      const seen = layer.get(id);
      if (seen !== undefined) return seen;
      if (visiting.has(id)) return 0;
      visiting.add(id);
      const ps = parents.get(id) ?? [];
      const d = ps.length === 0 ? 0 : 1 + Math.max(...ps.map(depth));
      visiting.delete(id);
      layer.set(id, d);
      return d;
    };
    return [...steps]
      .sort((a, b) => (a.sort_order ?? 0) - (b.sort_order ?? 0))
      .map((s) => ({ id: s.id, title: s.title, status: s.status, layer: depth(s.id) }));
  });
</script>

<div class="sg">
  {#if selected}
    <div class="sg-detail">
      <StepSurface step={selected} {jobId} {onUpdate} />
    </div>
  {/if}
  {#if steps.length > 0}
    <aside class="sg-rail">
      <h3 class="sg-rail-h">Workflow</h3>
      <StepRail
        nodes={railNodes}
        selectedId={selected?.id ?? null}
        onNodeClick={(id) => (pickedId = id)}
      />
    </aside>
  {/if}
</div>

<!-- The canvas keeps the job the rail cannot do — showing SHAPE — and
     stays full width below, where it has room to be read. -->
{#if steps.length > 0}
  <details class="sg-canvas">
    <summary>Show the whole workflow</summary>
    <StepDag
      {nodes}
      {edges}
      selectedId={selected?.id ?? null}
      onNodeClick={(id) => (pickedId = id)}
    />
  </details>
{/if}

<style>
  /* Surface wide, workflow narrow, tops aligned. The surface comes
     FIRST in the DOM so it is what a screen reader and a phone reach
     first — on a narrow viewport the columns stack and the rail sits
     under the thing it describes. */
  .sg {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 260px;
    gap: 16px;
    align-items: start;
  }
  @media (max-width: 900px) {
    .sg {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .sg-detail {
    border: 1px solid var(--hairline);
    border-radius: 8px;
    background: var(--card);
    padding: 12px 14px;
    min-width: 0;
  }
  .sg-rail {
    border: 1px solid var(--hairline);
    border-radius: 8px;
    padding: 10px 8px 12px;
    min-width: 0;
  }
  .sg-rail-h {
    font-family: var(--font-mono);
    font-size: 11px;
    font-weight: 400;
    letter-spacing: var(--ls-nav);
    text-transform: uppercase;
    color: var(--static);
    margin: 0 0 8px 8px;
  }
  .sg-canvas {
    margin-top: 14px;
  }
  .sg-canvas summary {
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: var(--ls-nav);
    text-transform: uppercase;
    color: var(--static);
    cursor: pointer;
    padding: 6px 0;
  }
</style>
