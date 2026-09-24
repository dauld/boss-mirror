<!--
  Interactive trigger→outcome graph for a Workflow draft (Slice 2).

  Steps are nodes; edges are derived from each step's `ready_when`
  references (the DAG is implicit in the predicates — `blocked_by` is
  never authored). Node types are first-class and visually distinct:
  trigger (entry), outcome (terminal), fork (≥2 successors), work.
  Layout is dagre (left→right). Live-lint `problems` badge the
  offending nodes. Clicking a node selects it (the parent opens the
  inspector). Editing the predicates themselves (the structured edge
  builder) is Slice 3 — this slice renders + selects + lints.
-->
<script lang="ts">
  import { SvelteFlow, Background, Controls } from '@xyflow/svelte';
  import type { Node, Edge } from '@xyflow/svelte';
  import '@xyflow/svelte/dist/style.css';
  import dagre from '@dagrejs/dagre';
  import type { StepSpec } from './workflowTypes';
  import type { LintProblem } from './liveLint';

  type Props = Readonly<{
    steps: ReadonlyArray<StepSpec>;
    /// Per-step lint problems, keyed by step slug (see liveLint).
    problems?: Map<string, LintProblem[]>;
    /// Currently-selected step slug (for highlight).
    selected?: string | null;
    onselect?: (slug: string) => void;
  }>;
  let { steps, problems, selected = null, onselect }: Props = $props();

  const NODE_W = 190;
  const NODE_H = 56;

  /// Unique `steps.<slug>` references out of a `ready_when` predicate.
  function referencedSlugs(readyWhen: string): string[] {
    const out = new Set<string>();
    const re = /steps\.([a-z][a-z0-9-]*)/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(readyWhen)) !== null) out.add(m[1]!);
    return [...out];
  }

  type NodeKind = 'trigger' | 'outcome' | 'fork' | 'work';

  function build(
    list: ReadonlyArray<StepSpec>,
    probs: Map<string, LintProblem[]> | undefined,
    sel: string | null,
  ): { nodes: Node[]; edges: Edge[] } {
    const declared = new Set(list.map((s) => s.title));
    const rawEdges = list.flatMap((s) =>
      referencedSlugs(s.ready_when ?? '')
        .filter((src) => declared.has(src) && src !== s.title)
        .map((src) => ({ from: src, to: s.title })),
    );
    const outDegree = new Map<string, number>();
    for (const e of rawEdges) outDegree.set(e.from, (outDegree.get(e.from) ?? 0) + 1);

    const g = new dagre.graphlib.Graph();
    g.setGraph({ rankdir: 'LR', nodesep: 30, ranksep: 60 });
    g.setDefaultEdgeLabel(() => ({}));
    for (const s of list) g.setNode(s.title, { width: NODE_W, height: NODE_H });
    for (const e of rawEdges) g.setEdge(e.from, e.to);
    dagre.layout(g);

    const nodes: Node[] = list.map((s) => {
      const isTrigger = (s.ready_when ?? '').trim() === 'true';
      const isTerminal = s.terminal != null;
      const isFork = (outDegree.get(s.title) ?? 0) >= 2;
      const kind: NodeKind = isTrigger
        ? 'trigger'
        : isTerminal
          ? 'outcome'
          : isFork
            ? 'fork'
            : 'work';
      const stepProblems = probs?.get(s.title) ?? [];
      const p = g.node(s.title);
      const sub = isTerminal ? `${s.kind} → ${s.terminal?.outcome}` : s.kind;
      const label = `${stepProblems.length ? '⚠ ' : ''}${s.title}\n${sub}`;
      const classes = [`jk-node`, `jk-${kind}`];
      if (stepProblems.length) classes.push('jk-problem');
      if (s.title === sel) classes.push('jk-selected');
      return {
        id: s.title,
        position: { x: p.x - NODE_W / 2, y: p.y - NODE_H / 2 },
        data: { label },
        class: classes.join(' '),
        sourcePosition: 'right',
        targetPosition: 'left',
      } as Node;
    });

    const edges: Edge[] = rawEdges.map((e) => ({
      id: `${e.from}__${e.to}`,
      source: e.from,
      target: e.to,
    }));
    return { nodes, edges };
  }

  const computed = $derived(build(steps, problems, selected));
  let nodes = $state.raw<Node[]>([]);
  let edges = $state.raw<Edge[]>([]);
  $effect(() => {
    nodes = computed.nodes;
    edges = computed.edges;
  });

  function handleNodeClick(slug: string): void {
    onselect?.(slug);
  }
</script>

<div class="jk-flow">
  {#if steps.length === 0}
    <div class="jk-empty">No steps yet — add a trigger and an outcome to begin.</div>
  {:else}
    <SvelteFlow
      bind:nodes
      bind:edges
      fitView
      nodesDraggable
      elementsSelectable
      onnodeclick={({ node }) => handleNodeClick(node.id)}
    >
      <Background />
      <Controls showLock={false} />
    </SvelteFlow>
  {/if}
</div>

<style>
  .jk-flow {
    width: 100%;
    height: 460px;
    border: 1px solid var(--hairline);
    border-radius: 8px;
    background: var(--ink-raised);
  }
  .jk-empty {
    display: grid;
    place-items: center;
    height: 100%;
    color: var(--static);
    font-size: 0.9rem;
  }
  /* Node-type styling via the class set in `build`. :global because the
     nodes render inside the Svelte Flow component subtree. */
  :global(.jk-node) {
    border-radius: 8px;
    border-width: 1.5px;
    white-space: pre-line;
    font-size: 0.78rem;
    text-align: center;
    padding: 6px 8px;
  }
  :global(.jk-trigger) {
    background: var(--ok-wash);
    border-color: var(--clear);
  }
  :global(.jk-outcome) {
    background: var(--signal-wash);
    border-color: var(--signal);
  }
  :global(.jk-fork) {
    background: var(--warn-wash);
    border-color: var(--busy);
  }
  :global(.jk-work) {
    background: var(--ink);
    border-color: var(--hairline);
  }
  :global(.jk-problem) {
    border-color: var(--troubled) !important;
  }
  :global(.jk-selected) {
    box-shadow: 0 0 0 2px var(--signal);
  }
</style>
