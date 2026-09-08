<script lang="ts">
  // /it/design/backlog — the IT backlog, visualized (c1624b94).
  //
  // David, 2026-08-31: "I think we lost a view of the IT backlog in
  // the re-design / consolidation." The queue never stopped existing —
  // q.platform-admin.task holds it — but no page rendered it, so the
  // backlog was API-only from the day /system folded into /it.
  //
  // Everything structural lives in `TriageBoard`/`TriageFlow`, exactly
  // as the feedback queue's page promised: "adding the next triage
  // queue should be a route and a filter, not another board." This
  // file is the route and the filter — kind=backlog-item — plus the
  // card face a backlog item wants: its title (backlog titles are
  // written to be read, unlike feedback's), the area chip, and
  // priority when it is not the default.
  import TriageBoard from '../../jobs/TriageBoard.svelte';
  import TriageFlow from '../../jobs/TriageFlow.svelte';
  import type { Job } from '../../jobs/types';

  let view = $state<'board' | 'flow'>('board');

  /// The pipeline area, when the filer named one — the board's grouping
  /// chip, never a guess.
  function area(j: Job): string | null {
    const a = j.metadata?.['area'];
    return typeof a === 'string' && a.trim() !== '' ? a : null;
  }

  function urgent(j: Job): boolean {
    return j.priority === 'urgent';
  }
</script>

<div class="bl-viewbar">
  <button
    type="button"
    class="bl-view"
    class:active={view === 'board'}
    onclick={() => (view = 'board')}
  >Board</button>
  <button
    type="button"
    class="bl-view"
    class:active={view === 'flow'}
    onclick={() => (view = 'flow')}
  >Flow</button>
</div>

{#if view === 'flow'}
  <TriageFlow
    kind="backlog-item"
    title="IT backlog — the Workflow"
    subtitle="Per-step queues along the backlog Workflow. Select an item at a step, then click an outgoing edge to route it."
  />
{:else}
  <TriageBoard
    kind="backlog-item"
    title="IT backlog"
    subtitle="Every card is a backlog-item Job. Columns are the triage step's state, so a card cannot disagree with the Job behind it."
    emptyMessage="Backlog clear — nothing filed and unrouted."
  >
    {#snippet card(j)}
      <p class="bl-card-title">{j.title}</p>
      <div class="bl-card-meta">
        {#if urgent(j)}
          <span class="bl-chip bl-chip-urgent">urgent</span>
        {/if}
        {#if area(j)}
          <span class="bl-chip bl-chip-area">{area(j)}</span>
        {/if}
        <span class="bl-opened">{j.opened_on}</span>
      </div>
    {/snippet}
  </TriageBoard>
{/if}

<style>
  .bl-card-title {
    margin: 0 0 8px;
    font-size: 13px;
    line-height: 1.45;
    /* Two lines like the feedback cards, so columns scan as lists. */
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .bl-card-meta {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .bl-chip {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    padding: 1px 6px;
    border-radius: 3px;
    font-weight: 600;
  }
  /* Urgent is the one severity the backlog carries; everything else
     is just an area label. */
  .bl-chip-urgent {
    background: #fef2f2;
    color: #b91c1c;
  }
  .bl-chip-area {
    background: var(--bg, var(--void, #0d1014));
    border: 1px solid var(--border, var(--hairline, #2a3138));
    color: inherit;
  }
  .bl-opened {
    font-size: 11px;
    color: var(--text-dim, var(--static, #7a838c));
    margin-left: auto;
  }
  .bl-viewbar {
    display: flex;
    gap: 6px;
    margin: 4px 0 10px;
  }
  .bl-view {
    font: inherit;
    font-size: 12px;
    font-weight: 600;
    padding: 3px 12px;
    border: 1px solid var(--border, var(--hairline, #2a3138));
    border-radius: 999px;
    background: transparent;
    cursor: pointer;
    color: inherit;
  }
  .bl-view.active {
    border-color: var(--signal, #5fd4a8);
    color: var(--signal, #5fd4a8);
  }
</style>
