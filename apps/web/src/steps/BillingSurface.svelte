<script lang="ts">
  // Billing step — orchestrates invoice creation AND step completion
  // from the surface (no cross-domain call inside boss-jobs).
  //
  // Flow on click (`billing.ts`):
  //   1. POST /api/commerce/invoices/create — commerce emits the
  //      finance.invoice.issued fact; ledger projects a balanced
  //      journal entry in the same tx. The invoice id is the step's,
  //      and an invoice already posted is not posted again, so a
  //      second click after a refused step write bills once (558396ff).
  //   2. The step completes with `invoice_id` in metadata so the audit
  //      chain is click-through-able.
  // If #1 fails the step stays put and we render the error.

  import { isPending, isTerminal as _isTerminal, type StepStatus } from '../jobs/types';
  import { postInvoiceAndComplete as postAndComplete } from './billing';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import { formatMoney } from '@boss/web-kit/ui/money';
  import { appNow, appToday } from '@boss/web-kit/sim-clock';

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

  let saving = $state(false);
  let terminal = $derived(_isTerminal(step.status));

  let errorMsg = $state<string | null>(null);

  let accountId = $derived(
    typeof step.metadata.account_id === 'string'
      ? (step.metadata.account_id as string)
      : undefined,
  );
  let amountCents = $derived(Number(step.metadata.amount_cents ?? NaN));
  let currency = $derived(
    typeof step.metadata.currency === 'string'
      ? (step.metadata.currency as string)
      : 'USD',
  );
  let revenueCategory = $derived(
    typeof step.metadata.revenue_category === 'string'
      ? (step.metadata.revenue_category as string)
      : 'new-sales',
  );
  let description = $derived(
    typeof step.metadata.description === 'string'
      ? (step.metadata.description as string)
      : 'Device sale',
  );
  // Only treat a metadata.invoice_id as "real" if it looks
  // shaped like an actual invoice id (UUID, or the `inv-step-…`
  // / `INV-…` prefix the SPA + sim emit). The simulator's faker
  // can put short English words ("scheduled", "queued", …) into
  // plain `string` fields; rendering those as
  // `/finance/<word>` links produces a console 404 and a broken
  // "not found" view. Defensively reject anything that doesn't
  // look like an id shape.
  function looksLikeInvoiceId(v: string): boolean {
    if (/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(v)) return true;
    if (/^(inv|INV)[-_]/.test(v)) return true;
    return false;
  }
  let existingInvoiceId = $derived(
    typeof step.metadata.invoice_id === 'string'
      && looksLikeInvoiceId(step.metadata.invoice_id as string)
      ? (step.metadata.invoice_id as string)
      : undefined,
  );

  let canPost = $derived(
    !saving &&
      !terminal &&
      typeof accountId === 'string' &&
      accountId.length > 0 &&
      Number.isFinite(amountCents) &&
      amountCents > 0,
  );

  async function postInvoiceAndComplete(): Promise<void> {
    if (!canPost || !accountId) return;
    saving = true;
    errorMsg = null;
    try {
      const dueOn = new Date(appNow().getTime() + 30 * 24 * 60 * 60 * 1000)
        .toISOString()
        .slice(0, 10);
      const result = await postAndComplete({
        jobId,
        stepId: step.id,
        accountId,
        amountCents,
        currency,
        revenueCategory,
        description,
        issuedOn: appToday(),
        dueOn,
      });
      if (result.kind === 'failed') {
        errorMsg = result.error;
        return;
      }
      onUpdate();
    } catch (e) {
      errorMsg = e instanceof Error ? e.message : String(e);
    } finally {
      saving = false;
    }
  }
</script>

<div class="step-surface step-billing" data-step-kind="billing">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-kind-label">billing</span>
    <span class="step-status step-status-{step.status}">{step.status}</span>
  </div>

  <div class="step-metadata-display">
    <div class="step-meta-row">
      <strong>Account:</strong>
      {#if accountId}
        <EntityLink kind="account" id={accountId} />
      {:else}
        <span class="empty">not set</span>
      {/if}
    </div>
    <div class="step-meta-row">
      <strong>Amount:</strong>
      {Number.isFinite(amountCents)
        ? formatMoney({ amount_cents: amountCents, currency })
        : '—'}
    </div>
    <div class="step-meta-row">
      <strong>Category:</strong>
      {revenueCategory}
    </div>
  </div>

  {#if existingInvoiceId}
    <div class="step-meta-row">
      Invoice posted:
      <EntityLink kind="invoice" id={existingInvoiceId} />
    </div>
  {/if}

  {#if errorMsg}
    <div class="error small">{errorMsg}</div>
  {/if}

  {#if !terminal}
    <div class="step-actions">
      <button
        class="btn btn-primary"
        onclick={postInvoiceAndComplete}
        disabled={!canPost}
        data-testid="billing-post-invoice"
      >
        {saving ? 'Posting…' : 'Post invoice & complete'}
      </button>
      {#if !canPost && !saving}
        <span class="step-help small">
          {accountId && amountCents > 0
            ? ''
            : 'Step metadata is missing account_id or amount_cents.'}
        </span>
      {/if}
    </div>
  {/if}
</div>
