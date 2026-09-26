// The billing step's one action: post the invoice, then complete the
// step with the invoice's id on it. Two services, two writes, no
// transaction spanning them — so the flow is built to be re-run.
//
// WHY IT MUST BE RE-RUNNABLE (backlog 558396ff, from the review of car
// 88123ae0, 2026-09-25). Since that car, a step write that races another
// write to the same step is refused 409 with nothing written, and the
// answer to it is "send it again" — here, a second click. The invoice
// had already been posted, under an id minted from the clock
// (`INV-<8 hex>-<Date.now()>`), so every second click was a second
// invoice for one sale: a second receivable and a second revenue fact.
//
// So the id is the STEP's: every attempt names the same invoice. And an
// invoice that already exists is not posted again — commerce upserts the
// row, but records `commerce.invoice.created` on every create it
// accepts, and the dispatcher's consume rule acts on each one.

import { saveStep } from './stepWrite';

export type BillingInput = Readonly<{
  jobId: string;
  stepId: string;
  accountId: string;
  amountCents: number;
  currency: string;
  revenueCategory: string;
  description: string;
  issuedOn: string;
  dueOn: string;
}>;

export type BillingResult =
  | { kind: 'ok'; invoiceId: string }
  | { kind: 'failed'; error: string };

/// One step bills once, so the step's id names its invoice. The `INV-`
/// prefix is one of the shapes the surface renders as an invoice link.
export function invoiceIdForStep(stepId: string): string {
  return `INV-${stepId}`;
}

async function describe(resp: Response): Promise<string> {
  return `${resp.status} ${await resp.text().catch(() => '')}`.trim();
}

/// Whether this step's invoice is already on the books: `true` / `false`,
/// or the failure that kept the question from being answered — never a
/// guess, because guessing "no" is the duplicate this exists to stop.
async function invoiceExists(invoiceId: string): Promise<boolean | { error: string }> {
  try {
    const resp = await fetch(`/api/commerce/invoices/${encodeURIComponent(invoiceId)}`);
    if (resp.ok) return true;
    if (resp.status === 404) return false;
    return { error: `invoice lookup failed: ${await describe(resp)}` };
  } catch (e) {
    return { error: `invoice lookup failed: ${e instanceof Error ? e.message : String(e)}` };
  }
}

export async function postInvoiceAndComplete(input: BillingInput): Promise<BillingResult> {
  const invoiceId = invoiceIdForStep(input.stepId);

  const exists = await invoiceExists(invoiceId);
  if (typeof exists === 'object') return { kind: 'failed', error: exists.error };
  if (!exists) {
    const invoicePayload = {
      id: invoiceId,
      account_id: input.accountId,
      issued_on: input.issuedOn,
      due_on: input.dueOn,
      paid_on: null,
      status: 'outstanding',
      amount_cents: input.amountCents,
      currency: input.currency,
      line_items: [
        {
          id: `${invoiceId}-L1`,
          invoice_id: invoiceId,
          revenue_category: input.revenueCategory,
          amount_cents: input.amountCents,
          currency: input.currency,
          description: input.description,
          ref_id: input.jobId,
        },
      ],
    };
    let invoiceResp: Response;
    try {
      invoiceResp = await fetch('/api/commerce/invoices/create', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(invoicePayload),
      });
    } catch (e) {
      return {
        kind: 'failed',
        error: `invoice create failed: ${e instanceof Error ? e.message : String(e)}`,
      };
    }
    if (!invoiceResp.ok) {
      return { kind: 'failed', error: `invoice create failed: ${await describe(invoiceResp)}` };
    }
  }

  // The invoice id through the step merge door, then the status alone:
  // a PUT carrying the surface's copy of the metadata is refused when
  // anything was written to the step since it was read (e39a9d2a), which
  // is exactly the state a retried click is in.
  const written = await saveStep(input.jobId, input.stepId, {
    status: 'completed',
    metadata: { invoice_id: invoiceId },
  });
  if (written.kind === 'failed') {
    return { kind: 'failed', error: `step update failed: ${written.error}` };
  }
  return { kind: 'ok', invoiceId };
}
