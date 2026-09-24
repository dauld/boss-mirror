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
</script>

<nav class="rail" aria-label="Workflow steps">
  {#each ordered as n (n.id)}
    <button
      type="button"
      class="rail-row status-{n.status}"
      class:is-selected={n.id === selectedId}
      class:is-branch={(branching.get(n.layer) ?? 1) > 1}
      onclick={() => onNodeClick?.(n.id)}
      aria-current={n.id === selectedId ? 'step' : undefined}
    >
      <span class="rail-lamp"></span>
      <span class="rail-title">{n.title}</span>
    </button>
  {/each}
</nav>

<style>
  .rail {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .rail-row {
    display: flex;
    align-items: baseline;
    gap: 8px;
    width: 100%;
    text-align: left;
    background: none;
    border: 0;
    border-radius: 5px;
    padding: 6px 8px;
    color: inherit;
    font: inherit;
    cursor: pointer;
    min-width: 0;
  }
  .rail-row:hover {
    background: var(--hairline);
  }
  /* The one being worked reads as the anchor, because the wide panel
     to its left is showing it. */
  .rail-row.is-selected {
    background: var(--hairline);
    box-shadow: inset 2px 0 0 var(--signal);
  }
  /* Siblings of a fork sit in from the spine: "one of these", without
     an edge there is no room to draw. */
  .rail-row.is-branch {
    margin-left: 12px;
  }
  .rail-lamp {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex: 0 0 auto;
    background: var(--static);
  }
  /* Status is the lamp, never the label — a rail of coloured words is
     unreadable at this width. */
  .rail-row.status-completed .rail-lamp { background: var(--ok); }
  .rail-row.status-active .rail-lamp { background: var(--signal); }
  .rail-row.status-ready .rail-lamp { background: var(--warn); }
  .rail-row.status-skipped .rail-lamp { background: transparent; box-shadow: inset 0 0 0 1px var(--static); }
  .rail-row.status-pending .rail-lamp { background: var(--hairline); box-shadow: inset 0 0 0 1px var(--static); }

  .rail-title {
    font-size: 12px;
    line-height: 1.35;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* A step nobody will run should not compete with the live ones. */
  .rail-row.status-skipped .rail-title {
    color: var(--static);
    text-decoration: line-through;
  }
</style>
