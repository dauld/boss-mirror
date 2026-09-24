<script lang="ts">
  // Handoff step surface — explicit transfer of work from one
  // role/person/team to another. The brewery uses this for
  // cellar → packaging, packaging → warehouse, the wholesale
  // bookkeeper-finance handoff, etc.
  //
  // Demo framing: from + to are pre-populated by the Workflow.
  // The user picks "I'm the sender, confirm" or "I'm the
  // receiver, confirm." Both ack lights up → step done.

  import { isPending, isTerminal as _isTerminal, type StepStatus } from '../jobs/types';
  import type { Employee } from '../people/types';
  import { putStep } from './stepWrite';

  type StepData = {
    id: string;
    kind: string;
    title: string;
    status: StepStatus;
    assignee_id: string | null;
    metadata: Record<string, unknown>;
    notes: string | null;
  };

  type Props = {
    step: StepData;
    jobId: string;
    onUpdate: () => void;
  };
  let { step, jobId, onUpdate }: Props = $props();

  function pickStr(key: string, fallback = ''): string {
    const v = step.metadata[key];
    return typeof v === 'string' ? v : fallback;
  }
  function pickBool(key: string): boolean {
    return step.metadata[key] === true;
  }

  let fromId = $state(pickStr('from_id'));
  let toId = $state(pickStr('to_id'));
  let locationFrom = $derived(pickStr('location_from'));
  let locationTo = $derived(pickStr('location_to'));

  let fromConfirmed = $state(pickBool('from_confirmed'));
  let toConfirmed = $state(pickBool('to_confirmed'));
  let notes = $state(step.notes ?? '');
  let saving = $state(false);
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
    fetch('/api/people')
      .then((r) => (r.ok ? r.json() : []))
      .then((roster: Employee[]) => {
        if (!cancelled) employees = roster;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  });

  let empNames = $derived.by(() => {
    const m = new Map<string, string>();
    for (const e of employees) m.set(e.id, e.name ?? "");
    return m;
  });


  let bothConfirmed = $derived(fromConfirmed && toConfirmed);

  async function persist(status?: string): Promise<void> {
    saving = true;
    writeError = null;
    try {
      const body = {
        ...step,
        job_id: jobId,
        notes: notes || undefined,
        status: status ?? step.status,
        metadata: {
          ...step.metadata,
          from_id: fromId,
          to_id: toId,
          from_confirmed: fromConfirmed,
          to_confirmed: toConfirmed,
        },
      };
      const res = await putStep(jobId, step.id, body);
      if (res.kind === 'failed') {
        writeError = res.error;
        // The checkmarks read as recorded facts, not as a form in
        // progress — a rejected write must not leave them rendering
        // confirmations the server never accepted. Fall back to the
        // confirmed state (packet cc9d7fc6, "phantom statuses").
        fromConfirmed = pickBool('from_confirmed');
        toConfirmed = pickBool('to_confirmed');
        return;
      }
      onUpdate();
    } finally {
      saving = false;
    }
  }
</script>

<div class="step-surface step-handoff">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-kind-label">{step.kind}</span>
    <span class="step-status step-status-{step.status}">{step.status}</span>
  </div>

  <div class="step-handoff-pair">
    <div class="step-handoff-side">
      <div class="step-handoff-label">From</div>
      <div class="step-handoff-name">
        {empNames.get(fromId) ?? fromId ?? '—'}
      </div>
      {#if locationFrom}
        <div class="step-handoff-loc">
          <code class="mono">{locationFrom}</code>
        </div>
      {/if}
      <label class="step-handoff-confirm">
        <input
          type="checkbox"
          bind:checked={fromConfirmed}
          disabled={terminal || saving}
        />
        Sender confirmed
      </label>
    </div>

    <div class="step-handoff-arrow">→</div>

    <div class="step-handoff-side">
      <div class="step-handoff-label">To</div>
      <div class="step-handoff-name">
        {empNames.get(toId) ?? toId ?? '—'}
      </div>
      {#if locationTo}
        <div class="step-handoff-loc">
          <code class="mono">{locationTo}</code>
        </div>
      {/if}
      <label class="step-handoff-confirm">
        <input
          type="checkbox"
          bind:checked={toConfirmed}
          disabled={terminal || saving}
        />
        Receiver confirmed
      </label>
    </div>
  </div>

  <div class="step-field">
    <label for={`notes-${step.id}`}>Notes</label>
    <textarea
      id={`notes-${step.id}`}
      rows="2"
      bind:value={notes}
      placeholder="Anything the receiver should know..."
      disabled={terminal}
    ></textarea>
  </div>

  {#if writeError}
    <p class="step-write-error" role="alert">{writeError}</p>
  {/if}

  <div class="step-actions">
    {#if !terminal}
      <button
        class="btn"
        onclick={() => persist(isPending(step.status) ? 'active' : undefined)}
        disabled={saving}
      >
        {saving ? 'Saving…' : 'Save'}
      </button>
      <button
        class="btn btn-primary"
        onclick={() => persist('completed')}
        disabled={saving || !bothConfirmed}
        title={!bothConfirmed
          ? 'Both sides must confirm before completing handoff'
          : ''}
      >
        Complete handoff
      </button>
    {/if}
  </div>
</div>

<style>
  .step-handoff-pair {
    display: flex;
    gap: 12px;
    align-items: stretch;
    margin: 8px 0;
  }
  .step-handoff-side {
    flex: 1;
    padding: 10px 12px;
    background: var(--ink-raised);
    border: 1px solid var(--hairline);
    border-radius: 6px;
  }
  .step-handoff-arrow {
    align-self: center;
    color: var(--static);
    font-size: 24px;
    font-weight: 300;
  }
  .step-handoff-label {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--static);
    margin-bottom: 2px;
  }
  .step-handoff-name {
    font-weight: 500;
    margin-bottom: 4px;
  }
  .step-handoff-loc {
    font-size: 12px;
    margin-bottom: 8px;
  }
  .step-handoff-confirm {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    cursor: pointer;
  }
  .step-handoff-confirm input {
    margin: 0;
  }
</style>
