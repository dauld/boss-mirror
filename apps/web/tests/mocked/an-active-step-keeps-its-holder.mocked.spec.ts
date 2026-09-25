// An active step keeps the holder that claimed it (backlogs 650ebd0c,
// 0f42efa0). The jobs API refuses a PUT that replaces an active step's
// holder or clears it without releasing the step; the review of car
// fb3e9424 found the step surface still offering exactly that write —
// pick "unassigned", Save, pick someone, Save. The picker on an active
// held step is inert and says how the step changes hands; on a ready
// step, whose nomination nobody has claimed, it stays live.

import { expect, test, type Page, type Route } from '@playwright/test';

const JOB_ID = 'job-held-1';

const OPERATOR = {
  id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [],
};

const step = (status: string, assignee: string | null) => ({
  id: 's1', job_id: JOB_ID, kind: 'task', title: 'build', status,
  assignee_id: assignee, sort_order: 0, blocked_by: [], sign_offs_required: [],
  sign_offs: [], completed_on: null, metadata: {}, notes: null, spec_slug: 'build',
});

const json = (r: Route, b: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

async function mocks(page: Page, s: ReturnType<typeof step>): Promise<void> {
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
}

test('an active held step does not offer its holder for change', async ({ page }) => {
  await mocks(page, step('active', OPERATOR.id));
  await page.goto(`/ux/jobs/${JOB_ID}`);

  const surface = page.locator('.step-generic');
  await expect(surface).toBeVisible();
  // The control is live — the operator can still complete it...
  await expect(surface.getByRole('button', { name: 'Complete' })).toBeEnabled();
  // ...but the picker is inert, and says how the step changes hands.
  await expect(surface.locator('#assignee-s1')).toBeDisabled();
  await expect(surface.getByText(/changes hands by release, then claim/)).toBeVisible();
});

test('a ready step’s nomination can still move', async ({ page }) => {
  await mocks(page, step('ready', OPERATOR.id));
  await page.goto(`/ux/jobs/${JOB_ID}`);

  const surface = page.locator('.step-generic');
  await expect(surface).toBeVisible();
  await expect(surface.locator('#assignee-s1')).toBeEnabled();
  await expect(surface.getByText(/changes hands by release, then claim/)).toHaveCount(0);
});
