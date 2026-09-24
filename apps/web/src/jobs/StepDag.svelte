<script module lang="ts">
  // Importable types: `import StepDag, { type DagNode } from './StepDag.svelte'`.
  export type DagNode = {
    /** Stable key — step slug (authoring) or step id (runtime). */
    id: string;
    title: string;
    kind: string;
    /** Runtime only. Undefined renders the neutral authoring card. */
    status?: 'pending' | 'ready' | 'active' | 'completed' | 'skipped' | string;
    /** Outcome label when this step is terminal (closes the Job). */
    terminal?: string | null;
    /** Optional caller-supplied chip (fleet depth counts, etc.). */
    badge?: string | null;
    /** Live-activity flash: bump the counter to retrigger the ring
     *  (FlowMotion — a step completing pulses its node). */
    pulse?: number;
  };
  export type DagEdge = {
    from: string;
    to: string;
    label?: string;
    /** The metadata comparison that opens this edge, when the
     *  downstream step's `ready_when` gates on one — a routing edge
     *  (fork choice) rather than a plain dependency. */
    condition?: { field: string; value: string };
  };
</script>

<script lang="ts">
  // Bespoke step-DAG renderer — the BOSS-native replacement for the
  // Mermaid diagrams.
  //
  // Presentation only: callers pass `nodes` + `edges`; this component
  // lays them out in topological layers (longest-path from the roots)
  // and draws dependency edges as smooth curves on an SVG layer behind
  // HTML node-cards. v2-native — edges come from the dependency list
  // (a step's resolved upstream steps / `ready_when` references), never
  // from tiers.
  //
  // Two modes, one renderer:
  //   - authoring  → `status` undefined, neutral cards (JobKind shape).
  //   - runtime    → `status` set, cards colored by live step status.

  type Props = {
    nodes: ReadonlyArray<DagNode>;
    edges: ReadonlyArray<DagEdge>;
    onNodeClick?: (id: string) => void;
    /** When set, edges become interactive: routing edges (those with
     *  a parsed `condition`) render as click targets. The caller
     *  decides what a click means — TriageFlow completes the fork
     *  step with the edge's condition. */
    onEdgeClick?: (edge: DagEdge) => void;
    selectedId?: string | null;
  };
  let { nodes, edges, onNodeClick, onEdgeClick, selectedId = null }: Props = $props();

  // ---- geometry ----
  const NODE_W = 188;
  const NODE_H = 66;
  const H_GAP = 30;
  const V_GAP = 52;
  const PAD = 18;

  type Placed = DagNode & { x: number; y: number; layer: number };

  // Dedupe edges by direction and drop self-loops, so a doubled
  // dependency — a blocker listed twice, or two predicates that both
  // reference the same upstream step — draws a single arrow rather than
  // stacking identical arrows on the same path.
  const uniqueEdges: DagEdge[] = $derived.by(() => {
    const seen = new Set<string>();
    const out: DagEdge[] = [];
    for (const e of edges) {
      if (e.from === e.to) continue;
      const key = `${e.from}→${e.to}`;
      if (seen.has(key)) continue;
      seen.add(key);
      out.push(e);
    }
    return out;
  });

  const layout = $derived.by(() => {
    const incoming = new Map<string, string[]>();
    for (const n of nodes) incoming.set(n.id, []);
    for (const e of uniqueEdges) {
      if (incoming.has(e.to) && incoming.has(e.from)) incoming.get(e.to)!.push(e.from);
    }

    // Longest-path layering. The viability lint guarantees the graph is
    // acyclic; the `visiting` set is a defensive guard so a malformed
    // edge set can't infinite-loop the UI.
    const layerOf = new Map<string, number>();
    const visiting = new Set<string>();
    const depth = (id: string): number => {
      const cached = layerOf.get(id);
      if (cached !== undefined) return cached;
      if (visiting.has(id)) return 0;
      visiting.add(id);
      const parents = incoming.get(id) ?? [];
      const d = parents.length === 0 ? 0 : 1 + Math.max(...parents.map(depth));
      visiting.delete(id);
      layerOf.set(id, d);
      return d;
    };
    for (const n of nodes) depth(n.id);

    // Group by layer, preserving the caller's authoring order within a
    // layer (deterministic, no crossing-minimization pass for v1 — the
    // graphs are small).
    const byLayer = new Map<number, DagNode[]>();
    for (const n of nodes) {
      const l = layerOf.get(n.id) ?? 0;
      (byLayer.get(l) ?? byLayer.set(l, []).get(l)!).push(n);
    }
    const layerKeys = [...byLayer.keys()].sort((a, b) => a - b);
    const widest = Math.max(1, ...layerKeys.map((l) => byLayer.get(l)!.length));
    const spanW = widest * NODE_W + (widest - 1) * H_GAP;

    const placed: Placed[] = [];
    for (const l of layerKeys) {
      const row = byLayer.get(l)!;
      const rowW = row.length * NODE_W + (row.length - 1) * H_GAP;
      const x0 = PAD + (spanW - rowW) / 2;
      row.forEach((n, i) => {
        placed.push({
          ...n,
          layer: l,
          x: x0 + i * (NODE_W + H_GAP),
          y: PAD + l * (NODE_H + V_GAP),
        });
      });
    }
    const pos = new Map(placed.map((p) => [p.id, p]));
    return {
      placed,
      pos,
      width: spanW + PAD * 2,
      height: PAD * 2 + layerKeys.length * NODE_H + Math.max(0, layerKeys.length - 1) * V_GAP,
    };
  });

  function edgePath(f: Placed, t: Placed): string {
    const x1 = f.x + NODE_W / 2;
    const y1 = f.y + NODE_H;
    const x2 = t.x + NODE_W / 2;
    const y2 = t.y;
    const my = (y1 + y2) / 2;
    return `M ${x1} ${y1} C ${x1} ${my}, ${x2} ${my}, ${x2} ${y2}`;
  }

  function midpoint(f: Placed, t: Placed): { x: number; y: number } {
    return { x: (f.x + t.x) / 2 + NODE_W / 2, y: (f.y + NODE_H + t.y) / 2 };
  }

  const ICON: Record<string, string> = {
    completed: '✓',
    active: '●',
    ready: '◆',
    pending: '○',
    skipped: '⊘',
  };
  function statusClass(n: DagNode): string {
    if (!n.status) return 'n-plain';
    switch (n.status) {
      case 'completed':
        return 'n-done';
      case 'active':
        return 'n-active';
      case 'ready':
        return 'n-ready';
      case 'skipped':
        return 'n-skipped';
      default:
        return 'n-pending';
    }
  }
</script>

<div class="dag-scroll">
  <div class="dag" style="width:{layout.width}px; height:{layout.height}px;">
    <svg
      class="dag-edges"
      width={layout.width}
      height={layout.height}
      viewBox="0 0 {layout.width} {layout.height}"
      aria-hidden="true"
    >
      <defs>
        <marker
          id="dag-arrow"
          viewBox="0 0 10 10"
          refX="8"
          refY="5"
          markerWidth="7"
          markerHeight="7"
          orient="auto-start-reverse"
        >
          <path d="M 0 1 L 9 5 L 0 9 z" class="dag-arrowhead" />
        </marker>
      </defs>
      {#each uniqueEdges as e (e.from + '→' + e.to)}
        {@const f = layout.pos.get(e.from)}
        {@const t = layout.pos.get(e.to)}
        {@const clickable = Boolean(onEdgeClick && e.condition)}
        {#if f && t}
          <path class="dag-edge" d={edgePath(f, t)} marker-end="url(#dag-arrow)" />
          {#if clickable}
            <!-- Wide transparent twin of the edge path — the actual
                 hit area, so a routing edge doesn't demand
                 pixel-perfect clicks on a 1.5px curve. -->
            <path
              class="dag-edge-hit"
              d={edgePath(f, t)}
              role="button"
              tabindex="0"
              aria-label="route: {e.label}"
              onclick={() => onEdgeClick?.(e)}
              onkeydown={(ev) => {
                if (ev.key === 'Enter' || ev.key === ' ') onEdgeClick?.(e);
              }}
            />
          {/if}
          {#if e.label}
            {@const m = midpoint(f, t)}
            <text
              class="dag-edge-label"
              class:clickable
              x={m.x}
              y={m.y}
              text-anchor="middle"
              dy="-2"
              onclick={() => { if (clickable) onEdgeClick?.(e); }}
            >{e.label}</text>
          {/if}
        {/if}
      {/each}
    </svg>

    {#snippet nodeBody(n: Placed)}
      <span class="node-top">
        {#if n.status}<span class="node-icon">{ICON[n.status] ?? '○'}</span>{/if}
        <span class="node-kind">{n.kind}</span>
        {#if n.terminal}<span class="node-terminal">⤳ {n.terminal}</span>{/if}
      </span>
      <span class="node-title">{n.title}</span>
      {#if n.badge}<span class="node-badge">{n.badge}</span>{/if}
      {#if n.pulse}{#key n.pulse}<span class="node-pulse" aria-hidden="true"></span>{/key}{/if}
    {/snippet}

    {#each layout.placed as n (n.id)}
      {@const place = `left:${n.x}px; top:${n.y}px; width:${NODE_W}px; height:${NODE_H}px;`}
      <!-- A node is a button only when a click means something: the
           interaction crawl found the workflow detail page's nodes
           rendered as buttons wired to a handler nobody passed
           (backlog 68409b1b). Without a handler it is a card. -->
      {#if onNodeClick}
        <button
          type="button"
          class="node {statusClass(n)}"
          class:selected={selectedId === n.id}
          class:terminal={n.terminal}
          style={place}
          onclick={() => onNodeClick?.(n.id)}
          title={n.title}
        >
          {@render nodeBody(n)}
        </button>
      {:else}
        <div
          class="node inert {statusClass(n)}"
          class:selected={selectedId === n.id}
          class:terminal={n.terminal}
          style={place}
          title={n.title}
        >
          {@render nodeBody(n)}
        </div>
      {/if}
    {/each}
  </div>
</div>

<style>
  .dag-scroll {
    overflow: auto;
    padding: 4px;
  }
  .dag {
    position: relative;
    margin: 0 auto;
  }
  .dag-edges {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }
  .node-pulse {
    position: absolute;
    inset: -3px;
    border-radius: 8px;
    pointer-events: none;
    animation: dag-pulse 1.4s ease-out forwards;
  }
  @keyframes dag-pulse {
    0% { box-shadow: 0 0 0 0 var(--signal); }
    100% { box-shadow: 0 0 0 14px transparent; }
  }
  .dag-edge {
    fill: none;
    stroke: var(--border-strong);
    stroke-width: 1.5px;
  }
  .dag-arrowhead {
    fill: var(--border-strong);
  }
  .dag-edge-label {
    fill: var(--static);
    font-size: 10.5px;
    font-weight: 600;
    paint-order: stroke;
    stroke: var(--bg);
    stroke-width: 3px;
  }
  /* Interactive routing edges: the svg layer is pointer-events:none,
     so only these two opt back in. */
  .dag-edge-hit {
    fill: none;
    stroke: transparent;
    stroke-width: 14px;
    pointer-events: stroke;
    cursor: pointer;
  }
  .dag-edge-hit:hover + .dag-edge-label,
  .dag-edge-label.clickable:hover {
    fill: var(--signal);
  }
  .dag-edge-label.clickable {
    pointer-events: auto;
    cursor: pointer;
  }

  .node {
    position: absolute;
    display: flex;
    flex-direction: column;
    gap: 3px;
    justify-content: center;
    box-sizing: border-box;
    padding: 8px 12px;
    border: 1px solid var(--hairline);
    border-left: 4px solid var(--border-strong);
    border-radius: 8px;
    background: var(--card);
    cursor: pointer;
    text-align: left;
    font: inherit;
    color: inherit;
    transition:
      box-shadow 0.12s ease,
      transform 0.12s ease;
  }
  .node:hover {
    transform: translateY(-1px);
  }
  /* No handler: no pointer cursor and no hover lift, so the card does
     not promise a click it cannot answer. */
  .node.inert,
  .node.inert:hover {
    cursor: default;
    transform: none;
  }
  .node.selected {
    outline: 2px solid var(--signal);
    outline-offset: 1px;
  }

  .node-top {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--static);
    text-transform: lowercase;
    letter-spacing: 0.02em;
  }
  .node-icon {
    font-size: 11px;
  }
  .node-kind {
    font-weight: 600;
  }
  .node-terminal {
    margin-left: auto;
    color: var(--signal);
    font-weight: 600;
    text-transform: none;
  }
  .node-title {
    font-size: 13px;
    font-weight: 500;
    line-height: 1.25;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
  }
  .node-badge {
    font-size: 11px;
    font-weight: 600;
    color: var(--signal);
    white-space: nowrap;
  }

  /* Status accents — the left edge, in the step's plate (backlog
     6f471ff6, car 3), so a state is one colour wherever a step is
     drawn: completed solid ink, active busy, ready the action blue,
     pending the plate's dashed ink frame, skipped that frame gone
     quiet. No wash: Enamel's states are solid plates, never tints —
     and the tints here had drifted from the plates (a ready step sat
     on the clear green's wash). */
  .node.n-done {
    border-left-color: var(--completed);
  }
  .node.n-active {
    border-left-color: var(--busy);
  }
  .node.n-ready {
    border-left-color: var(--ready);
  }
  .node.n-pending {
    border-left-color: var(--border-strong);
    border-left-style: dashed;
  }
  .node.n-skipped {
    border-left-color: var(--static);
    border-left-style: dashed;
  }
  .node.n-skipped .node-title {
    color: var(--static);
    text-decoration: line-through;
  }
  .node.terminal {
    border-style: solid;
    box-shadow: 0 0 0 1px var(--signal) inset;
  }
</style>
