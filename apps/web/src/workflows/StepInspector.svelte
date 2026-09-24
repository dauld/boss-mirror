<!--
  Node inspector (Slice 2). Edits the step selected on the canvas — a
  focused subset of the full list editor (StepDagEditor) for one step.
  The parent mounts this inside `{#key selected}` so the local edit
  buffers (slug draft, metadata JSON) reset cleanly each time a
  different node is selected; no reset-effects needed.

  Slug is identity (D1): renaming routes through `onrename`, which the
  parent applies with `renameSlug` so every `ready_when` reference is
  rewritten. Every other field is a plain `onpatch`.
-->
<script lang="ts">
  import { untrack } from 'svelte';
  import type { StepSpec } from './workflowTypes';
  import type { StepTypeInfo } from './stepTypes';
  import type { LintProblem } from './liveLint';

  type Props = Readonly<{
    step: StepSpec;
    stepTypes: ReadonlyArray<StepTypeInfo>;
    /// Other steps' slugs — offered as `ready_when` reference hints.
    siblingSlugs: ReadonlyArray<string>;
    /// Server dry-run problems scoped to this step.
    problems?: ReadonlyArray<LintProblem>;
    onpatch: (patch: Partial<StepSpec>) => void;
    onrename: (to: string) => void;
    onremove: () => void;
    onclose: () => void;
  }>;
  let {
    step,
    stepTypes,
    siblingSlugs,
    problems = [],
    onpatch,
    onrename,
    onremove,
    onclose,
  }: Props = $props();

  // Local buffers — seeded once on mount (the parent remounts per
  // selection via `{#key}`), committed up on change/input. `untrack`
  // documents the intentional snapshot read: the buffers must NOT
  // re-derive from `step` on every patch, or typing a slug/JSON would
  // fight the committed value.
  let slugDraft = $state(untrack(() => step.title));
  let metaText = $state(
    untrack(() => JSON.stringify(step.metadata_defaults, null, 2)),
  );
  let metaError = $state<string | null>(null);

  function commitSlug(): void {
    const next = slugDraft.trim();
    if (next.length === 0) {
      // Don't orphan the selection to an empty slug — revert.
      slugDraft = step.title;
      return;
    }
    if (next !== step.title) onrename(next);
  }

  function onMetaInput(raw: string): void {
    metaText = raw;
    try {
      const parsed = JSON.parse(raw) as unknown;
      if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
        metaError = 'metadata_defaults must be a JSON object.';
        return;
      }
      metaError = null;
      onpatch({ metadata_defaults: parsed as Record<string, unknown> });
    } catch (e) {
      metaError = e instanceof Error ? e.message : 'Invalid JSON';
    }
  }

  const isTerminal = $derived(step.terminal != null);
</script>

<aside class="jk-inspector">
  <div class="jk-insp-head">
    <span class="jk-insp-title">Step · <span class="mono">{step.title}</span></span>
    <button type="button" class="jk-insp-x" title="Close" onclick={onclose}>×</button>
  </div>

  {#each problems as p (p.reason + p.message)}
    <div class="jk-insp-problem">⚠ {p.message}</div>
  {/each}

  <label class="jk-field">
    <span class="jk-field-label">
      Slug
      <span class="jk-field-hint">identity — rename rewrites every reference</span>
    </span>
    <input
      class="mono"
      type="text"
      bind:value={slugDraft}
      onchange={commitSlug}
      onblur={commitSlug}
      placeholder="demand-check"
    />
  </label>

  <label class="jk-field">
    <span class="jk-field-label">Step type</span>
    <select
      value={step.kind}
      onchange={(e) => onpatch({ kind: (e.target as HTMLSelectElement).value })}
    >
      {#if stepTypes.length === 0}
        <option value={step.kind}>{step.kind}</option>
      {/if}
      {#each stepTypes as t (t.kind)}
        <option value={t.kind}>{t.label} ({t.kind})</option>
      {/each}
    </select>
  </label>

  <label class="jk-field">
    <span class="jk-field-label">
      ready_when
      <span class="jk-field-hint">
        {#if siblingSlugs.length > 0}
          refs: {#each siblingSlugs as s, i (s)}<code>steps.{s}.done</code>{#if i < siblingSlugs.length - 1}, {/if}{/each}
        {:else}
          use <code>true</code> for the opening trigger
        {/if}
      </span>
    </span>
    <input
      class="mono"
      type="text"
      value={step.ready_when}
      oninput={(e) => onpatch({ ready_when: (e.target as HTMLInputElement).value })}
      placeholder="true"
    />
  </label>

  <label class="jk-field">
    <span class="jk-field-label">
      Title template
      <span class="jk-field-hint">blank → humanized slug</span>
    </span>
    <input
      type="text"
      value={step.title_template}
      oninput={(e) => onpatch({ title_template: (e.target as HTMLInputElement).value })}
      placeholder={'Demand check — {subject.id}'}
    />
  </label>

  <label class="jk-field">
    <span class="jk-field-label">Authority role <span class="jk-field-hint">optional</span></span>
    <input
      class="mono"
      type="text"
      value={step.authority_role ?? ''}
      oninput={(e) => {
        const v = (e.target as HTMLInputElement).value;
        onpatch({ authority_role: v ? v : null });
      }}
      placeholder="head-brewer"
    />
  </label>

  <label class="jk-field">
    <span class="jk-field-label">Sign-off roles <span class="jk-field-hint">comma-separated</span></span>
    <input
      type="text"
      value={(step.sign_offs_required ?? []).join(', ')}
      onchange={(e) =>
        onpatch({
          sign_offs_required: (e.target as HTMLInputElement).value
            .split(',')
            .map((r) => r.trim())
            .filter(Boolean),
        })}
      placeholder="qa-lead, bookkeeper"
    />
  </label>

  <label class="jk-field">
    <span class="jk-field-label">
      Assurance
      <span class="jk-field-hint">presence = passkey signs the step's content</span>
    </span>
    <select
      value={step.assurance_required ?? ''}
      onchange={(e) => {
        const v = (e.target as HTMLSelectElement).value;
        onpatch({
          assurance_required: v === 'presence' ? 'presence' : null,
        });
      }}
    >
      <option value="">session (default)</option>
      <option value="presence">presence</option>
    </select>
  </label>

  <div class="jk-field jk-field-terminal">
    <label class="jk-check">
      <input
        type="checkbox"
        checked={isTerminal}
        onchange={(e) =>
          onpatch({
            terminal: (e.target as HTMLInputElement).checked
              ? { outcome: step.terminal?.outcome || 'completed' }
              : null,
          })}
      />
      terminal — closes the Job
    </label>
    {#if isTerminal}
      <input
        class="mono jk-outcome"
        type="text"
        value={step.terminal?.outcome ?? ''}
        oninput={(e) => onpatch({ terminal: { outcome: (e.target as HTMLInputElement).value } })}
        placeholder="completed"
        title="Outcome the Job closes with"
      />
    {/if}
  </div>

  <label class="jk-field">
    <span class="jk-field-label">
      metadata_defaults
      <span class="jk-field-hint">JSON object seeded on every instance</span>
    </span>
    <textarea
      class="mono jk-meta"
      rows="3"
      value={metaText}
      oninput={(e) => onMetaInput((e.target as HTMLTextAreaElement).value)}
    ></textarea>
    {#if metaError}<span class="jk-meta-error">{metaError}</span>{/if}
  </label>

  <button type="button" class="jk-insp-remove" onclick={onremove}>Remove step</button>
</aside>

<style>
  .jk-inspector {
    width: 320px;
    flex: 0 0 320px;
    border: 1px solid var(--hairline);
    border-radius: 8px;
    background: var(--ink);
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    align-self: stretch;
    overflow-y: auto;
    max-height: 460px;
  }
  .jk-insp-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .jk-insp-title {
    font-size: 13px;
    font-weight: 600;
    color: var(--static);
  }
  .jk-insp-x {
    width: 24px;
    height: 24px;
    border: 1px solid var(--hairline);
    border-radius: 6px;
    background: var(--ink-raised);
    cursor: pointer;
    font-size: 14px;
    line-height: 1;
  }
  .jk-insp-problem {
    color: var(--err);
    background: var(--err-wash);
    border: 1px solid var(--troubled);
    border-radius: 6px;
    font-size: 12px;
    padding: 5px 8px;
  }
  .jk-field {
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: 12px;
  }
  .jk-field-label {
    color: var(--static);
    font-weight: 500;
  }
  .jk-field-hint {
    color: var(--static);
    font-weight: 400;
    margin-left: 4px;
  }
  .jk-field-hint code {
    background: var(--ink-raised);
    padding: 0 3px;
    border-radius: 3px;
    font-size: 11px;
  }
  .jk-field input,
  .jk-field select,
  .jk-field textarea {
    padding: 5px 7px;
    font-size: 13px;
    border: 1px solid var(--hairline);
    border-radius: 5px;
    width: 100%;
    box-sizing: border-box;
  }
  .jk-field-terminal {
    flex-direction: row;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .jk-check {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 13px;
  }
  .jk-check input {
    width: auto;
  }
  .jk-outcome {
    max-width: 160px;
  }
  .jk-meta {
    line-height: 1.4;
    resize: vertical;
  }
  .jk-meta-error {
    color: var(--err);
    font-size: 11px;
  }
  .jk-insp-remove {
    margin-top: 4px;
    align-self: flex-start;
    font-size: 12px;
    padding: 5px 12px;
    border: 1px solid var(--troubled);
    border-radius: 6px;
    background: var(--err-wash);
    color: var(--err);
    cursor: pointer;
  }
</style>
