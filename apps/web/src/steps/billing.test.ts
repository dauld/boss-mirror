// The billing step posts an invoice, then completes the step (backlog
// 558396ff, from the review of car 88123ae0). Since that car, a step
// write that races another write to the same step is refused 409 with
// nothing written, and the operator clicks again. The invoice had
// already been posted — under an id minted from the clock — so every
// second click was a second invoice for one sale: a second receivable,
// a second revenue fact.

import { afterEach, describe, expect, test } from 'bun:test';
import { invoiceIdForStep, postInvoiceAndComplete, type BillingInput } from './billing';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

const JOB = '11111111-2222-4333-8444-555555555555';
const STEP = 'aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee';

const input: BillingInput = {
  jobId: JOB,
  stepId: STEP,
  accountId: 'acct-7',
  amountCents: 125_00,
  currency: 'USD',
  revenueCategory: 'new-sales',
  description: 'Device sale',
  issuedOn: '2026-09-25',
  dueOn: '2026-10-25',
};

/// Commerce keeps invoices by id; the jobs API refuses the FIRST step
/// completion as stale (the race 88123ae0 made an ordinary answer).
function fakeServer() {
  const invoices = new Map<string, unknown>();
  const creates: string[] = [];
  let stepWrites = 0;
  const fetchFn = async (url: string, init?: RequestInit): Promise<Response> => {
    const method = (init?.method ?? 'GET').toUpperCase();
    if (url === '/api/commerce/invoices/create' && method === 'POST') {
      const body = JSON.parse(String(init?.body)) as { id: string };
      creates.push(body.id);
      invoices.set(body.id, body);
      return new Response(JSON.stringify({ ok: true, id: body.id }), { status: 201 });
    }
    const got = url.match(/^\/api\/commerce\/invoices\/([^/]+)$/);
    if (got && method === 'GET') {
      const inv = invoices.get(decodeURIComponent(got[1] ?? ''));
      return inv
        ? new Response(JSON.stringify(inv), { status: 200 })
        : new Response('no invoice', { status: 404 });
    }
    if (url.startsWith(`/api/jobs/${JOB}/steps/${STEP}`)) {
      if (method === 'PATCH') return new Response(null, { status: 204 });
      stepWrites += 1;
      return stepWrites === 1
        ? new Response(
            JSON.stringify({ error: 'step changed while this write was computed' }),
            { status: 409 },
          )
        : new Response(null, { status: 204 });
    }
    return new Response('unexpected', { status: 500 });
  };
  globalThis.fetch = fetchFn as unknown as typeof fetch;
  return { invoices, creates };
}

describe('postInvoiceAndComplete', () => {
  test('a second click after a refused completion posts no second invoice', async () => {
    const server = fakeServer();

    const first = await postInvoiceAndComplete(input);
    expect(first.kind).toBe('failed');
    // A person's second click is not in the same millisecond.
    await new Promise((r) => setTimeout(r, 5));
    const second = await postInvoiceAndComplete(input);

    expect(second.kind).toBe('ok');
    expect(server.invoices.size).toBe(1);
    // Not even re-sent: commerce records `commerce.invoice.created` on
    // every create it accepts, so a re-post of the same id is a second
    // event for the dispatcher's consume rule to act on.
    expect(server.creates.length).toBe(1);
  });

  test('the invoice id is the step’s, so every attempt names the same invoice', () => {
    expect(invoiceIdForStep(STEP)).toBe(invoiceIdForStep(STEP));
    expect(invoiceIdForStep(STEP)).not.toBe(invoiceIdForStep('ffffffff-bbbb-4ccc-8ddd-eeeeeeeeeeee'));
    // The surface renders an id as an invoice link only in these shapes.
    expect(invoiceIdForStep(STEP).startsWith('INV-')).toBe(true);
  });

  test('a refused invoice stops before the step is touched', async () => {
    let stepTouched = false;
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      if (url.startsWith('/api/commerce/invoices/') && (init?.method ?? 'GET') === 'GET') {
        return new Response('no invoice', { status: 404 });
      }
      if (url === '/api/commerce/invoices/create') {
        return new Response('account on hold', { status: 400 });
      }
      stepTouched = true;
      return new Response(null, { status: 204 });
    }) as unknown as typeof fetch;

    const res = await postInvoiceAndComplete(input);
    expect(res.kind).toBe('failed');
    expect(stepTouched).toBe(false);
  });
});
