// Backlog b568044a (round-4 review of car 7abc0154, 2026-09-25): no
// screen could complete a passkey-gated step. ApprovalSurface ran the
// presence ceremony for the STAMP, then completed with a PUT that
// carried no ticket — and the jobs API judges assurance on every
// request that leaves the open states, from that request's own header
// (steps.rs is_leaving_open -> judge_assurance). So the stamp landed,
// the completion answered 422, and the step stayed ready after the
// passkey tap. The server half is pinned in boss-jobs
// (the_sign_offs_own_ticket_completes_the_step_and_the_stamp_alone_does_not).
//
// The mocks below refuse a ticketless stamp AND a ticketless completion
// exactly as the server does, so the surface is green only if the
// completion carries the ticket its own ceremony was issued — once, for
// Approve and for Reject alike, since both complete the step.

import { expect, test, type Page, type Route } from '@playwright/test';
import { servePeopleRows } from './_smokeMocks';

const JOB_ID = 'job-apc-1';
const TICKET = 'ticket-from-this-ceremony';

const EMP = { id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const PRESENCE_REFUSAL = {
  error: 'step requires stronger assurance than this request carries',
  required: 'presence',
  produced: 'session',
};

type Seen = { stampTickets: (string | undefined)[]; putTickets: (string | undefined)[];
  begins: number; finishes: number };

async function presenceGatedApproval(page: Page): Promise<Seen> {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
    // The authenticator: a passkey that signs whatever it is asked to.
    const buf = () => new Uint8Array([1, 2, 3]).buffer;
    Object.defineProperty(navigator, 'credentials', {
      configurable: true,
      value: {
        get: async () => ({
          id: 'cred-1',
          rawId: buf(),
          type: 'public-key',
          response: { authenticatorData: buf(), clientDataJSON: buf(), signature: buf(),
            userHandle: null },
        }),
      },
    });
  });
  const step = {
    id: 's1', job_id: JOB_ID, title: 'Approve the plan: wipe on forge', kind: 'sign-off',
    status: 'ready', assignee_id: null, sort_order: 0, blocked_by: [],
    sign_offs_required: ['platform-admin'], sign_offs: [] as unknown[],
    assurance_required: 'presence',
    metadata: { plan: 'PLAN wipe target-a' } as Record<string, unknown>, notes: null,
  };
  const job = {
    id: JOB_ID, kind: 'ops-request', title: 'wipe a disk on forge', status: 'open',
    opened_on: '2026-09-25', due_on: null, closed_on: null, owner_id: EMP.id,
    priority: 'standard', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'forge' }, metadata: {},
  };
  const seen: Seen = { stampTickets: [], putTickets: [], begins: 0, finishes: 0 };

  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await servePeopleRows(page, [EMP]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'sign-off', label: 'Sign-off', category: 'approval', ux: 'inline',
      description: '', surface: 'approval' },
  ]));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, { ...job, steps: [step] }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1/metadata$`), (r) => {
    Object.assign(step.metadata, JSON.parse(r.request().postData() ?? '{}'));
    return json(r, step);
  });
  await page.route(/\/api\/auth\/passkey\/assert\/begin$/, (r) => {
    seen.begins += 1;
    return json(r, {
      challenge_id: 'chal-1',
      publicKey: { challenge: 'AAAA', rpId: 'localhost',
        allowCredentials: [{ type: 'public-key', id: 'AAAA' }],
        userVerification: 'required', timeout: 60000 },
    });
  });
  await page.route(/\/api\/auth\/passkey\/assert\/finish$/, (r) => {
    seen.finishes += 1;
    return json(r, { ticket: TICKET });
  });
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1/sign-offs$`), async (r) => {
    const ticket = await r.request().headerValue('x-presence-ticket');
    seen.stampTickets.push(ticket ?? undefined);
    if (ticket !== TICKET) return json(r, PRESENCE_REFUSAL, 422);
    step.sign_offs = [{ role: 'platform-admin', authority_id: EMP.id, shape_hash: 'h',
      assurance: 'presence', presence_nonce: 'n' }];
    return json(r, step);
  });
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1$`), async (r) => {
    const ticket = await r.request().headerValue('x-presence-ticket');
    seen.putTickets.push(ticket ?? undefined);
    if (ticket !== TICKET) return json(r, PRESENCE_REFUSAL, 422);
    step.status = 'completed';
    return json(r, step);
  });
  return seen;
}

for (const [button, decision] of [['Approve', 'approved'], ['Reject', 'rejected']] as const) {
  test(`${button} on a presence-gated step completes it with the ticket its own ceremony was issued`, async ({ page }) => {
    const seen = await presenceGatedApproval(page);

    await page.goto(`/ux/jobs/${JOB_ID}`);
    const surface = page.locator('.sg-detail');
    await surface.getByRole('button', { name: button }).click();

    await expect(surface.locator('.step-status')).toHaveText('completed');
    await expect(surface.locator('.step-write-error')).toHaveCount(0);
    await expect(surface.locator('.step-approval-result')).toContainText(decision);
    // One passkey tap: the ticket the stamp was granted on is the one
    // the completion carries — never a second ceremony, never none.
    expect(seen.begins).toBe(1);
    expect(seen.finishes).toBe(1);
    expect(seen.stampTickets).toEqual([undefined, TICKET]);
    expect(seen.putTickets).toEqual([TICKET]);
  });
}
