// Packet cc9d7fc6 — "Step surfaces: silent 400s, phantom statuses, a
// one-fetch downgrade." Pins the three properties the fix restores:
//
//   1. A refused write is VISIBLE: a 400 on Complete renders an inline
//      error and the step does not render completed; clicking again
//      after the outage recovers (writes are retryable).
//   2. No phantom statuses: a rejected decision aborts the
//      stamp/complete chain (approval), and rejected handoff
//      confirmations revert to the server-confirmed state.
//   3. No one-fetch downgrade: a failed step-types registry load shows
//      an error + Retry over the still-usable generic fallback, and
//      Retry restores the real surface without a reload.

import { expect, test, type Page, type Route } from '@playwright/test';

const JOB_ID = 'job-swf-1';

const EMP = { id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };
const EMP2 = { ...EMP, id: 'emp-002', name: 'Robin', role: 'brewer' };

type MockStep = {
  id: string; job_id: string; title: string; kind: string; status: string;
  assignee_id: string | null; sort_order: number; blocked_by: string[];
  sign_offs_required: string[]; sign_offs: unknown[];
  metadata: Record<string, unknown>; notes: string | null;
};

const step = (over: Partial<MockStep>): MockStep => ({
  id: 's1', job_id: JOB_ID, title: 'do the thing', kind: 'task', status: 'active',
  assignee_id: null, sort_order: 0, blocked_by: [], sign_offs_required: [],
  sign_offs: [], metadata: {}, notes: null, ...over,
});

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

type StepTypeRow = { kind: string; label: string; category: string; ux: string;
  description: string; surface?: string };

async function baseMocks(page: Page, steps: MockStep[], stepTypes: StepTypeRow[]) {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });
  const job = {
    id: JOB_ID, kind: 'user-feedback', title: 'Write-failure fixture', status: 'open',
    opened_on: '2026-08-20', due_on: null, closed_on: null, owner_id: EMP.id,
    priority: 'standard', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'fixture' }, metadata: {},
  };
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP, EMP2]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, stepTypes));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, { ...job, steps }));
  return job;
}

const TASK_TYPES: StepTypeRow[] = [
  { kind: 'task', label: 'Task', category: 'generic', ux: 'inline', description: '' },
];

test('a 400 on Complete shows an inline error and the step does not render completed; retrying after the outage completes it', async ({ page }) => {
  const steps = [step({ kind: 'task' })];
  await baseMocks(page, steps, TASK_TYPES);

  let refuse = true;
  const putBodies: Record<string, unknown>[] = [];
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1$`), (r) => {
    const body = JSON.parse(r.request().postData() ?? '{}') as Record<string, unknown>;
    putBodies.push(body);
    if (refuse) return json(r, { error: 'scheduled_at is required at done' }, 400);
    if (body['status'] === 'completed') steps[0].status = 'completed';
    return json(r, steps[0]);
  });

  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  await expect(surface.locator('.step-status')).toHaveText('active');

  // The write is refused — the surface must say so and must not
  // render the step as completed.
  await surface.getByRole('button', { name: 'Complete' }).click();
  await expect(surface.locator('.step-write-error')).toContainText('scheduled_at is required at done');
  await expect(surface.locator('.step-status')).toHaveText('active');
  expect(putBodies.length).toBe(1);

  // The outage clears; the same button retries and recovers.
  refuse = false;
  await surface.getByRole('button', { name: 'Complete' }).click();
  await expect(surface.locator('.step-status')).toHaveText('completed');
  await expect(surface.locator('.step-write-error')).toHaveCount(0);
});

test('a rejected approval decision aborts the chain: inline error, no completion PUT, the choice stays open', async ({ page }) => {
  const steps = [step({ kind: 'sign-off', title: 'approve the design' })];
  await baseMocks(page, steps, [
    { kind: 'sign-off', label: 'Sign-off', category: 'approval', ux: 'inline',
      description: '', surface: 'approval' },
  ]);

  const putBodies: Record<string, unknown>[] = [];
  let stampPosts = 0;
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1/sign-offs$`), (r) => {
    stampPosts += 1;
    return json(r, { ok: true });
  });
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1$`), (r) => {
    putBodies.push(JSON.parse(r.request().postData() ?? '{}') as Record<string, unknown>);
    return json(r, steps[0]);
  });
  // The decision is metadata, so it goes through the step merge door
  // (backlog e39a9d2a) — and that is the write refused here.
  const mergeBodies: Record<string, unknown>[] = [];
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1/metadata$`), (r) => {
    mergeBodies.push(JSON.parse(r.request().postData() ?? '{}') as Record<string, unknown>);
    return json(r, { error: 'decision refused by policy' }, 400);
  });

  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  await surface.getByRole('button', { name: 'Approve' }).click();

  await expect(surface.locator('.step-write-error')).toContainText('decision refused by policy');
  // The rejected decision must not be stamped or completed on top of.
  expect(stampPosts).toBe(0);
  expect(putBodies.length).toBe(0);
  expect(mergeBodies.length).toBe(1);
  expect(mergeBodies[0]['decision']).toBe('approved');
  // No phantom "Decision: approved" — the choice is still open.
  await expect(surface.getByRole('button', { name: 'Approve' })).toBeVisible();
});

test('rejected handoff confirmations revert to the server-confirmed state', async ({ page }) => {
  const steps = [step({
    kind: 'handoff', title: 'cellar to packaging',
    metadata: { from_id: EMP.id, to_id: EMP2.id, from_confirmed: false, to_confirmed: false },
  })];
  await baseMocks(page, steps, [
    { kind: 'handoff', label: 'Handoff', category: 'generic', ux: 'inline',
      description: '', surface: 'handoff' },
  ]);
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1$`), (r) =>
    json(r, { error: 'db down' }, 500));

  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  const boxes = surface.locator('.step-handoff-confirm input');
  await boxes.nth(0).check();
  await boxes.nth(1).check();

  await surface.getByRole('button', { name: 'Complete handoff' }).click();
  await expect(surface.locator('.step-write-error')).toContainText('db down');
  // The surface must not keep rendering confirmations the server
  // rejected — both sides fall back to the confirmed (server) state.
  await expect(boxes.nth(0)).not.toBeChecked();
  await expect(boxes.nth(1)).not.toBeChecked();
});

test('a failed step-types load is an error with a Retry, not a permanent downgrade to the generic surface', async ({ page }) => {
  const steps = [step({ kind: 'sign-off', title: 'approve the design' })];
  await baseMocks(page, steps, []); // step-types below overrides
  let registryUp = false;
  await page.route(/\/api\/jobs\/step-types$/, (r) => {
    if (!registryUp) return json(r, 'registry unavailable', 500);
    return json(r, [
      { kind: 'sign-off', label: 'Sign-off', category: 'approval', ux: 'inline',
        description: '', surface: 'approval' },
    ]);
  });

  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');

  // Degraded, visibly: the generic fallback still renders, with an
  // error + retry affordance — not a silent downgrade.
  await expect(surface.locator('.step-registry-error')).toBeVisible();
  await expect(surface.locator('.step-generic')).toBeVisible();
  await expect(surface.getByRole('button', { name: 'Approve' })).toHaveCount(0);

  registryUp = true;
  await surface.locator('.step-registry-error').getByRole('button', { name: 'Retry' }).click();
  await expect(surface.getByRole('button', { name: 'Approve' })).toBeVisible();
  await expect(surface.locator('.step-registry-error')).toHaveCount(0);
});
