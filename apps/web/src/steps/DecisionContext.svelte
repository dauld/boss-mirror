<script lang="ts">
  // The packet's case for action, rendered above every non-plugin
  // step surface (19db52de). A sign-off button with no context is not
  // a choice; this panel is the default that makes one. Plugins are
  // exempt — a mounted plugin IS the bespoke presentation.
  //
  // Prose renders through web-kit's escape-first markdown renderer
  // (2244db9e) — safe for {@html} because every character is escaped
  // before any tag the renderer emits.
  import { renderMarkdown } from '@boss/web-kit/markdown';
  import {
    contextFromJob,
    contextFromPriorSteps,
    contextFromStep,
  } from './decisionContext';
  import type { DecisionContext } from './decisionContext';

  type Props = {
    step: { id: string; metadata: Record<string, unknown> };
    jobId: string;
  };
  let { step, jobId }: Props = $props();

  let resolved = $state<DecisionContext | null>(null);
  let collapsed = $state(false);

  $effect(() => {
    const own = contextFromStep(step.metadata);
    if (own) {
      resolved = own;
      return;
    }
    resolved = null;
    let cancelled = false;
    void (async () => {
      try {
        const r = await fetch(`/api/jobs/${jobId}`, {
          headers: { accept: 'application/json' },
        });
        if (!r.ok || cancelled) return;
        const job = (await r.json()) as {
          metadata?: Record<string, unknown>;
          steps?: { title?: string; status?: string; metadata?: Record<string, unknown> }[];
        };
        if (cancelled) return;
        // The job's own briefing, then what earlier steps recorded. The
        // fourth source is what makes a multi-step protocol legible: a
        // retro's case is spread across four completed steps, and a
        // rotation's sits in `scope`. Both reached a decision with this
        // panel empty before it existed.
        resolved =
          contextFromJob(job.metadata ?? {}) ?? contextFromPriorSteps(job.steps ?? []);
      } catch {
        // No context is a quiet absence, never a broken surface.
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  const sourceLabel: Record<DecisionContext['source'], string> = {
    step: 'written for this step',
    'job-body': 'the filed item',
    'prior-steps': 'recorded by earlier steps',
    'job-context': 'the packet’s briefing',
    'job-message': 'the packet as filed',
  };
</script>

{#if resolved}
  <div class="step-decision-context">
    <button
      type="button"
      class="sdc-head"
      onclick={() => (collapsed = !collapsed)}
    >
      <span class="sdc-title">What this step is deciding</span>
      <span class="sdc-source">{sourceLabel[resolved.source]}</span>
      <span class="sdc-toggle">{collapsed ? 'show' : 'hide'}</span>
    </button>
    {#if !collapsed}
      <!-- eslint-disable-next-line svelte/no-at-html-tags — renderMarkdown escapes first -->
      <div class="sdc-body">{@html renderMarkdown(resolved.text)}</div>
    {/if}
  </div>
{/if}

<style>
  .step-decision-context {
    border: 1px solid var(--border, #e7e5e4);
    border-left: 3px solid var(--accent, #2563eb);
    border-radius: 6px;
    background: var(--card, #fff);
    margin-bottom: 12px;
  }
  .sdc-head {
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
  .sdc-title {
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-dim, #78716c);
  }
  .sdc-source {
    font-size: 11px;
    color: var(--text-dim, #78716c);
    flex: 1 1 auto;
  }
  .sdc-toggle {
    font-size: 11px;
    color: var(--accent, #2563eb);
  }
  .sdc-body {
    padding: 0 12px 10px;
    font-size: 13px;
    line-height: 1.6;
    color: var(--text, #1c1917);
    word-break: break-word;
    max-height: 22em;
    overflow-y: auto;
  }
  /* Rendered-markdown children ({@html} output is outside Svelte's
     scoping, so :global). Tight rhythm — this is a briefing card. */
  .sdc-body :global(p),
  .sdc-body :global(ul),
  .sdc-body :global(ol),
  .sdc-body :global(blockquote) {
    margin: 0 0 8px;
  }
  .sdc-body :global(pre) {
    background: var(--bg, #f5f5f4);
    padding: 8px 10px;
    border-radius: 5px;
    overflow-x: auto;
    font-size: 12px;
  }
  .sdc-body :global(code) {
    background: var(--bg, #f5f5f4);
    padding: 1px 4px;
    border-radius: 3px;
    font-size: 0.9em;
  }
  .sdc-body :global(table) {
    border-collapse: collapse;
    font-size: 12px;
    margin: 0 0 8px;
  }
  .sdc-body :global(th),
  .sdc-body :global(td) {
    border: 1px solid var(--border, #e7e5e4);
    padding: 3px 8px;
    text-align: left;
  }
</style>
