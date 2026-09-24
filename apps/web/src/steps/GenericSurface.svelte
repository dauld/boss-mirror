<script lang="ts">
  // Generic step surface — fallback for kinds without a specialised
  // view. Doubles as the "assign tech / reschedule" affordance that
  // every service Job's steps pick up implicitly. Port of
  // apps/web-legacy/src/steps/GenericSurface.tsx.

  import type { Snippet } from 'svelte';
  import {
    isPending,
    isTerminal as _isTerminal,
    type StepStatus,
    type StepField,
  } from '../jobs/types';
  import type { SpecStep } from '../jobs/fork';
  import type { Employee } from '../people/types';
  import { saveStep } from './stepWrite';
  import { PROCEDURE_KEY } from './procedure';
  import {
    askRoutes,
    completeLabel,
    missingRequired as missingOf,
    needsLine,
    optionsFor,
    specSlugOf,
  } from './stepAsk';

  type StepData = {
    id: string;
    kind: string;
    title: string;
    /// The authored step name inside its Workflow — what the spec's
    /// predicates refer to. On the wire; optional for older callers.
    spec_slug?: string;
    status: StepStatus;
    assignee_id: string | null;
    metadata: Record<string, unknown>;
    notes: string | null;
    /// The step's completion contract. Declared on the Workflow step
    /// (inline authoring), so it is data rather than a bespoke
    /// surface — which is exactly why this generic view can honour it.
    fields?: StepField[];
  };

  type Props = Readonly<{
    step: StepData;
    jobId: string;
    onUpdate: () => void;
    /// The step's case — the decision-context panel — rendered by the
    /// dispatcher INSIDE this card, under the title and above the
    /// form. It used to sit above the card, so the order on screen
    /// was brief, title, assignee, due date, form: the one thing the
    /// step asks for came fifth (feedback 26ae4d44).
    children?: Snippet;
  }>;
  let { step, jobId, onUpdate, children }: Props = $props();

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

  let missingRequired = $derived(missingOf(step.fields ?? [], fieldValues));
  let needs = $derived(needsLine(missingRequired));

  /// The Workflow's step graph, read so each enum option can say which
  /// step it opens (stepAsk). Fetched only when the step has an enum
  /// field to explain; the job says which kind and which VERSION the
  /// packet is pinned to, and that version's spec is the one whose
  /// predicates will actually route this answer. A failed read leaves
  /// every route null — the select still shows its words, the button
  /// still says Complete — so the surface degrades, never blanks.
  let specSteps = $state<ReadonlyArray<SpecStep> | null>(null);
  let hasEnumField = $derived((step.fields ?? []).some((f) => optionsFor(f) !== null));
  $effect(() => {
    if (!hasEnumField) return;
    let cancelled = false;
    void (async () => {
      try {
        const jr = await fetch(`/api/jobs/${jobId}`, { headers: { accept: 'application/json' } });
        if (!jr.ok || cancelled) return;
        const job = (await jr.json()) as { kind?: string; workflow_version?: number };
        if (cancelled || !job.kind) return;
        const kind = encodeURIComponent(job.kind);
        const path =
          typeof job.workflow_version === 'number'
            ? `/api/workflows/${kind}/versions/${job.workflow_version}`
            : `/api/workflows/${kind}`;
        const sr = await fetch(path, { headers: { accept: 'application/json' } });
        if (!sr.ok || cancelled) return;
        const spec = (await sr.json()) as { steps?: unknown };
        if (cancelled) return;
        specSteps = Array.isArray(spec.steps) ? (spec.steps as SpecStep[]) : null;
      } catch {
        // No graph is a quiet absence: the words render without routes.
      }
    })();
    return () => {
      cancelled = true;
    };
  });
  let specSlug = $derived(specSlugOf(step, specSteps));
  let routesByField = $derived(
    new Map(
      (step.fields ?? [])
        .filter((f) => optionsFor(f) !== null)
        .map((f) => [f.name, askRoutes(specSteps, specSlug, f)]),
    ),
  );
  /// The fork field — the first enum field — is the one whose chosen
  /// value names the route the Complete control takes.
  let forkField = $derived((step.fields ?? []).find((f) => optionsFor(f) !== null) ?? null);
  let completeText = $derived(
    forkField
      ? completeLabel(routesByField.get(forkField.name) ?? [], fieldValues[forkField.name] ?? '')
      : 'Complete',
  );
  /// The ask renders whenever the step has a contract and a person can
  /// still answer it. `ready` is included: an assigned triage step IS
  /// ready, and the API accepts completion from there (TriageFlow has
  /// completed forks from ready since it existed). Making a person
  /// press Start first, then find Complete, was half of what stopped
  /// David on 2026-09-15.
  let hasAsk = $derived(
    (step.fields ?? []).length > 0 && (step.status === 'ready' || step.status === 'active'),
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

  async function persist(overrides: {
    status?: string;
    assignee_id?: string | null;
    notes?: string;
  }): Promise<void> {
    saving = true;
    writeError = null;
    try {
      // Only the keys this surface owns go to the merge door — never a
      // spread of the step's metadata (backlog e39a9d2a). A cleared due
      // date is an explicit null, which the door deletes; it used to be
      // cleared by OMISSION from a wholesale PUT.
      const body = {
        notes: overrides.notes ?? notes ?? undefined,
        status: overrides.status ?? step.status,
        assignee_id:
          overrides.assignee_id !== undefined
            ? overrides.assignee_id
            : assigneeId || null,
        metadata: {
          ...(dueOnDirty ? { due_on: dueOn || null } : {}),
          // Only send fields the operator actually filled — an
          // empty string is not an answer, and writing one would
          // satisfy a required-field check with nothing in it.
          ...Object.fromEntries(
            Object.entries(fieldValues).filter(([, v]) => v.trim() !== ''),
          ),
        },
      };
      const res = await saveStep(jobId, step.id, body);
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

  <!-- The one ask, in reading order: the step's title (its question),
       the case for it (the dispatcher's decision-context panel, passed
       in as children), then the answer — the declared fields and the
       control that records them. Nothing else sits between. The
       assignee, due date and notes are details of the step, and they
       follow the ask (feedback 26ae4d44). -->
  {@render children?.()}

  {#if hasAsk}
    <!-- The step's own completion contract, rendered from data.
         Validators run at `completed`, so a required field missing
         here is not a warning — it is a step that cannot close.
         Independent of any metadata: the form's presence depends on
         the CONTRACT, not on whether context happens to exist (they
         were tangled, and a context-less step lost its form). -->
    <div class="step-fields step-ask">
      {#each step.fields ?? [] as f (f.name)}
        {@const options = optionsFor(f)}
        {@const routes = routesByField.get(f.name) ?? []}
        <label class="step-field">
          <span class="step-field-label">
            {f.name.replace(/_/g, ' ')}{#if f.required}<span
                class="step-field-required"
                title="required">*</span
              >{/if}
          </span>
          {#if options}
            <!-- Each option carries the step it opens, read off the
                 Workflow's predicates: "build — Build the change". The
                 bare word was the whole label before, and six bare
                 words are not a choice a person can make unaided. -->
            <select class="step-field-input" bind:value={fieldValues[f.name]}>
              <option value="">Choose…</option>
              {#each routes as r (r.value)}
                <option value={r.value}>{r.route ? `${r.value} — ${r.route}` : r.value}</option>
              {/each}
            </select>
          {:else if f.field_type === 'string'}
            <!-- Free text is prose (evidence, a finding, a brief), and a
                 one-line box crammed a whole markdown brief into one
                 scrolling line. -->
            <textarea class="step-field-input" rows="2" bind:value={fieldValues[f.name]}
            ></textarea>
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
      <div class="step-ask-actions">
        <!-- Labelled with its effect — the route the chosen answer
             takes — and, when it cannot be pressed, the field it is
             waiting on, in the row rather than a hover title. -->
        <button
          class="btn btn-primary"
          onclick={() => persist({ status: 'completed' })}
          disabled={saving || missingRequired.length > 0}
        >
          {completeText}
        </button>
        {#if needs}
          <span class="step-ask-needs">{needs}</span>
        {:else}
          <span class="step-ask-needs step-ask-legend">* required</span>
        {/if}
      </div>
    </div>
  {/if}

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
        class="btn"
        onclick={() => persist({})}
        disabled={saving}
      >
        {saving ? 'Saving…' : 'Save assignment'}
      </button>
    {/if}
    {#if !terminal && isPending(step.status)}
      <button
        class="btn btn-primary"
        onclick={() => persist({ status: 'active' })}
        disabled={saving}
      >
        Start
      </button>
    {/if}
    {#if !terminal && step.status === 'active' && !hasAsk}
      <!-- A step with no contract completes here; one WITH a contract
           completes from the ask above, where the answer is. -->
      <button
        class="btn btn-primary"
        onclick={() => persist({ status: 'completed' })}
        disabled={saving}
      >
        Complete
      </button>
    {/if}
  </div>
</div>

<style>
  .step-ask {
    border: 1px solid var(--border);
    border-left: 3px solid var(--accent);
    border-radius: 6px;
    padding: 10px 12px;
    margin-bottom: 12px;
  }
  .step-ask-actions {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    margin-top: 8px;
  }
  .step-ask-needs {
    font-size: 12px;
    color: var(--text-dim);
  }
  /* Enamel's field label, the board's `.field label` (backlog 6f471ff6). */
  .step-field-label {
    font-size: 13.5px;
    font-weight: 700;
    color: var(--text);
  }
  .step-ask-legend {
    margin-left: auto;
  }
  .step-field-required {
    color: var(--err);
    margin-left: 2px;
  }
</style>
