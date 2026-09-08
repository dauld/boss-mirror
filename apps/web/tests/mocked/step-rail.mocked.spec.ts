// The job page's step area: surface WIDE left, workflow NARROW right
// (David, 7d63af73). Pins the three properties that make the layout
// worth having rather than just narrower.

import { expect, test, type Page, type Route } from '@playwright/test';

const JOB_ID = 'job-uf-1';
const S = (id: string, title: string, kind: string, status: string, blocked: string[] = []) =>
  ({ id, job_id: JOB_ID, title, kind, status, assignee_id: null, sort_order: 0,
     sign_offs_required: [], sign_offs: [], metadata: {}, notes: null,
     blocked_by: blocked, spec_slug: title });

// A user-feedback packet — the 6-wide fork that makes StepDag's canvas
// 1,314px, which is the whole reason the rail is a different rendering
// rather than a squeezed one.
const STEPS = [
  S('s0', 'submitted', 'trigger', 'completed'),
  S('s1', 'triage', 'task', 'completed', ['s0']),
  S('s2', 'investigate', 'task', 'active', ['s1']),
  S('s3', 'design-review', 'task', 'pending', ['s1']),
  S('s4', 'build', 'task', 'pending', ['s1']),
  S('s5', 'needs-info', 'task', 'skipped', ['s1']),
  S('s6', 'duplicate', 'outcome', 'skipped', ['s1']),
  S('s7', 'declined', 'outcome', 'skipped', ['s1']),
  S('s8', 'closed', 'outcome', 'pending', ['s2', 's3', 's4', 's5']),
].map((s, i) => ({ ...s, sort_order: i }));

const JOB = {
  id: JOB_ID, kind: 'user-feedback', title: 'Feedback on /it',
  status: 'open', opened_on: '2026-08-15', due_on: null, closed_on: null,
  owner_id: 'emp-david', priority: 'standard', simulated: false, tags: [],
  subject: { subject_kind: 'custom', id: '/it' }, metadata: {}, steps: STEPS,
};

const EMP = { id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

async function mocks(page: Page) {
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await page.route(/\/api\/session$/, (r) => json(r, { username: 'david', employee_id: 'emp-001', role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/live$/, (r) => json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, JOB));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'task', label: 'Task', category: 'generic', ux: 'inline', description: '' },
    { kind: 'trigger', label: 'Trigger', category: 'generic', ux: 'inline', description: '' },
    { kind: 'outcome', label: 'Outcome', category: 'generic', ux: 'inline', description: '' },
  ]));
}

test('the workflow renders as a rail beside the step surface', async ({ page }) => {
  await mocks(page);
  await page.goto(`/ux/jobs/${JOB_ID}`);

  const rail = page.locator('.sg-rail');
  await expect(rail).toBeVisible();
  // Every step is reachable in the rail — the canvas showed all nine
  // too, it just needed 1,314px to do it.
  await expect(rail.locator('.rail-row')).toHaveCount(9);
  // The active step is the anchor, because the wide panel is showing it.
  await expect(rail.locator('.rail-row.is-selected')).toHaveText(/investigate/);

  // The fork's branches are indented as siblings; the spine is not.
  await expect(rail.locator('.rail-row.is-branch')).toHaveCount(6);

  // The surface comes FIRST in the DOM so a screen reader and a narrow
  // viewport reach the thing being worked before the map of it.
  const order = await page.locator('.sg > *').evaluateAll((els) =>
    els.map((e) => e.className),
  );
  expect(order[0]).toContain('sg-detail');
  expect(order[1]).toContain('sg-rail');

  // The canvas keeps the job the rail cannot do, collapsed until asked.
  await expect(page.locator('.sg-canvas')).toBeVisible();
  await expect(page.locator('.sg-canvas .dag')).toBeHidden();
});

test('job page rail screenshot', async ({ page }, testInfo) => {
  await mocks(page);
  await page.goto(`/ux/jobs/${JOB_ID}`);
  await page.waitForTimeout(1200);
  await page.screenshot({ path: testInfo.outputPath('rail.png'), fullPage: true });
});
