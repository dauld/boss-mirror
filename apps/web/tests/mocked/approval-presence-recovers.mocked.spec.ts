// Backlog 3ce3c15f (review of car 5b30ccf9, 2026-09-25): a completion the
// jobs API refuses for PRESENCE stopped ApprovalSurface at the raw 422.
// Two shapes, measured on the car's tree:
//
//   - a RELOAD after the signature: the stamp is already on the step, and
//     a presence step whose sign-offs this user's role does not carry
//     never runs the stamp ceremony at all — so the gesture holds no
//     ticket, and the completion goes bare;
//   - an EXPIRED ticket: the ceremony ran for the stamp, but the ticket it
//     issued is past its two-minute life by the time the completion lands.
//
// Either way the surface now answers that 422 with ONE passkey tap on the
// step as shown and retries the completion ONCE with the fresh ticket. The
// mocks below issue a distinct ticket per ceremony and honour only the
// ones they say, so a surface that re-sent the stale ticket, looped, or
// invented a ticket cannot pass.

import { expect, test, type Page, type Route } from '@playwright/test';

const JOB_ID = 'job-apr-1';

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
  begins: unknown[]; finishes: number };

type Setup = Readonly<{
  signOffsRequired: string[];
  signOffs: unknown[];
  /** Which tickets the completion honours, given every ticket issued so far. */
  completionHonours: (ticket: string | undefined, issued: readonly string[]) => boolean;
}>;

async function presenceGatedApproval(page: Page, setup: Setup): Promise<Seen> {
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
    sign_offs_required: setup.signOffsRequired, sign_offs: setup.signOffs,
    assurance_required: 'presence',
    metadata: { plan: 'PLAN wipe target-a' } as Record<string, unknown>, notes: null,
  };
  const job = {
    id: JOB_ID, kind: 'ops-request', title: 'wipe a disk on forge', status: 'open',
    opened_on: '2026-09-25', due_on: null, closed_on: null, owner_id: EMP.id,
    priority: 'standard', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'forge' }, metadata: {},
  };
  const seen: Seen = { stampTickets: [], putTickets: [], begins: [], finishes: 0 };
  const issued: string[] = [];

  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
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
    seen.begins.push(JSON.parse(r.request().postData() ?? '{}'));
    return json(r, {
      challenge_id: `chal-${seen.begins.length}`,
      publicKey: { challenge: 'AAAA', rpId: 'localhost',
        allowCredentials: [{ type: 'public-key', id: 'AAAA' }],
        userVerification: 'required', timeout: 60000 },
    });
  });
  await page.route(/\/api\/auth\/passkey\/assert\/finish$/, (r) => {
    seen.finishes += 1;
    const ticket = `ticket-${seen.finishes}`;
    issued.push(ticket);
    return json(r, { ticket });
  });
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1/sign-offs$`), async (r) => {
    const ticket = (await r.request().headerValue('x-presence-ticket')) ?? undefined;
    seen.stampTickets.push(ticket);
    if (!ticket || !issued.includes(ticket)) return json(r, PRESENCE_REFUSAL, 422);
    step.sign_offs = [{ role: 'platform-admin', authority_id: EMP.id, shape_hash: 'h',
      assurance: 'presence', presence_nonce: 'n' }];
    return json(r, step);
  });
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1$`), async (r) => {
    const ticket = (await r.request().headerValue('x-presence-ticket')) ?? undefined;
    seen.putTickets.push(ticket);
    if (!setup.completionHonours(ticket, issued)) return json(r, PRESENCE_REFUSAL, 422);
    step.status = 'completed';
    return json(r, step);
  });
  return seen;
}

test('after a reload with the stamp already on the step, Approve takes one tap and completes', async ({ page }) => {
  // The signature landed before the reload; this user's role carries no
  // sign-off on the step, so the gesture runs no stamp ceremony and holds
  // no ticket. The completion honours only a ticket a ceremony issued.
  const seen = await presenceGatedApproval(page, {
    signOffsRequired: [],
    signOffs: [{ role: 'controller', authority_id: 'emp-002', shape_hash: 'h',
      assurance: 'presence', presence_nonce: 'n0' }],
    completionHonours: (t, issued) => t !== undefined && issued.includes(t),
  });

  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  await surface.getByRole('button', { name: 'Approve' }).click();

  await expect(surface.locator('.step-status')).toHaveText('completed');
  await expect(surface.locator('.step-write-error')).toHaveCount(0);
  expect(seen.stampTickets).toEqual([]);
  expect(seen.finishes).toBe(1);
  expect(seen.putTickets).toEqual([undefined, 'ticket-1']);
  // The tap signs the step as this surface showed it, decision folded in.
  const shown = (seen.begins[0] as { shown: { title: string; metadata: Record<string, unknown> } })
    .shown;
  expect(shown.title).toBe('Approve the plan: wipe on forge');
  expect(shown.metadata.plan).toBe('PLAN wipe target-a');
  expect(shown.metadata.decision).toBe('approved');
});

test('a stamp ticket expired before the completion: one fresh tap, and the retry carries it', async ({ page }) => {
  // The stamp's own ceremony issues ticket-1; by the completion it is
  // past its life, so the completion honours only a LATER ticket.
  const seen = await presenceGatedApproval(page, {
    signOffsRequired: ['platform-admin'],
    signOffs: [],
    completionHonours: (t) => t === 'ticket-2',
  });

  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  await surface.getByRole('button', { name: 'Approve' }).click();

  await expect(surface.locator('.step-status')).toHaveText('completed');
  await expect(surface.locator('.step-write-error')).toHaveCount(0);
  expect(seen.stampTickets).toEqual([undefined, 'ticket-1']);
  expect(seen.finishes).toBe(2);
  expect(seen.putTickets).toEqual(['ticket-1', 'ticket-2']);
});

test('refused again after the fresh tap: the surface says so and never asks a third time', async ({ page }) => {
  const seen = await presenceGatedApproval(page, {
    signOffsRequired: [],
    signOffs: [],
    completionHonours: () => false,
  });

  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  await surface.getByRole('button', { name: 'Approve' }).click();

  await expect(surface.locator('.step-write-error')).toContainText(
    'refused again after a fresh passkey tap',
  );
  expect(seen.finishes).toBe(1);
  expect(seen.putTickets).toEqual([undefined, 'ticket-1']);
});
