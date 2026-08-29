<script lang="ts">
  import { isPending, isTerminal as _isTerminal, type StepStatus } from '../jobs/types';
  import { putStep } from './stepWrite';
  import { formatMoney } from '@boss/web-kit/ui/money';
  // Procurement step surface — place a purchase order with a
  // vendor. The ingredient-restock Workflow opens with this step
  // (parts-buyer fires off an order to the malt-supplier or
  // hops-supplier counterparty).
  //
  // Demo framing: vendor + line items typically arrive populated
  // from the Workflow's metadata_defaults. The buyer confirms +
  // sets expected delivery, on done emits inventory.po.place
  // which lands a PO row + the malt-supplier counterparty fires
  // its 5-business-day vendor-invoice chain.

  type LineItem = {
    part_sku?: string;
    qty?: number;
    description?: string;
    unit_cost_cents?: number;
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

  function pickStr(key: string, fallback = ''): string {
    const v = step.metadata[key];
    return typeof v === 'string' ? v : fallback;
  }

  let lineItems = $derived<LineItem[]>(
    Array.isArray(step.metadata.line_items)
      ? (step.metadata.line_items as LineItem[])
      : Array.isArray(step.metadata.expected_items)
        ? (step.metadata.expected_items as LineItem[])
        : [],
  );

  let vendorId = $derived(pickStr('vendor_id'));
  let poId = $derived(pickStr('po_id'));

  let expectedDate = $state(pickStr('expected_date'));
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




  let totalCents = $derived(
    lineItems.reduce(
      (sum, li) => sum + (li.unit_cost_cents ?? 0) * (li.qty ?? 0),
      0,
    ),
  );

  function fmtMoney(cents: number): string {
    if (cents === 0) return '';
    return formatMoney({ amount_cents: cents, currency: 'USD' }, { precision: 'cents' });
  }

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
          expected_date: expectedDate || undefined,
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
</script>

<div class="step-surface step-procurement">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-kind-label">{step.kind}</span>
    <span class="step-status step-status-{step.status}">{step.status}</span>
  </div>

  <div class="step-meta-row">
    {#if vendorId}
      <strong>Vendor:</strong> <code class="mono">{vendorId}</code>
    {/if}
    {#if poId}
      <span class="muted"> · </span>
      <strong>PO:</strong> <code class="mono">{poId}</code>
    {/if}
  </div>

  {#if lineItems.length > 0}
    <div class="step-field">
      <label>Order</label>
      <table class="step-line-items">
        <thead>
          <tr>
            <th class="col-sku">SKU</th>
            <th class="col-desc">Description</th>
            <th class="col-num">Qty</th>
            <th class="col-num">Unit</th>
            <th class="col-num">Total</th>
          </tr>
        </thead>
        <tbody>
          {#each lineItems as li, idx (idx)}
            <tr>
              <td><code class="mono">{li.part_sku ?? ''}</code></td>
              <td class="desc">{li.description ?? ''}</td>
              <td class="num">{li.qty ?? '—'}</td>
              <td class="num">{fmtMoney(li.unit_cost_cents ?? 0)}</td>
              <td class="num">
                {fmtMoney((li.unit_cost_cents ?? 0) * (li.qty ?? 0))}
              </td>
            </tr>
          {/each}
          {#if totalCents > 0}
            <tr class="total">
              <td colspan="4">Total</td>
              <td class="num">{fmtMoney(totalCents)}</td>
            </tr>
          {/if}
        </tbody>
      </table>
    </div>
  {/if}

  <div class="step-field step-assign-row">
    <label for={`exp-${step.id}`}>Expected delivery</label>
    <input
      id={`exp-${step.id}`}
      type="date"
      bind:value={expectedDate}
      disabled={terminal || saving}
    />
  </div>

  <div class="step-field">
    <label for={`notes-${step.id}`}>Notes</label>
    <textarea
      id={`notes-${step.id}`}
      rows="2"
      bind:value={notes}
      placeholder="Vendor instructions, payment terms..."
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
        disabled={saving || lineItems.length === 0}
      >
        {saving ? 'Saving…' : 'Place order'}
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
  .step-line-items th {
    text-align: left;
    font-weight: 600;
    padding: 4px 6px;
    border-bottom: 1px solid var(--border, #e5e7eb);
    color: var(--text-muted, #6b7280);
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }
  .step-line-items th.col-num {
    text-align: right;
  }
  .step-line-items td {
    padding: 4px 6px;
    border-bottom: 1px solid var(--border-soft, #f3f4f6);
  }
  .step-line-items .col-sku { width: 160px; }
  .step-line-items .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
    width: 90px;
  }
  .step-line-items .desc {
    color: var(--text-muted, #6b7280);
  }
  .step-line-items tr.total {
    font-weight: 600;
  }
  .step-line-items tr.total td {
    border-top: 1px solid var(--border, #d1d5db);
    border-bottom: none;
  }
  .muted {
    color: var(--text-muted, #6b7280);
  }
</style>
