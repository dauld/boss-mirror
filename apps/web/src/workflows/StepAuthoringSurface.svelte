<!--
  The graphical step-authoring surface (Slice 2) — shared by the New
  and Edit Workflow pages so neither duplicates the canvas wiring.

  Composes: a StepPalette (add by type) · the interactive graph
  (lazy-loaded per D2) · a StepInspector for the selected node · the
  full StepDagEditor list as an expandable power view. Owns the single
  step-types fetch (passed to palette + inspector) and the debounced
  server dry-run lint (the same validate_all the publish path runs —
  Slice 1's /_validate), whose problems badge the graph and the
  inspector.

  Every mutation goes through the pure helpers in stepEdits so the
  rules (fresh-slug, rename-rewrites-references) live in one tested
  place. The component is otherwise stateless about the spec: it owns
  only transient UI state (selection, lint cache) and reflects all step
  changes up via `onChange`.
-->
<script lang="ts">
  import StepPalette from './StepPalette.svelte';
  import StepInspector from './StepInspector.svelte';
  import StepDagEditor from './StepDagEditor.svelte';
  import type { StepSpec } from './workflowTypes';
  import type { StepTypeInfo } from './stepTypes';
  import {
    validateDraft,
    problemsByStep,
    type LintProblem,
  } from './liveLint';
  import { makeStep, patchStep, removeStep, renameSlug } from './stepEdits';

  type Props = Readonly<{
    steps: ReadonlyArray<StepSpec>;
    /// Used only to label the dry-run; the lint validates the graph,
    /// not the slug.
    kindSlug: string;
    onChange: (next: ReadonlyArray<StepSpec>) => void;
  }>;
  let { steps, kindSlug, onChange }: Props = $props();

  // The heavy graph editor (Svelte Flow + dagre) stays in its own chunk
  // (D2). Hoisted so the dynamic import resolves once, not per render.
  const graphModule = import('./WorkflowGraphEditor.svelte');

  let selected = $state<string | null>(null);
  let stepTypes = $state<ReadonlyArray<StepTypeInfo>>([]);
  let lintProblems = $state<Map<string, LintProblem[]>>(new Map());

  let selectedStep = $derived(steps.find((s) => s.title === selected) ?? null);
  let selectedProblems = $derived(
    selected ? (lintProblems.get(selected) ?? []) : [],
  );
  let siblingSlugs = $derived(
    steps
      .filter((s) => s.title !== selected)
      .map((s) => s.title)
      .filter((t) => t.length > 0),
  );

  // One step-types fetch for the whole surface; palette + inspector
  // share it.
  $effect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const r = await fetch('/api/jobs/step-types');
        if (!r.ok) return;
        const data = (await r.json()) as StepTypeInfo[];
        if (!cancelled) stepTypes = data;
      } catch {
        // Non-fatal: palette shows "loading", inspector keeps the
        // current kind as the only option.
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  // Debounced server dry-run. A transient failure leaves the prior
  // result in place so the graph doesn't flicker empty.
  $effect(() => {
    const k = kindSlug;
    const s = steps;
    const t = setTimeout(() => {
      void (async () => {
        try {
          lintProblems = problemsByStep(await validateDraft(k || 'draft', s));
        } catch {
          // keep the prior result
        }
      })();
    }, 400);
    return () => clearTimeout(t);
  });

  function add(kind: string): void {
    const step = makeStep(kind, steps);
    onChange([...steps, step]);
    selected = step.title;
  }
  function patch(p: Partial<StepSpec>): void {
    if (selected) onChange(patchStep(steps, selected, p));
  }
  function rename(to: string): void {
    if (!selected) return;
    onChange(renameSlug(steps, selected, to));
    selected = to;
  }
  function remove(): void {
    if (!selected) return;
    onChange(removeStep(steps, selected));
    selected = null;
  }
</script>

<div class="jk-authoring">
  <StepPalette {stepTypes} onadd={add} />

  <div class="jk-surface-body">
    <div class="jk-canvas">
      {#await graphModule}
        <div class="jk-canvas-msg">Loading graph…</div>
      {:then { default: GraphEditor }}
        <GraphEditor
          {steps}
          problems={lintProblems}
          {selected}
          onselect={(s) => (selected = s)}
        />
      {:catch err}
        <div class="jk-canvas-msg jk-canvas-err">Graph editor failed to load: {err}</div>
      {/await}
    </div>
    {#if selectedStep}
      {#key selected}
        <StepInspector
          step={selectedStep}
          {stepTypes}
          {siblingSlugs}
          problems={selectedProblems}
          onpatch={patch}
          onrename={rename}
          onremove={remove}
          onclose={() => (selected = null)}
        />
      {/key}
    {/if}
  </div>

  <details class="jk-list-details">
    <summary>Edit all steps as a list (every field)</summary>
    <div class="jk-list-body">
      <StepDagEditor value={steps} {onChange} />
    </div>
  </details>
</div>

<style>
  .jk-authoring {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .jk-surface-body {
    display: flex;
    gap: 12px;
    align-items: flex-start;
  }
  .jk-canvas {
    flex: 1 1 auto;
    min-width: 0;
  }
  .jk-canvas-msg {
    padding: 24px;
    color: var(--static);
    font-size: 0.9rem;
  }
  .jk-canvas-err {
    color: var(--err);
  }
  .jk-list-details {
    border: 1px solid var(--hairline);
    border-radius: 8px;
    background: var(--ink);
  }
  .jk-list-details summary {
    cursor: pointer;
    padding: 10px 12px;
    font-size: 13px;
    font-weight: 600;
    color: var(--static);
    user-select: none;
  }
  .jk-list-body {
    padding: 0 12px 12px;
  }
</style>
