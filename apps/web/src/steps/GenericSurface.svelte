<script lang="ts">
  // Generic step surface — fallback for kinds without a specialised
  // view. Doubles as the "assign tech / reschedule" affordance that
  // every service Job's steps pick up implicitly. Port of
  // apps/web-legacy/src/steps/GenericSurface.tsx.

  import {
    isPending,
    isTerminal as _isTerminal,
    type StepStatus,
    type StepField,
  } from '../jobs/types';
  import type { Employee } from '../people/types';
  import { putStep } from './stepWrite';
  import { PROCEDURE_KEY } from './procedure';

  type StepData = {
    id: string;
    kind: string;
    title: string;
    status: StepStatus;
    assignee_id: string | null;
    metadata: Record<string, unknown>;
    notes: string | null;
    /// The step's completion contract. Declared on the Workflow step
    /// (inline authoring), so it is data rather than a bespoke
    /// surface — which is exactly why this generic view can honour it.
    fields?: StepField[];
  };

  type Props = {
    step: StepData;
    jobId: string;
    onUpdate: () => void;
  };
  let { step, jobId, onUpdate }: Props = $props();

  const initialDueOn =
    typeof step.metadata.due_on === 'string' ? step.metadata.due_on : '';

  /// Values for the step's declared fields, seeded from whatever is
  /// already in metadata.
  ///
  /// Without this the surface could not complete a step that declares
  /// a required field: it sent `status: completed` with the existing
  /// metadata and the API refused it. That made every such step a dead
  /// end everywhere except a bespoke plugin — which defeats inline
  /// field authoring, whose whole point is that a Workflow can state
  /// its own contract without one.
  let fieldValues = $state<Record<string, string>>(
    Object.fromEntries(
      (step.fields ?? []).map((f) => {
        const v = step.metadata[f.name];
        return [f.name, typeof v === 'string' ? v : ''];
      }),
    ),
  );

  /// A pipe-shaped `field_type` is an enum domain — the same shape the
  /// Workflow viability lint reads to prove fork coverage. Anything
  /// else is free text.
  function optionsFor(f: StepField): string[] | null {
    return f.field_type.includes('|') ? f.field_type.split('|') : null;
  }

  let missingRequired = $derived(
    (step.fields ?? []).filter((f) => f.required && !fieldValues[f.name]?.trim()),
  );

  let notes = $state(step.notes ?? '');
  let assigneeId = $state(step.assignee_id ?? '');
  let dueOn = $state(initialDueOn);
  let saving = $state(false);
  /// A refused write, rendered inline. The surface keeps its state
  /// exactly as the user left it so the same click can retry; it
  /// never pretends the write landed (packet cc9d7fc6).
  let writeError = $state<string | null>(null);
  $effect(() => {
    // The surface instance is reused when the rail switches steps —
    // an error from step A must not render under step B.
    void step.id;
    writeError = null;
  });
  let terminal = $derived(_isTerminal(step.status));

  let employees = $state<Employee[]>([]);

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/people');
        if (r.ok) {
          const roster = (await r.json()) as Employee[];
          if (!cancelled) employees = roster;
        }
      } catch {
        // ignore
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let empNames = $derived.by(() => {
    const m = new Map<string, string>();
    for (const e of employees) m.set(e.id, e.name ?? "");
    return m;
  });


  let assigneeDirty = $derived(
    (assigneeId || null) !== (step.assignee_id ?? null),
  );
  let dueOnDirty = $derived((dueOn || '') !== initialDueOn);
  let dirty = $derived(assigneeDirty || dueOnDirty);

  let activeEmployees = $derived(
    [...employees].sort((a, b) => (a.name ?? "").localeCompare(b.name ?? "")),
  );

  function mergeMetadata(
    existing: Record<string, unknown>,
    d: string,
  ): Record<string, unknown> {
    const next = { ...existing };
    if (d) next.due_on = d;
    else delete next.due_on;
    return next;
  }

  async function persist(overrides: {
    status?: string;
    assignee_id?: string | null;
    metadata?: Record<string, unknown>;
    notes?: string;
  }): Promise<void> {
    saving = true;
    writeError = null;
    try {
      const body = {
        ...step,
        job_id: jobId,
        notes: overrides.notes ?? notes ?? undefined,
        status: overrides.status ?? step.status,
        assignee_id:
          overrides.assignee_id !== undefined
            ? overrides.assignee_id
            : assigneeId || null,
        metadata:
          overrides.metadata ?? {
            ...mergeMetadata(step.metadata, dueOn),
            // Only send fields the operator actually filled — an
            // empty string is not an answer, and writing one would
            // satisfy a required-field check with nothing in it.
            ...Object.fromEntries(
              Object.entries(fieldValues).filter(([, v]) => v.trim() !== ''),
            ),
          },
      };
      const res = await putStep(jobId, step.id, body);
      if (res.kind === 'failed') {
        writeError = res.error;
        return;
      }
      onUpdate();
    } finally {
      saving = false;
    }
  }

  /// System/structured leftovers only — human-written string context
  /// renders as prose via contextEntries; declared fields render as
  /// the form. What remains (objects, flags) shows as a small dump.
  let extraMetadataEntries = $derived.by(() => {
    const declared = new Set((step.fields ?? []).map((f) => f.name));
    return Object.entries(step.metadata ?? {}).filter(
      ([k, v]) =>
        !declared.has(k) &&
        !HIDDEN_KEYS.has(k) &&
        !(typeof v === 'string' && v.trim().length > 0),
    );
  });

  /// Undeclared string metadata is CONTEXT someone wrote for the
  /// operator (a decision brief, options, an agent's analysis) — it
  /// was invisible because only declared fields render, which turned
  /// context-rich steps into bare forms. Internal routing keys stay
  /// hidden.
  const HIDDEN_KEYS = new Set([
    'authority_role', 'due_on', 'notify_on_done', 'trigger_kind', 'trigger_name',
    // The decision panel (DecisionContext, mounted by StepSurface
    // above every platform surface) already renders context_md as the
    // step's brief. Re-dumping it here printed the same brief twice on
    // one screen — the second time as a flattened raw-markdown wall
    // (browser-verified on the live gateway, 2026-08-19).
    'context_md',
    // The step's procedure, for the same reason one line up: StepProcedure
    // (mounted by StepSurface above every surface) renders it as the
    // step's instructions, with its paragraphs intact. This list is
    // where it used to land — unstyled prose under its own lowercase
    // key, below the assignee and due-date rows, newlines collapsed
    // into one wall. The key is imported so the panel and this
    // exclusion cannot disagree about its name (CLAUDE.md §9a).
    PROCEDURE_KEY,
  ]);
  let contextEntries = $derived.by(() => {
    const declared = new Set((step.fields ?? []).map((f) => f.name));
    return Object.entries(step.metadata ?? {})
      .filter(([k, v]) =>
        !declared.has(k) && !HIDDEN_KEYS.has(k) &&
        typeof v === 'string' && v.trim().length > 0)
      .map(([k, v]) => ({ key: k.replaceAll('_', ' '), value: v as string }));
  });
</script>

<div class="step-surface step-generic">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-kind-label">{step.kind}</span>
    <span class="step-status step-status-{step.status}">{step.status}</span>
  </div>

  <div class="step-field step-assign-row">
    <label for={`assignee-${step.id}`}>Assignee</label>
    <select
      id={`assignee-${step.id}`}
      bind:value={assigneeId}
      disabled={terminal || saving}
    >
      <option value="">— unassigned —</option>
      {#each activeEmployees as e (e.id)}
        <option value={e.id}>{e.name} · {e.role}</option>
      {/each}
    </select>
    {#if step.assignee_id && !assigneeDirty}
      <span class="step-meta-row small">
        ({empNames.get(step.assignee_id) ?? step.assignee_id})
      </span>
    {/if}
  </div>

  <div class="step-field step-assign-row">
    <label for={`due-${step.id}`}>Due on</label>
    <input
      id={`due-${step.id}`}
      type="date"
      bind:value={dueOn}
      disabled={terminal || saving}
    />
  </div>

  {#if contextEntries.length > 0}
    <!-- Human-written context (a decision brief, options, an agent's
         analysis). This existed in metadata and never rendered — the
         step page was "start buttons with no context" (2026-08-10). -->
    <div class="gs-context">
      {#each contextEntries as c (c.key)}
        <div class="gs-context-item">
          <span class="gs-context-k">{c.key}</span>
          <p class="gs-context-v">{c.value}</p>
        </div>
      {/each}
    </div>
  {/if}

  {#if (step.fields ?? []).length > 0 && !terminal}
    <!-- The step's own completion contract, rendered from data.
         Validators run at `completed`, so a required field missing
         here is not a warning — it is a step that cannot close.
         Independent of any metadata: the form's presence depends on
         the CONTRACT, not on whether context happens to exist (they
         were tangled, and a context-less step lost its form). -->
    <div class="step-fields">
      {#each step.fields ?? [] as f (f.name)}
        {@const options = optionsFor(f)}
        <label class="step-field">
          <span class="step-field-label">
            {f.name.replace(/_/g, ' ')}{#if f.required}<span
                class="step-field-required"
                aria-hidden="true">*</span
              >{/if}
          </span>
          {#if options}
            <select class="step-field-input" bind:value={fieldValues[f.name]}>
              <option value="">Choose…</option>
              {#each options as o (o)}
                <option value={o}>{o}</option>
              {/each}
            </select>
          {:else}
            <input
              class="step-field-input"
              type="text"
              bind:value={fieldValues[f.name]}
              placeholder={f.field_type}
            />
          {/if}
        </label>
      {/each}
    </div>
  {/if}

  {#if extraMetadataEntries.length > 0}
    <div class="step-metadata-display">
      {#each extraMetadataEntries as [k, v] (k)}
        <div class="step-meta-row">
          <strong>{k}:</strong>
          {typeof v === 'object' ? JSON.stringify(v) : String(v)}
        </div>
      {/each}
    </div>
  {/if}

  <div class="step-field">
    <label for={`notes-${step.id}`}>Notes</label>
    <textarea
      id={`notes-${step.id}`}
      rows="2"
      bind:value={notes}
      placeholder="Add notes..."
      disabled={terminal}
    ></textarea>
  </div>

  {#if writeError}
    <p class="step-write-error" role="alert">{writeError}</p>
  {/if}

  <div class="step-actions">
    {#if dirty && !terminal}
      <button
        class="step-btn"
        onclick={() => persist({})}
        disabled={saving}
      >
        {saving ? 'Saving…' : 'Save assignment'}
      </button>
    {/if}
    {#if !terminal && isPending(step.status)}
      <button
        class="step-btn step-btn-primary"
        onclick={() => persist({ status: 'active' })}
        disabled={saving}
      >
        Start
      </button>
    {/if}
    {#if !terminal && step.status === 'active'}
      <button
        class="step-btn step-btn-primary"
        onclick={() => persist({ status: 'completed' })}
        disabled={saving || missingRequired.length > 0}
        title={missingRequired.length > 0
          ? `Needs ${missingRequired.map((f) => f.name).join(', ')}`
          : undefined}
      >
        Complete
      </button>
    {/if}
  </div>
</div>
