<script lang="ts">
  // Intake step surface — first step of every wholesale-keg-order
  // Job (and any other Workflow that opens with `intake`).
  //
  // Demo framing: line items arrive pre-populated from the
  // Workflow's metadata_defaults (the brewery's wholesale-keg-order
  // ships with default SKUs + qtys per the seed). The user
  // confirms the order, optionally tweaks the delivery window,
  // and marks the step done. We don't want to be a full
  // order-composer here — that's what the SPA's Sales surface is
  // for. Intake is the "yes, this is what we're brewing" gate.

  import { isPending, isTerminal as _isTerminal, type StepStatus } from '../jobs/types';
  import type { Employee } from '../people/types';
  import { putStep } from './stepWrite';
  import { formatMoney } from '@boss/web-kit/ui/money';

  type LineItem = {
    sku?: string;
    qty?: number;
    description?: string;
    amount_cents?: number;
    revenue_category?: string;
  };

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

  let lineItems = $derived<LineItem[]>(
    Array.isArray(step.metadata.line_items)
      ? (step.metadata.line_items as LineItem[])
      : [],
  );
  let accountId = $derived(
    typeof step.metadata.account_id === 'string'
      ? step.metadata.account_id
      : null,
  );

  let deliveryWindow = $state(
    typeof step.metadata.delivery_window === 'string'
      ? step.metadata.delivery_window
      : '',
  );
  let notes = $state(step.notes ?? '');
  let assigneeId = $state(step.assignee_id ?? '');
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


  let totalCents = $derived(
    lineItems.reduce((sum, li) => sum + (li.amount_cents ?? 0), 0),
  );

  async function persist(status?: string): Promise<void> {
    saving = true;
    writeError = null;
    try {
      const body = {
        ...step,
        job_id: jobId,
        notes: notes || undefined,
        status: status ?? step.status,
        assignee_id: assigneeId || null,
        metadata: {
          ...step.metadata,
          delivery_window: deliveryWindow,
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

  function fmtMoney(cents: number | undefined): string {
    if (cents === undefined || cents === 0) return '';
    return formatMoney({ amount_cents: cents, currency: 'USD' }, { precision: 'cents' });
  }
</script>

<div class="step-surface step-intake">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-kind-label">{step.kind}</span>
    <span class="step-status step-status-{step.status}">{step.status}</span>
  </div>

  {#if accountId}
    <div class="step-meta-row">
      <strong>Account:</strong>
      <code class="mono">{accountId}</code>
    </div>
  {/if}

  {#if lineItems.length > 0}
    <div class="step-field">
      <label>Order</label>
      <table class="step-line-items">
        <tbody>
          {#each lineItems as li, idx (idx)}
            <tr>
              <td class="qty">{li.qty ?? '—'} ×</td>
              <td class="desc">{li.description ?? li.sku ?? ''}</td>
              <td class="amount">{fmtMoney(li.amount_cents)}</td>
            </tr>
          {/each}
          {#if totalCents > 0}
            <tr class="total">
              <td colspan="2">Total</td>
              <td class="amount">{fmtMoney(totalCents)}</td>
            </tr>
          {/if}
        </tbody>
      </table>
    </div>
  {/if}

  <div class="step-field step-assign-row">
    <label for={`delivery-${step.id}`}>Delivery</label>
    <input
      id={`delivery-${step.id}`}
      type="text"
      placeholder="e.g. 2026-05-01..2026-05-03 or Mon AM"
      bind:value={deliveryWindow}
      disabled={terminal || saving}
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
      placeholder="Customer requests, exceptions..."
      disabled={terminal}
    ></textarea>
  </div>

  {#if writeError}
    <p class="step-write-error" role="alert">{writeError}</p>
  {/if}

  <div class="step-actions">
    {#if !terminal && isPending(step.status)}
      <button
        class="step-btn step-btn-primary"
        onclick={() => persist('active')}
        disabled={saving}
      >
        Start
      </button>
    {/if}
    {#if !terminal && step.status === 'active'}
      <button
        class="step-btn step-btn-primary"
        onclick={() => persist('completed')}
        disabled={saving}
      >
        {saving ? 'Saving…' : 'Confirm order'}
      </button>
    {/if}
  </div>
</div>

<style>
  .step-line-items {
    width: 100%;
    border-collapse: collapse;
    margin-top: 4px;
    font-size: 13px;
  }
  .step-line-items td {
    padding: 4px 6px;
    border-bottom: 1px solid var(--border-soft, #f3f4f6);
  }
  .step-line-items .qty {
    width: 60px;
    color: var(--text-muted, #6b7280);
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .step-line-items .desc {
    color: var(--text, #111827);
  }
  .step-line-items .amount {
    width: 100px;
    text-align: right;
    font-variant-numeric: tabular-nums;
    color: var(--text-muted, #6b7280);
  }
  .step-line-items tr.total {
    font-weight: 600;
  }
  .step-line-items tr.total td {
    border-top: 1px solid var(--border, #d1d5db);
    border-bottom: none;
    color: var(--text, #111827);
  }
  .step-line-items tr.total .amount {
    color: var(--text, #111827);
  }
</style>
