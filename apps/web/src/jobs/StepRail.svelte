<script lang="ts">
  // The step graph, rendered narrow.
  //
  // David asked for the workflow graph narrow on the right with the
  // step UX wide on the left (7d63af73). The layout is easy; the graph
  // is not. `StepDag` lays out at a FIXED pixel width — widest layer ×
  // 188px + gaps — and scrolls rather than reflows, so a user-feedback
  // packet's graph is 1,314px wide. In a ~340px rail that is a quarter
  // of a diagram behind a scrollbar.
  //
  // So the rail is a different rendering of the same facts, not a
  // squeezed one. Top to bottom in dependency order, one step per row,
  // with the branch structure shown by indentation rather than by
  // edges: at a glance you get where am I, what is next, what is
  // blocked — which is what the graph is for in a sidebar. The
  // full-width canvas keeps the job of showing SHAPE.

  import type { StepStatus } from './types';

  export type RailNode = Readonly<{
    id: string;
    title: string;
    status: StepStatus;
    /** Longest-path layer, as StepDag computes it. Steps sharing a
     *  layer are alternatives to one another — the fork's branches —
     *  and read as a group. */
    layer: number;
  }>;

  type Props = Readonly<{
    nodes: readonly RailNode[];
    selectedId?: string | null;
    onNodeClick?: (id: string) => void;
  }>;
  let { nodes, selectedId = null, onNodeClick }: Props = $props();

  // Layer order, authoring order within a layer — the same ordering
  // StepDag places left to right, read top to bottom instead.
  const ordered = $derived([...nodes].sort((a, b) => a.layer - b.layer));

  /** A layer holding more than one step is a fork's branches: siblings,
   *  not a sequence. Indenting them says "one of these" without
   *  drawing an edge nobody has room for. */
  const branching = $derived.by(() => {
    const count = new Map<number, number>();
    for (const n of nodes) count.set(n.layer, (count.get(n.layer) ?? 0) + 1);
    return count;
  });

  /** How far the packet has come: the last row, in rail order, that is
   *  done, being done, or waiting to be done. The progress line is
   *  filled from the first stop to this one (-1: nothing yet). A
   *  skipped branch does not carry the line past it — a fork's
   *  not-taken arms sit after the live one in the same layer. */
  const reached = $derived(
    ordered.reduce(
      (at, n, i) => (n.status === 'completed' || n.status === 'active' || n.status === 'ready' ? i : at),
      -1,
    ),
  );
</script>

<!-- The packet's progress line (backlog 6f471ff6, car 3): the round-2
     board's route, run down the rail — a square 10px line through a
     stop for every step, filled as far as the packet has come. Each row
     draws the half of the line above its stop (::before) and the half
     below (::after), so the line is continuous with no measuring. -->
<nav class="rail" aria-label="Workflow steps">
  {#each ordered as n, i (n.id)}
    <button
      type="button"
      class="rail-row status-{n.status}"
      class:is-selected={n.id === selectedId}
      class:is-branch={(branching.get(n.layer) ?? 1) > 1}
      class:run-in={i > 0 && i <= reached}
      class:run-out={i < reached}
      onclick={() => onNodeClick?.(n.id)}
      aria-current={n.id === selectedId ? 'step' : undefined}
    >
      <span class="rail-stop"></span>
      <span class="rail-title">{n.title}</span>
    </button>
  {/each}
</nav>

<style>
  .rail {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .rail-row {
    position: relative;
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    text-align: left;
    background: none;
    border: 0;
    border-radius: var(--radius);
    padding: 6px 8px;
    color: inherit;
    font: inherit;
    cursor: pointer;
    min-width: 0;
  }
  /* The line: square and 10px, in the rule colour, centred under the
     stops (8px padding + half a 26px stop - half the line = 16px). The
     first stop has nothing above it and the last nothing below. */
  .rail-row::before,
  .rail-row::after {
    content: '';
    position: absolute;
    left: 16px;
    width: 10px;
    border-radius: 0;
    background: var(--hairline);
  }
  .rail-row::before {
    top: 0;
    bottom: 50%;
  }
  .rail-row::after {
    top: 50%;
    bottom: 0;
  }
  .rail-row:first-child::before,
  .rail-row:last-child::after {
    display: none;
  }
  /* Filled in the progress colour as far as the packet has come. */
  .rail-row.run-in::before,
  .rail-row.run-out::after {
    background: var(--signal);
  }
  /* The raised ground, not the rule colour: the unfilled line IS the
     rule colour, and would vanish into a hover drawn in it. */
  .rail-row:hover {
    background: var(--ink-raised);
  }
  /* The one being worked reads as the anchor, because the wide panel
     to its left is showing it. */
  .rail-row.is-selected {
    background: var(--ink-raised);
    box-shadow: inset 2px 0 0 var(--signal);
  }
  /* Siblings of a fork sit in from the spine: "one of these", without
     an edge there is no room to draw. Their stops stay ON the line —
     it is one route — so it is the name that steps in. */
  .rail-row.is-branch .rail-title {
    margin-left: 12px;
  }
  /* A stop: 26px, ringed 4px in night ink, over the line. */
  .rail-stop {
    position: relative;
    z-index: 1;
    box-sizing: border-box;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    border: 4px solid var(--border-strong);
    flex: 0 0 auto;
    background: var(--card);
  }
  /* Status is the stop, never the label — a rail of coloured words is
     unreadable at this width. Each stop wears its state's plate, the
     same colour a step's status plate is anywhere else: completed solid
     ink, active busy (the board's stop "you are here"), ready solid
     blue, pending the dashed frame of a plate not yet filled, skipped
     that frame gone quiet. */
  .rail-row.status-completed .rail-stop { background: var(--completed); }
  .rail-row.status-active .rail-stop { background: var(--busy); }
  .rail-row.status-ready .rail-stop { background: var(--ready); }
  .rail-row.status-pending .rail-stop { border-style: dashed; }
  .rail-row.status-skipped .rail-stop { border-style: dashed; border-color: var(--static); }

  .rail-title {
    font-size: 10.5px;
    font-weight: 700;
    letter-spacing: var(--ls-button);
    text-transform: uppercase;
    line-height: 1.35;
    color: var(--static);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* The stop the panel beside is showing reads in night ink. */
  .rail-row.is-selected .rail-title {
    color: var(--fog);
  }
  /* A step nobody will run should not compete with the live ones. */
  .rail-row.status-skipped .rail-title {
    text-decoration: line-through;
  }
</style>
