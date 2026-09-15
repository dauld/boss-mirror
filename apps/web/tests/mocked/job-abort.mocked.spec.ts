// Aborting a job asks for the reason (design c6f9fb3e, backlog
// 7a98040e). The job page's Abort control, end to end against a
// mocked step API:
//   1. a job whose workflow declares an aborted terminal shows the
//      control; the modal names the terminal, refuses an empty or
//      one-word reason, and on a sentence PUTs {status: completed,
//      metadata: {…existing, reason}} to that step and nothing else;
//   2. a job with no aborted terminal shows no control;
//   3. a viewer without the terminal's authority_role sees the control
//      disabled with the role named.

import { expect, test, type Page, type Route } from '@playwright/test';

const JOB_ID = 'job-abort-1';

const EMP = { id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };
const BREWER = { ...EMP, id: 'emp-002', name: 'Robin', role: 'brewer' };

type MockStep = {
  id: string; job_id: string; title: string; spec_slug: string; kind: string; status: string;
  assignee_id: string | null; sort_order: number; blocked_by: string[];
  sign_offs_required: string[]; sign_offs: unknown[];
  metadata: Record<string, unknown>; notes: string | null; completed_on: string | null;
};

const step = (over: Partial<MockStep>): MockStep => ({
  id: 's1', job_id: JOB_ID, title: 'Triage', spec_slug: 'triage', kind: 'task', status: 'ready',
  assignee_id: null, sort_order: 0, blocked_by: [], sign_offs_required: [],
  sign_offs: [], metadata: {}, notes: null, completed_on: null, ...over,
});

// The shape every workflow row gives its abort: an outcome step with
// `outcome_kind = aborted` stamped from metadata_defaults at admission.
const ABANDONED = step({
  id: 's9', title: 'Abandoned', spec_slug: 'abandoned', kind: 'outcome', status: 'pending',
  sort_order: 9, metadata: { outcome_kind: 'aborted' },
});

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

async function baseMocks(page: Page, steps: MockStep[], viewer = EMP) {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });
  const job = {
    id: JOB_ID, kind: 'backlog-item', title: 'Abort fixture', status: 'open',
    opened_on: '2026-09-15', due_on: null, closed_on: null, owner_id: EMP.id,
    priority: 'standard', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'fixture' }, metadata: {},
  };
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP, BREWER]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: viewer.name.toLowerCase(), employee_id: viewer.id, role: viewer.role }));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, []));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, { ...job, steps }));
}

test('the Abort control names the terminal, requires a sentence, and completes that step with the reason', async ({ page }) => {
  const steps = [step({}), ABANDONED];
  await baseMocks(page, steps);

  const puts: { url: string; body: Record<string, unknown> }[] = [];
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/[^/]+$`), (r) => {
    const body = JSON.parse(r.request().postData() ?? '{}') as Record<string, unknown>;
    puts.push({ url: r.request().url(), body });
    return json(r, { ...ABANDONED, ...body });
  });

  await page.goto(`/ux/jobs/${JOB_ID}`);
  const abort = page.getByRole('button', { name: 'Abort…' });
  await expect(abort).toBeVisible();
  await expect(abort).toBeEnabled();
  await abort.click();

  const modal = page.getByRole('dialog', { name: 'Abort this job' });
  await expect(modal).toBeVisible();
  // The modal says which terminal it will complete — by the row's own
  // title and slug, so the operator sees what the protocol will record.
  await expect(modal.locator('.am-terminal')).toContainText('Completes Abandoned (abandoned)');

  // The reason is required: nothing typed, then one word, and the
  // submit stays disabled with no write sent.
  const submit = modal.getByRole('button', { name: 'Abort job' });
  await expect(submit).toBeDisabled();
  await modal.getByLabel('Reason').fill('dup');
  await expect(submit).toBeDisabled();
  expect(puts.length).toBe(0);

  await modal.getByLabel('Reason').fill('Filed twice; the other packet carries the work.');
  await expect(submit).toBeEnabled();
  await submit.click();

  await expect(modal).toHaveCount(0);
  expect(puts.length).toBe(1);
  expect(puts[0]?.url).toContain(`/api/jobs/${JOB_ID}/steps/s9`);
  expect(puts[0]?.body).toEqual({
    status: 'completed',
    metadata: { outcome_kind: 'aborted', reason: 'Filed twice; the other packet carries the work.' },
  });
});

test('a job whose workflow declares no aborted terminal shows no control', async ({ page }) => {
  await baseMocks(page, [
    step({}),
    step({ id: 's8', title: 'Closed', spec_slug: 'closed', kind: 'outcome', status: 'pending',
      sort_order: 8, metadata: { outcome_kind: 'completed' } }),
  ]);
  await page.goto(`/ux/jobs/${JOB_ID}`);
  await expect(page.locator('h1')).toContainText('Abort fixture');
  await expect(page.getByRole('button', { name: 'Abort…' })).toHaveCount(0);
});

test('a viewer without the terminal\'s authority_role sees the control disabled with the role named', async ({ page }) => {
  await baseMocks(page, [
    step({}),
    { ...ABANDONED, metadata: { outcome_kind: 'aborted', authority_role: 'platform-admin' } },
  ], BREWER);
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const abort = page.getByRole('button', { name: 'Abort…' });
  await expect(abort).toBeVisible();
  await expect(abort).toBeDisabled();
  await expect(abort).toHaveAttribute('title', 'Only platform-admin may abort this job.');
  await expect(page.locator('.jd-abort-why')).toContainText('platform-admin');
});
