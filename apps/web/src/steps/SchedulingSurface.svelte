<script lang="ts">
  // Scheduling step surface — slot a Job into a calendar window.
  // The morning-brew Workflow opens with a scheduling step
  // ("plan today's brew"), wholesale-keg-order's tier 2 schedules
  // the production batch, equipment-preventive-maintenance schedules the technician
  // visit.
  //
  // Demo framing: pick a date/time + duration + assignee, save.
  // The full calendar conflict view is overkill for the surface;
  // the Calendar KB page in the sidebar handles that.

  import { isPending, isTerminal as _isTerminal, type StepStatus } from '../jobs/types';
  import type { Employee } from '../people/types';
  import { saveStep } from './stepWrite';

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
  function pickNum(key: string): number | '' {
    const v = step.metadata[key];
    return typeof v === 'number' ? v : '';
  }

  let location = $state(pickStr('location'));
  let scheduledAt = $state(pickStr('scheduled_at'));
  let durationMinutes = $state<number | ''>(pickNum('duration_minutes'));
  let assigneeId = $state(step.assignee_id ?? '');
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



  // <input type="datetime-local"> wants `YYYY-MM-DDTHH:mm`. The
  // metadata stores ISO-8601 with possible seconds + timezone, so
  // trim down for the input + restore on save.
  let scheduledAtForInput = $derived(scheduledAt.slice(0, 16));

  // What Schedule still needs (backlog 2f14cdd8, decided 2026-09-24).
  // The jobs↔calendar hook reserves only when the step goes active
  // with all three — a when, a positive duration, an assignee — and
  // answers NoOp otherwise, so a button that enabled on the date alone
  // made a step active with no reservation and nothing on screen to
  // say so. The button waits for all three and the line beside it
  // names what is missing; required-at-done validation is unchanged.
  let scheduleMissing = $derived(
    [
      scheduledAt ? null : 'date/time',
      typeof durationMinutes === 'number' && durationMinutes > 0 ? null : 'duration',
      assigneeId ? null : 'assignee',
    ].filter((m): m is string => m !== null),
  );

  async function persist(status?: string): Promise<void> {
    saving = true;
    writeError = null;
    try {
      // The keys this surface owns, through the merge door; an emptied
      // field is sent as null and deleted, where it used to be cleared
      // by omission from a wholesale PUT (backlog e39a9d2a).
      const body = {
        notes: notes || undefined,
        status: status ?? step.status,
        assignee_id: assigneeId || null,
        metadata: {
          location: location || undefined,
          scheduled_at: scheduledAt || undefined,
          duration_minutes:
            typeof durationMinutes === 'number' ? durationMinutes : undefined,
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
</script>

<div class="step-surface step-scheduling">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-kind-label">{step.kind}</span>
    <span class="step-status step-status-{step.status}">{step.status}</span>
  </div>

  <div class="step-field-row">
    <div class="step-field step-assign-row">
      <label for={`when-${step.id}`}>When</label>
      <input
        id={`when-${step.id}`}
        type="datetime-local"
        value={scheduledAtForInput}
        disabled={terminal || saving}
        oninput={(e) => (scheduledAt = (e.target as HTMLInputElement).value)}
      />
    </div>
    <div class="step-field step-assign-row step-duration">
      <label for={`dur-${step.id}`}>Duration (min)</label>
      <input
        id={`dur-${step.id}`}
        type="number"
        min="0"
        step="15"
        value={durationMinutes}
        disabled={terminal || saving}
        oninput={(e) => {
          const n = parseInt((e.target as HTMLInputElement).value);
          durationMinutes = Number.isFinite(n) ? n : '';
        }}
      />
    </div>
  </div>

  <div class="step-field step-assign-row">
    <label for={`loc-${step.id}`}>Location</label>
    <input
      id={`loc-${step.id}`}
      type="text"
      bind:value={location}
      disabled={terminal || saving}
      placeholder="e.g. loc-brewery-brewhouse"
    />
  </div>

  <div class="step-field step-assign-row">
    <label for={`assignee-${step.id}`}>Assignee</label>
    <select
      id={`assignee-${step.id}`}
      bind:value={assigneeId}
      disabled={terminal || saving}
    >
      <option value="">— unassigned —</option>
      {#each employees as e (e.id)}
        <option value={e.id}>{e.name} · {e.role}</option>
      {/each}
    </select>
  </div>

  <div class="step-field">
    <label for={`notes-${step.id}`}>Notes</label>
    <textarea
      id={`notes-${step.id}`}
      rows="2"
      bind:value={notes}
      placeholder="Window constraints, dependencies..."
      disabled={terminal}
    ></textarea>
  </div>

  {#if writeError}
    <p class="step-write-error" role="alert">{writeError}</p>
  {/if}

  <div class="step-actions">
    {#if !terminal && isPending(step.status)}
      <button
        class="btn btn-primary"
        onclick={() => persist('active')}
        disabled={saving || scheduleMissing.length > 0}
      >
        Schedule
      </button>
      {#if scheduleMissing.length > 0}
        <span class="step-schedule-missing">Missing: {scheduleMissing.join(', ')}</span>
      {/if}
    {/if}
    {#if !terminal && step.status === 'active'}
      <button
        class="btn btn-primary"
        onclick={() => persist('completed')}
        disabled={saving || !scheduledAt}
      >
        {saving ? 'Saving…' : 'Confirm'}
      </button>
    {/if}
  </div>
</div>

<style>
  .step-field-row {
    display: flex;
    gap: 12px;
    flex-wrap: wrap;
  }
  .step-field-row .step-field {
    flex: 1;
    min-width: 130px;
  }
  .step-duration {
    flex: 0 0 140px;
  }
  .step-schedule-missing {
    align-self: center;
    font-size: 0.85em;
    color: var(--static);
  }
</style>
