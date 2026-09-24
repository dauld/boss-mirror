<script lang="ts">
  // Workflow v2 step editor — a FLAT ordered step list.
  //
  // v1 was a tier-list editor (tiers + edges); v2 deletes that. A
  // Workflow is now a flat list of steps and the DAG is implicit in
  // each step's `ready_when` predicate (it references sibling step
  // slugs as `steps.<slug>.done`). Array order is the authoring
  // order; it carries no execution semantics — readiness is purely
  // predicate-driven. We keep the list ordered anyway because authors
  // think top-to-bottom and it makes the predicates readable.
  //
  // TODO(future): a visual Sugiyama DAG layout (auto-placed nodes +
  // edges derived from the ready_when predicates) is an explicit
  // future refinement. This form-driven editor is the pragmatic
  // first cut.

  import type { StepSpec } from './workflowTypes';
  import { lintSteps, type StepWarning } from './stepValidation';

  type StepTypeInfo = {
    kind: string;
    label: string;
    category: string;
    ux: string;
    description: string;
  };

  type Props = {
    value: ReadonlyArray<StepSpec>;
    onChange: (next: ReadonlyArray<StepSpec>) => void;
  };
  let { value, onChange }: Props = $props();

  let stepTypes = $state<ReadonlyArray<StepTypeInfo>>([]);
  let showJson = $state(false);

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/jobs/step-types');
        if (!r.ok) return;
        const data = (await r.json()) as StepTypeInfo[];
        if (!cancelled) stepTypes = data;
      } catch {
        // ignore — falls back to the current kind as the only option
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  // Inline, non-blocking lint. Recomputed on every change.
  let warnings = $derived<ReadonlyArray<StepWarning>>(lintSteps(value));
  function warningsFor(idx: number): ReadonlyArray<StepWarning> {
    return warnings.filter((w) => w.stepIndex === idx);
  }
  let specWarnings = $derived(warnings.filter((w) => w.stepIndex === null));

  // Sibling slugs available to reference in a step's ready_when, for
  // the per-step hint. Excludes the step's own slug + blanks.
  function siblingSlugs(idx: number): ReadonlyArray<string> {
    return value
      .map((s, i) => ({ slug: s.title.trim(), i }))
      .filter((x) => x.i !== idx && x.slug.length > 0)
      .map((x) => x.slug);
  }

  function updateStep(idx: number, patch: Partial<StepSpec>): void {
    onChange(value.map((s, i) => (i !== idx ? s : { ...s, ...patch })));
  }

  function addStep(): void {
    const fresh: StepSpec = {
      title: '',
      kind: stepTypes[0]?.kind ?? 'generic',
      ready_when: value.length === 0 ? 'true' : '',
      terminal: null,
      title_template: '',
      sign_offs_required: [],
      authority_role: null,
      metadata_defaults: {},
    };
    onChange([...value, fresh]);
  }

  function removeStep(idx: number): void {
    onChange(value.filter((_, i) => i !== idx));
  }

  function moveStep(idx: number, direction: -1 | 1): void {
    const target = idx + direction;
    if (target < 0 || target >= value.length) return;
    const next = [...value];
    [next[idx], next[target]] = [next[target]!, next[idx]!];
    onChange(next);
  }

  function toggleTerminal(idx: number, checked: boolean): void {
    updateStep(idx, { terminal: checked ? { outcome: 'completed' } : null });
  }

  function setOutcome(idx: number, outcome: string): void {
    updateStep(idx, { terminal: { outcome } });
  }

  // metadata_defaults edits as raw JSON (same affordance as v1). We
  // keep the textarea's string state local + per-step so a transient
  // invalid edit doesn't blow away the underlying object; only valid
  // JSON is committed up via onChange.
  let metaText = $state<Record<number, string>>({});
  let metaError = $state<Record<number, string | null>>({});
  function metaValue(idx: number, step: StepSpec): string {
    return metaText[idx] ?? JSON.stringify(step.metadata_defaults, null, 2);
  }
  function onMetaInput(idx: number, raw: string): void {
    metaText = { ...metaText, [idx]: raw };
    try {
      const parsed = JSON.parse(raw) as unknown;
      if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
        metaError = { ...metaError, [idx]: 'metadata_defaults must be a JSON object.' };
        return;
      }
      metaError = { ...metaError, [idx]: null };
      updateStep(idx, { metadata_defaults: parsed as Record<string, unknown> });
    } catch (e) {
      metaError = { ...metaError, [idx]: e instanceof Error ? e.message : 'Invalid JSON' };
    }
  }
</script>

<div class="sde">
  <div class="sde-header">
    <span class="sde-hint">
      Steps are a flat list — the DAG is implicit in each step's
      <code>ready_when</code> predicate. Mark exactly one step
      <code>ready_when = "true"</code> as the trigger that fires when
      the Job opens, and at least one step <em>terminal</em> so the
      Job can close. Order below is authoring order only; readiness is
      predicate-driven.
    </span>
    <button
      type="button"
      class="sde-json-toggle"
      onclick={() => (showJson = !showJson)}
    >
      {showJson ? 'Hide JSON' : 'Show JSON'}
    </button>
  </div>

  <div class="sde-grammar">
    <strong>ready_when grammar:</strong>
    <code>AND OR NOT = != &lt; &lt;= &gt; &gt;=</code> + parentheses.
    Equality is a single <code>=</code>. Vocabulary:
    <code>true</code>, <code>steps.&lt;slug&gt;.done</code>,
    <code>steps.&lt;slug&gt;.metadata.&lt;field&gt;</code>,
    <code>subject.subject_kind</code>, <code>subject.id</code>,
    <code>job.metadata.&lt;field&gt;</code>. e.g.
    <code>steps.demand-check.done AND steps.demand-check.metadata.outcome = "brew"</code>
  </div>

  {#each specWarnings as w (w.message)}
    <div class="sde-warn sde-warn-spec">⚠ {w.message}</div>
  {/each}

  <ol class="sde-steps-list">
    {#each value as step, idx (idx)}
      {@const sibs = siblingSlugs(idx)}
      {@const stepWarnings = warningsFor(idx)}
      <li class="sde-step">
        <div class="sde-step-top">
          <span class="sde-step-num">{idx + 1}</span>
          <div class="sde-step-actions">
            <button
              type="button"
              onclick={() => moveStep(idx, -1)}
              disabled={idx === 0}
              title="Move up"
            >↑</button>
            <button
              type="button"
              onclick={() => moveStep(idx, 1)}
              disabled={idx === value.length - 1}
              title="Move down"
            >↓</button>
            <button
              type="button"
              onclick={() => removeStep(idx)}
              class="sde-step-remove"
              title="Remove step"
            >×</button>
          </div>
        </div>

        <div class="sde-field-grid">
          <label class="sde-field">
            <span class="sde-field-label">
              Slug
              <span class="sde-field-hint">kebab-case, unique; referenced by predicates as steps.&lt;slug&gt;.done</span>
            </span>
            <input
              type="text"
              value={step.title}
              oninput={(e) => updateStep(idx, { title: (e.target as HTMLInputElement).value })}
              placeholder="demand-check"
              class="mono"
            />
          </label>

          <label class="sde-field">
            <span class="sde-field-label">Step type</span>
            <select
              value={step.kind}
              onchange={(e) => updateStep(idx, { kind: (e.target as HTMLSelectElement).value })}
            >
              {#if stepTypes.length === 0}
                <option value={step.kind}>{step.kind}</option>
              {/if}
              {#each stepTypes as t (t.kind)}
                <option value={t.kind}>{t.label} ({t.kind})</option>
              {/each}
            </select>
          </label>

          <label class="sde-field sde-field-wide">
            <span class="sde-field-label">
              ready_when
              {#if sibs.length > 0}
                <span class="sde-field-hint">
                  available: {#each sibs as s, i (s)}<code>steps.{s}.done</code>{#if i < sibs.length - 1}, {/if}{/each}
                </span>
              {:else}
                <span class="sde-field-hint">use <code>true</code> for the opening trigger</span>
              {/if}
            </span>
            <input
              type="text"
              value={step.ready_when}
              oninput={(e) => updateStep(idx, { ready_when: (e.target as HTMLInputElement).value })}
              placeholder="true"
              class="mono"
            />
          </label>

          <label class="sde-field sde-field-wide">
            <span class="sde-field-label">
              Title template
              <span class="sde-field-hint">display string; {'{subject.id}'} expands at runtime; blank → humanized slug</span>
            </span>
            <input
              type="text"
              value={step.title_template}
              oninput={(e) => updateStep(idx, { title_template: (e.target as HTMLInputElement).value })}
              placeholder={'Demand check — {subject.id}'}
            />
          </label>

          <label class="sde-field">
            <span class="sde-field-label">Authority role <span class="sde-field-hint">optional</span></span>
            <input
              type="text"
              value={step.authority_role ?? ''}
              oninput={(e) => {
                const v = (e.target as HTMLInputElement).value;
                updateStep(idx, { authority_role: v ? v : null });
              }}
              placeholder="head-brewer"
              class="mono"
            />
          </label>

          <div class="sde-field sde-field-checks">
            <label class="sde-check sde-signoffs">
              sign-off roles
              <input
                type="text"
                placeholder="e.g. qa-lead, bookkeeper"
                value={(step.sign_offs_required ?? []).join(', ')}
                onchange={(e) => updateStep(idx, { sign_offs_required: (e.target as HTMLInputElement).value.split(',').map((r) => r.trim()).filter(Boolean) })}
              />
            </label>
            <label class="sde-check">
              <input
                type="checkbox"
                checked={step.terminal != null}
                onchange={(e) => toggleTerminal(idx, (e.target as HTMLInputElement).checked)}
              />
              terminal (closes the Job)
            </label>
            {#if step.terminal}
              <input
                type="text"
                value={step.terminal.outcome}
                oninput={(e) => setOutcome(idx, (e.target as HTMLInputElement).value)}
                placeholder="completed"
                class="mono sde-outcome"
                title="Outcome the Job closes with"
              />
            {/if}
          </div>

          <label class="sde-field sde-field-wide">
            <span class="sde-field-label">
              metadata_defaults
              <span class="sde-field-hint">raw JSON object seeded onto every instance of this step</span>
            </span>
            <textarea
              rows="3"
              value={metaValue(idx, step)}
              oninput={(e) => onMetaInput(idx, (e.target as HTMLTextAreaElement).value)}
              class="mono sde-meta"
            ></textarea>
            {#if metaError[idx]}
              <span class="sde-meta-error">{metaError[idx]}</span>
            {/if}
          </label>
        </div>

        {#each stepWarnings as w (w.message)}
          <div class="sde-warn">⚠ {w.message}</div>
        {/each}
      </li>
    {/each}
  </ol>

  <button type="button" class="sde-add-step" onclick={addStep}>+ Add step</button>

  {#if showJson}
    <pre class="sde-json">{JSON.stringify(value, null, 2)}</pre>
  {/if}
</div>

<style>
  .sde {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .sde-header {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    justify-content: space-between;
  }
  .sde-hint {
    font-size: 12px;
    color: var(--static);
    line-height: 1.5;
    max-width: 720px;
  }
  .sde-hint code,
  .sde-grammar code {
    background: var(--ink-raised);
    padding: 0 4px;
    border-radius: 3px;
    font-size: 11px;
  }
  .sde-json-toggle,
  .sde-add-step {
    font-size: 12px;
    padding: 4px 10px;
    border: 1px solid var(--hairline);
    border-radius: 6px;
    background: var(--ink-raised);
    cursor: pointer;
    white-space: nowrap;
  }
  .sde-grammar {
    font-size: 12px;
    color: var(--static);
    line-height: 1.6;
    background: var(--ink-raised);
    border: 1px solid var(--hairline);
    border-radius: 6px;
    padding: 8px 10px;
  }
  .sde-steps-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .sde-step {
    border: 1px solid var(--hairline);
    border-radius: 8px;
    padding: 12px;
    background: var(--ink);
  }
  .sde-step-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 10px;
  }
  .sde-step-num {
    font-size: 12px;
    font-weight: 600;
    color: var(--static);
    background: var(--ink-raised);
    border-radius: 999px;
    width: 22px;
    height: 22px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .sde-step-actions {
    display: flex;
    gap: 4px;
  }
  .sde-step-actions button,
  .sde-step-remove {
    width: 26px;
    height: 26px;
    border: 1px solid var(--hairline);
    border-radius: 6px;
    background: var(--ink-raised);
    cursor: pointer;
    font-size: 13px;
  }
  .sde-step-actions button:disabled {
    opacity: 0.35;
    cursor: not-allowed;
  }
  .sde-step-remove {
    color: var(--err);
  }
  .sde-field-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
  }
  .sde-field {
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: 12px;
  }
  .sde-field-wide {
    grid-column: 1 / -1;
  }
  .sde-field-label {
    color: var(--static);
    font-weight: 500;
  }
  .sde-field-hint {
    color: var(--static);
    font-weight: 400;
    margin-left: 4px;
  }
  .sde-field input,
  .sde-field select,
  .sde-field textarea {
    padding: 5px 7px;
    font-size: 13px;
    border: 1px solid var(--hairline);
    border-radius: 5px;
    width: 100%;
    box-sizing: border-box;
  }
  .sde-field-checks {
    flex-direction: row;
    align-items: center;
    gap: 16px;
    grid-column: 1 / -1;
  }
  .sde-check {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 13px;
  }
  .sde-outcome {
    max-width: 200px;
  }
  .sde-meta {
    font-size: 12px;
    line-height: 1.4;
    resize: vertical;
  }
  .sde-meta-error {
    color: var(--err);
    font-size: 11px;
  }
  .sde-warn {
    color: var(--warn);
    background: var(--warn-wash);
    border: 1px solid var(--busy);
    border-radius: 5px;
    font-size: 12px;
    padding: 4px 8px;
    margin-top: 8px;
  }
  .sde-warn-spec {
    margin-top: 0;
  }
  .sde-json {
    background: var(--ink-raised);
    color: var(--fog);
    padding: 12px;
    border-radius: 8px;
    font-size: 12px;
    overflow: auto;
    max-height: 360px;
  }
</style>
