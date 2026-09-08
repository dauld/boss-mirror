<script lang="ts">
  // /it/design/feedback — the feedback queue.
  //
  // Everything structural lives in `TriageBoard`: the columns, the
  // agent hand-off, completing the gated step. This file is what is
  // actually specific to feedback — which queue to show, what to call
  // it, and the fact that a feedback Job's headline is its message
  // rather than its title ("Feedback on /ux/jobs" tells a triager
  // nothing they cannot already see from the route chip).
  //
  // If that ratio looks lopsided, that is the point. Adding the next
  // triage queue should be a route and a filter, not another board.
  import TriageBoard from '../../jobs/TriageBoard.svelte';
  import TriageFlow from '../../jobs/TriageFlow.svelte';
  import type { Job } from '../../jobs/types';
  import { navigate } from '../../router';

  // Two views of the same queue: Board (flat columns keyed on the
  // triage fork) and Flow (the Workflow DAG with per-step depth and
  // routing edges — 65fa5a1c). Board stays the default; the toggle
  // is view state, not routing state.
  let view = $state<'board' | 'flow'>('board');

  function message(j: Job): string {
    const m = j.metadata?.['message'];
    return typeof m === 'string' ? m : '(no message)';
  }

  /// Bug or feature, if the reporter said. `feedback_kind` arrives on
  /// items filed after the form learned the difference; everything
  /// older has no answer and gets no chip rather than a guessed one —
  /// a wrong label on a card is worse than an absent one.
  function kindOf(j: Job): 'bug' | 'feature' | null {
    const k = j.metadata?.['feedback_kind'];
    return k === 'bug' || k === 'feature' ? k : null;
  }

  /// The first sentence or line, for the card.
  ///
  /// Cards were rendering the WHOLE message, so one long report made a
  /// card four times the height of its neighbours and the column
  /// stopped being scannable — which is what "visually challenging"
  /// meant. Nothing is lost: double-clicking opens the full text.
  ///
  /// Splits on the first line break or sentence end, whichever comes
  /// first, then falls back to a hard clip. A summary that cuts
  /// mid-word reads as broken, so the clip backs up to a space.
  function summary(j: Job): string {
    const full = message(j).trim();
    const firstLine = full.split('\n')[0]!.trim();
    const stop = firstLine.search(/[.!?](\s|$)/);
    const candidate = stop > 20 ? firstLine.slice(0, stop + 1) : firstLine;
    if (candidate.length <= 120) return candidate;
    const cut = candidate.lastIndexOf(' ', 120);
    return `${candidate.slice(0, cut > 40 ? cut : 120)}…`;
  }

  /// Whether the summary is actually shorter than what it summarises —
  /// only then is there a reason to say "there is more".
  function truncated(j: Job): boolean {
    return summary(j).replace(/…$/, '').length < message(j).trim().length;
  }

  /// The surface the feedback is about. Falls back to the Subject id,
  /// which is the same value — the route path IS the Subject id.
  function route(j: Job): string | null {
    const r = j.metadata?.['route'];
    if (typeof r === 'string') return r;
    return j.subject?.id ?? null;
  }
</script>

<div class="fb-viewbar">
  <button
    type="button"
    class="fb-view"
    class:active={view === 'board'}
    onclick={() => (view = 'board')}
  >Board</button>
  <button
    type="button"
    class="fb-view"
    class:active={view === 'flow'}
    onclick={() => (view = 'flow')}
  >Flow</button>
</div>

{#if view === 'flow'}
  <TriageFlow
    kind="user-feedback"
    title="Feedback triage — the Workflow"
    subtitle="Per-step queues along the Workflow. Select an item at a step, then click an outgoing edge to route it."
  />
{:else}
<TriageBoard
  kind="user-feedback"
  title="Feedback triage"
  subtitle="Every item is a user-feedback Job. Columns are the triage step's state, so a card cannot disagree with the Job behind it."
  emptyMessage="No feedback yet. It arrives from the Feedback control in the top bar, on whichever page the person was looking at."
>
  {#snippet card(j)}
    <p class="fb-card-msg">{summary(j)}</p>
    <div class="fb-card-meta">
      {#if kindOf(j)}
        <span class="fb-chip fb-chip-{kindOf(j)}">{kindOf(j)}</span>
      {/if}
      {#if route(j)}
        <button
          class="fb-route"
          type="button"
          onclick={() => navigate(route(j) ?? '/')}
          title="Open the page this is about"
        >
          {route(j)}
        </button>
      {/if}
      {#if truncated(j)}
        <span class="fb-more" title="Double-click the card for the full report">more…</span>
      {/if}
    </div>
  {/snippet}
</TriageBoard>
{/if}

<style>
  .fb-card-msg {
    margin: 0 0 8px;
    font-size: 13px;
    line-height: 1.45;
    /* Two lines, so every card in a column is the same height and the
       column scans as a list rather than a ragged stack. */
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .fb-card-meta {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .fb-chip {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    padding: 1px 6px;
    border-radius: 3px;
    font-weight: 600;
  }
  /* Two different questions, so two different colours: a bug is a
     claim something is wrong, a feature is a claim something is
     missing. Not severity — the board has no severity. */
  .fb-chip-bug {
    background: #fef2f2;
    color: #b91c1c;
  }
  .fb-chip-feature {
    background: #eff6ff;
    color: #1d4ed8;
  }
  .fb-more {
    font-size: 11px;
    color: var(--text-dim, var(--static, #7A838C));
    margin-left: auto;
  }
  .fb-route {
    font: inherit;
    font-size: 11px;
    background: var(--bg, var(--void, #0D1014));
    border: 1px solid var(--border, var(--hairline, #2A3138));
    border-radius: 3px;
    padding: 1px 6px;
    cursor: pointer;
    color: inherit;
  }
  .fb-viewbar {
    display: flex;
    gap: 6px;
    margin: 4px 0 10px;
  }
  .fb-view {
    font: inherit;
    font-size: 12px;
    font-weight: 600;
    padding: 3px 12px;
    border: 1px solid var(--border, var(--hairline, #2A3138));
    border-radius: 999px;
    background: transparent;
    cursor: pointer;
    color: inherit;
  }
  .fb-view.active {
    border-color: var(--signal, #5FD4A8);
    color: var(--signal, #5FD4A8);
  }
</style>
