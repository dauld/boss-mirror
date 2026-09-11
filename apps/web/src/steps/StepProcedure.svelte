<script lang="ts">
  // The step's own instructions, rendered above the thing it asks you
  // to do (backlog 3c640ac3).
  //
  // The protocol row already says what doing this step right looks
  // like — what the step is for, what counts as a legitimate answer,
  // what not to do. That prose reached nobody: the generic surface
  // dumped it unstyled among the rest of metadata with its key for a
  // heading, and no plugin bundle read it. David completed `fold` on
  // design-doc dacea60d exactly as its procedure prescribes and still
  // could not tell whether he had, because the sentence that says so
  // was on the screen's other side of a fetch.
  //
  // Same treatment as DecisionContext, deliberately: the two panels sit
  // together above every surface and answer adjacent questions — that
  // one says what this step is DECIDING, this one says how it is DONE.
  // Prose renders through web-kit's escape-first markdown renderer
  // (2244db9e), safe for {@html} because every character is escaped
  // before any tag the renderer emits.
  import { renderMarkdown } from '@boss/web-kit/markdown';
  import { procedureFromStep } from './procedure';

  type Props = {
    step: { metadata: Record<string, unknown> };
  };
  let { step }: Props = $props();

  // No fetch and no fallback chain: a procedure is authored ON the step
  // by the Workflow it came from. Absent means absent — the panel does
  // not appear, and a step without one renders exactly as before.
  let procedure = $derived(procedureFromStep(step.metadata));
  let collapsed = $state(false);
</script>

{#if procedure}
  <div class="step-procedure">
    <button
      type="button"
      class="sp-head"
      onclick={() => (collapsed = !collapsed)}
    >
      <span class="sp-title">How this step is done</span>
      <span class="sp-source">from the protocol</span>
      <span class="sp-toggle">{collapsed ? 'show' : 'hide'}</span>
    </button>
    {#if !collapsed}
      <!-- eslint-disable-next-line svelte/no-at-html-tags — renderMarkdown escapes first -->
      <div class="sp-body">{@html renderMarkdown(procedure)}</div>
    {/if}
  </div>
{/if}

<style>
  .step-procedure {
    border: 1px solid var(--border, #e7e5e4);
    border-left: 3px solid var(--text-dim, #78716c);
    border-radius: 6px;
    background: var(--card, #fff);
    margin-bottom: 12px;
  }
  .sp-head {
    display: flex;
    align-items: baseline;
    gap: 10px;
    width: 100%;
    padding: 8px 12px;
    background: none;
    border: 0;
    cursor: pointer;
    text-align: left;
  }
  .sp-title {
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-dim, #78716c);
  }
  .sp-source {
    font-size: 11px;
    color: var(--text-dim, #78716c);
    flex: 1 1 auto;
  }
  .sp-toggle {
    font-size: 11px;
    color: var(--accent, #2563eb);
  }
  .sp-body {
    padding: 0 12px 10px;
    font-size: 13px;
    line-height: 1.6;
    color: var(--text, #1c1917);
    word-break: break-word;
    max-height: 22em;
    overflow-y: auto;
  }
  /* Rendered-markdown children ({@html} output is outside Svelte's
     scoping, so :global). Same rhythm as the decision panel. */
  .sp-body :global(p),
  .sp-body :global(ul),
  .sp-body :global(ol),
  .sp-body :global(blockquote) {
    margin: 0 0 8px;
  }
  .sp-body :global(pre) {
    background: var(--bg, #f5f5f4);
    padding: 8px 10px;
    border-radius: 5px;
    overflow-x: auto;
    font-size: 12px;
  }
  .sp-body :global(code) {
    background: var(--bg, #f5f5f4);
    padding: 1px 4px;
    border-radius: 3px;
    font-size: 0.9em;
  }
</style>
