// Backlog d82b5f60 (review of car 66de0e4b, 2026-09-25): ApprovalSurface's
// decide() read `step.id`, `step.title` and `step.metadata` AFTER its
// awaits. The surface instance is reused when the rail switches steps, so
// a click on another step while the first write was in flight re-aimed
// every later write of the gesture — the stamp, the ceremony's `shown`,
// and the completion PUT — at the step now on screen. A presence step
// would refuse that PUT 412/422; an ordinary one was COMPLETED by a
// request its approver never aimed at it.
//
// The mocks below hold the gesture's first write (the decision's metadata
// merge on s1) until the test has switched the surface to s2, then let it
// go. Every write the gesture makes afterwards must still name s1, and s2
// must receive none.

import { expect, test, type Page, type Route } from '@playwright/test';

const JOB_ID = 'job-apsw-1';

const EMP = { id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

type Write = { method: string; step: string; path: string; ticket?: string };
type Seen = { writes: Write[]; begins: unknown[]; release: () => void };

async function twoApprovals(page: Page, presence: boolean): Promise<Seen> {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
    const buf = () => new Uint8Array([1, 2, 3]).buffer;
    Object.defineProperty(navigator, 'credentials', {
      configurable: true,
      value: {
        get: async () => ({
          id: 'cred-1', rawId: buf(), type: 'public-key',
          response: { authenticatorData: buf(), clientDataJSON: buf(), signature: buf(),
            userHandle: null },
        }),
      },
    });
  });
  const mk = (id: string, title: string, order: number, plan: string) => ({
    id, job_id: JOB_ID, title, kind: 'sign-off', status: 'ready', assignee_id: null,
    sort_order: order, blocked_by: [],
    sign_offs_required: presence ? ['platform-admin'] : [], sign_offs: [] as unknown[],
    ...(presence ? { assurance_required: 'presence' } : {}),
    metadata: { plan } as Record<string, unknown>, notes: null,
  });
  const steps = [
    mk('s1', 'Approve the budget', 0, 'PLAN budget'),
    mk('s2', 'Approve the hire', 1, 'PLAN hire'),
  ];
  const job = {
    id: JOB_ID, kind: 'ops-request', title: 'two approvals', status: 'open',
    opened_on: '2026-09-25', due_on: null, closed_on: null, owner_id: EMP.id,
    priority: 'standard', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'forge' }, metadata: {},
  };
  let release: () => void = () => {};
  const held = new Promise<void>((r) => {
    release = r;
  });
  const seen: Seen = { writes: [], begins: [], release: () => release() };

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
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, { ...job, steps }));
  await page.route(/\/api\/auth\/passkey\/assert\/begin$/, (r) => {
    seen.begins.push(JSON.parse(r.request().postData() ?? '{}'));
    return json(r, {
      challenge_id: 'chal-1',
      publicKey: { challenge: 'AAAA', rpId: 'localhost',
        allowCredentials: [{ type: 'public-key', id: 'AAAA' }],
        userVerification: 'required', timeout: 60000 },
    });
  });
  await page.route(/\/api\/auth\/passkey\/assert\/finish$/, (r) => json(r, { ticket: 'ticket-1' }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/(s1|s2)(/.*)?$`), async (r) => {
    const m = new RegExp(`/steps/(s1|s2)(/.*)?$`).exec(r.request().url());
    const id = m?.[1] ?? '?';
    const path = m?.[2] ?? '';
    const method = r.request().method();
    const ticket = (await r.request().headerValue('x-presence-ticket')) ?? undefined;
    seen.writes.push({ method, step: id, path, ticket });
    const step = steps.find((s) => s.id === id)!;
    // The gesture's first write is held until the test has switched steps.
    if (id === 's1' && path === '/metadata') await held;
    if (path === '/metadata') {
      Object.assign(step.metadata, JSON.parse(r.request().postData() ?? '{}'));
      return json(r, step);
    }
    if (path === '/sign-offs') {
      if (presence && !ticket) {
        return json(r, { error: 'presence', required: 'presence', produced: 'session' }, 422);
      }
      step.sign_offs = [{ role: 'platform-admin', authority_id: EMP.id, shape_hash: 'h' }];
      return json(r, step);
    }
    if (presence && !ticket) {
      return json(r, { error: 'presence', required: 'presence', produced: 'session' }, 422);
    }
    step.status = 'completed';
    return json(r, step);
  });
  return seen;
}

async function approveThenSwitch(page: Page, seen: Seen): Promise<void> {
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  await expect(surface.locator('h3')).toHaveText('Approve the budget');
  await surface.getByRole('button', { name: 'Approve' }).click();
  await expect.poll(() => seen.writes.length).toBe(1);
  // Mid-gesture: the approver opens the other step on the rail.
  await page.locator('.sg-rail .rail-row', { hasText: 'Approve the hire' }).click();
  await expect(surface.locator('h3')).toHaveText('Approve the hire');
  seen.release();
}

test('a step switched mid-gesture: the completion lands on the step clicked, never the one now shown', async ({ page }) => {
  const seen = await twoApprovals(page, false);
  await approveThenSwitch(page, seen);

  await expect
    .poll(() => seen.writes.filter((w) => w.method === 'PUT').length)
    .toBe(1);
  expect(seen.writes.filter((w) => w.step === 's2')).toEqual([]);
  expect(seen.writes.map((w) => `${w.method} ${w.step}${w.path}`)).toEqual([
    'PATCH s1/metadata',
    'PUT s1',
  ]);
});

// Design f623e425 D3 (backlog 6c9183de, 2026-09-25) tightened the presence
// half of this: the passkey signs only what is on screen. Once the rail
// shows s2, s1's plan is no longer in front of the approver, so the
// gesture that was aimed at s1 refuses its ceremony and signs nothing —
// where it used to sign s1 unseen. d82b5f60's claim stands unchanged:
// no write of the gesture reaches s2.
test('a presence step switched mid-gesture: nothing is signed, and nothing reaches the step now shown', async ({ page }) => {
  const seen = await twoApprovals(page, true);
  await approveThenSwitch(page, seen);

  const surface = page.locator('.sg-detail');
  await expect(surface.locator('.step-write-error')).toContainText('Nothing was signed');
  expect(seen.writes.filter((w) => w.step === 's2')).toEqual([]);
  // The decision landed on s1 (the write the test held); the stamp that
  // asked for presence was refused, and no ceremony, second stamp or
  // completion followed.
  expect(seen.writes.map((w) => `${w.method} ${w.step}${w.path}`)).toEqual([
    'PATCH s1/metadata',
    'POST s1/sign-offs',
  ]);
  expect(seen.begins).toEqual([]);
});
