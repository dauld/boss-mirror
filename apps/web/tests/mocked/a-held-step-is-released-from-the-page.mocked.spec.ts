// The review of car 781b9209 (backlog 6ef4a36b) found two things the
// step surface did with a held step. It said a held step "changes hands
// by release, then claim" and offered no release, so an operator could
// not hand one over in the browser. And every write it made carried the
// status the page was DRAWN with: a page drawn while the step was ready
// and unassigned, saved after an agent had claimed it, sent `{status:
// ready, assignee_id: null}` — the release body — and freed the agent's
// claim. The surface now releases on a button, and a Save carries only
// what the gesture changed.

import { expect, test, type Page, type Route } from '@playwright/test';

const JOB_ID = 'job-release-1';

const OPERATOR = {
  id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [],
};

const step = (status: string, assignee: string | null, metadata: Record<string, unknown>) => ({
  id: 's1', job_id: JOB_ID, kind: 'task', title: 'build', status,
  assignee_id: assignee, sort_order: 0, blocked_by: [], sign_offs_required: [],
  sign_offs: [], completed_on: null, metadata, notes: null, spec_slug: 'build',
});

const json = (r: Route, b: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

type Write = { method: string; path: string; body: Record<string, unknown> };

/// What the release's writes and read-back answer: the PUT's status,
/// and the step list the page reads back after it.
type Answers = { putStatus?: number; readBack?: ReturnType<typeof step> };

/// Mocks the page around one step and records every write to it.
async function mocks(
  page: Page,
  s: ReturnType<typeof step>,
  answers: Answers = {},
): Promise<Write[]> {
  const writes: Write[] = [];
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, {
    id: JOB_ID, kind: 'backlog-item', title: 'A step someone is working',
    status: 'open', opened_on: '2026-09-25', due_on: null, closed_on: null,
    owner_id: 'emp-001', priority: 'standard', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'bosspipeline' }, metadata: {}, steps: [s],
  }));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'task', label: 'Task', category: 'generic', ux: 'inline', description: '' },
  ]));
  await page.route(/\/api\/people$/, (r) => json(r, [OPERATOR]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: OPERATOR.id, role: OPERATOR.role }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1(/metadata)?$`), (r) => {
    const req = r.request();
    writes.push({
      method: req.method(),
      path: new URL(req.url()).pathname,
      body: req.postDataJSON() as Record<string, unknown>,
    });
    const status = req.method() === 'PUT' ? (answers.putStatus ?? 204) : 204;
    return status === 204
      ? r.fulfill({ status, body: '' })
      : json(r, { error: 'refused by the mock' }, status);
  });
  // The read-back the release makes after its writes.
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps$`), (r) =>
    json(r, [answers.readBack ?? s]));
  return writes;
}

const WHY = 'handing over to the day shift';
const released = (from: ReturnType<typeof step>) =>
  step('ready', null, { ...from.metadata, agent_run: undefined, released: { why: WHY } });

test('an active held step is released from the page, with its reason, and read back', async ({ page }) => {
  // The page is drawn from a snapshot with NO run edge — the dispatcher
  // wrote `agent_run` after it loaded. The release clears it anyway
  // (the review of car 675f1858, #1).
  const held = step('active', 'agent-claude', {});
  const writes = await mocks(page, held, { readBack: released(held) });
  await page.goto(`/ux/jobs/${JOB_ID}`);

  const surface = page.locator('.step-generic');
  await expect(surface).toBeVisible();
  await expect(surface.getByText(/changes hands by release, then claim/)).toBeVisible();
  let asked = '';
  page.once('dialog', (d) => {
    asked = d.message();
    void d.accept(WHY);
  });
  await surface.getByRole('button', { name: 'Release' }).click();

  // The writes `boss step release` makes: the run edge cleared and the
  // `released` stamp recorded through the merge door (#2), then the
  // release — ready, nobody's.
  await expect.poll(() => writes.length).toBe(2);
  expect(asked).toMatch(/why/i);
  expect(writes).toEqual([
    {
      method: 'PATCH',
      path: `/api/jobs/${JOB_ID}/steps/s1/metadata`,
      body: {
        agent_run: null,
        released: { why: WHY, by: OPERATOR.id, at: expect.any(String), from_run: null },
      },
    },
    {
      method: 'PUT',
      path: `/api/jobs/${JOB_ID}/steps/s1`,
      body: { status: 'ready', assignee_id: null },
    },
  ]);
  await expect(surface.getByText(/PARTIAL RELEASE/)).toHaveCount(0);
});

test('a cancelled reason releases nothing', async ({ page }) => {
  const writes = await mocks(page, step('active', 'agent-claude', {}));
  await page.goto(`/ux/jobs/${JOB_ID}`);

  const surface = page.locator('.step-generic');
  await expect(surface).toBeVisible();
  page.once('dialog', (d) => void d.dismiss());
  await surface.getByRole('button', { name: 'Release' }).click();
  await expect(surface.getByRole('button', { name: 'Release' })).toBeEnabled();
  expect(writes).toEqual([]);
});

test('a status write refused after the merge shows a partial release, loudly', async ({ page }) => {
  // The merge landed — reason recorded, run edge cleared — and the PUT
  // was refused, so the step is still held. The page says exactly that.
  const held = step('active', 'agent-claude', { agent_run: 'run-1' });
  const writes = await mocks(page, held, {
    putStatus: 409,
    readBack: step('active', 'agent-claude', { released: { why: WHY } }),
  });
  await page.goto(`/ux/jobs/${JOB_ID}`);

  const surface = page.locator('.step-generic');
  await expect(surface).toBeVisible();
  page.once('dialog', (d) => void d.accept(WHY));
  await surface.getByRole('button', { name: 'Release' }).click();

  const alert = surface.getByRole('alert');
  await expect(alert).toContainText('PARTIAL RELEASE');
  await expect(alert).toContainText('refused by the mock');
  await expect(alert).toContainText('reads back active');
  expect(writes.map((w) => w.method)).toEqual(['PATCH', 'PUT']);
});

test('a step nobody holds offers no release', async ({ page }) => {
  await mocks(page, step('ready', OPERATOR.id, {}));
  await page.goto(`/ux/jobs/${JOB_ID}`);

  const surface = page.locator('.step-generic');
  await expect(surface).toBeVisible();
  await expect(surface.getByRole('button', { name: 'Start' })).toBeVisible();
  await expect(surface.getByRole('button', { name: 'Release' })).toHaveCount(0);
});

test('a Save sends no status and no holder the operator did not change', async ({ page }) => {
  // Drawn while the step was ready and unassigned — the snapshot a
  // claim made after this page loaded does not reach.
  const writes = await mocks(page, step('ready', null, {}));
  await page.goto(`/ux/jobs/${JOB_ID}`);

  const surface = page.locator('.step-generic');
  await expect(surface).toBeVisible();
  await surface.locator('#due-s1').fill('2026-10-01');
  await surface.getByRole('button', { name: 'Save assignment' }).click();

  await expect.poll(() => writes.length).toBe(2);
  const put = writes.find((w) => w.method === 'PUT');
  expect(put, 'the Save still writes the step').toBeDefined();
  expect(put!.body, 'no snapshot status rides a Save').not.toHaveProperty('status');
  expect(put!.body, 'an untouched picker is not a holder change').not.toHaveProperty(
    'assignee_id',
  );
  const patch = writes.find((w) => w.method === 'PATCH');
  expect(patch?.body).toEqual({ due_on: '2026-10-01' });
});
