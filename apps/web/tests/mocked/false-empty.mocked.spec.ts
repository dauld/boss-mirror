// Packet 3fba9c35 — "Failures render as data: the false-empty sweep."
// A fetch failure must render as a FAILURE, visibly distinct from
// "loaded fine and truly empty". Pins the two audited instances:
//
//   TriageBoard — a failed /api/workflows used to leave the fork
//   null, so already-routed cards printed under "Waiting on triage /
//   Nobody has routed these yet" while the jobs fetch had succeeded.
//   The registry read is part of the board's truth; its failure is
//   the board's failure.
//
//   Inbox — a failed /api/messages/inbox/{id} left `messages` empty,
//   so the header announced "Nothing is waiting on you" during an
//   outage. Failure now renders with the error and a Retry.

import { expect, test, type Page, type Route } from '@playwright/test';

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const EMP = { id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

// ---- triage board -----------------------------------------------------

const DISPOSITIONS = 'reproduce|design|build|duplicate|needs-info|decline';

const KIND = {
  kind: 'user-feedback', version: 1, status: 'active',
  steps: [
    { title: 'submitted', kind: 'trigger', ready_when: 'true', title_template: 'Feedback submitted', fields: [] },
    { title: 'triage', kind: 'task', ready_when: 'steps.submitted.done', title_template: 'Triage feedback',
      fields: [{ name: 'disposition', field_type: DISPOSITIONS, required: true }] },
    { title: 'design-review', kind: 'task',
      ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "design"',
      title_template: 'Decide the design', fields: [] },
  ],
};

/// One already-routed packet: its fork step is completed with a
/// disposition. Under a healthy registry it renders in the route
/// column, never under "Waiting on triage".
const ROUTED_JOB = {
  id: 'fb-routed', kind: 'user-feedback', title: 'Feedback on /ux/jobs', status: 'open',
  subject: { subject_kind: 'custom', id: '/ux/jobs' }, owner_id: EMP.id,
  metadata: { message: 'Needs a design call', route: '/ux/jobs' },
  steps: [
    { id: 'fb-routed-t', kind: 'trigger', status: 'completed', fields: [], metadata: {} },
    { id: 'fb-routed-a', kind: 'task', status: 'completed',
      fields: [{ name: 'disposition', field_type: DISPOSITIONS, required: true }],
      metadata: { authority_role: 'platform-admin', disposition: 'design' } },
    { id: 'fb-routed-o', kind: 'outcome', status: 'pending', fields: [], metadata: {} },
  ],
};

async function triageMocks(page: Page) {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/tenant\/manifest$/, (r) =>
    json(r, { display_name: 'Algedonic Ales', modules: {}, labels: {} }));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: 'platform-admin' }));
}

test('a failed workflows read fails the board — routed cards do not print under "Nobody has routed these yet"', async ({ page }) => {
  await triageMocks(page);
  await page.route(/\/api\/jobs\?kind=user-feedback/, (r) =>
    json(r, { data: [ROUTED_JOB], total: 1 }));
  await page.route(/\/api\/workflows$/, (r) => json(r, 'registry down', 500));

  await page.goto('/it/design/feedback');
  await expect(page.locator('.tb-err')).toBeVisible();
  await expect(page.locator('.tb-err')).toContainText('workflows');
  // The false-empty this replaces: the routed card misfiled as
  // untriaged under a column claiming nobody has routed it.
  await expect(page.getByText('Nobody has routed these yet.')).toHaveCount(0);
  await expect(page.locator('.tb-card')).toHaveCount(0);
});

test('a truly empty queue still reads as empty, not as a failure', async ({ page }) => {
  await triageMocks(page);
  await page.route(/\/api\/jobs\?kind=user-feedback/, (r) => json(r, { data: [], total: 0 }));
  await page.route(/\/api\/workflows$/, (r) => json(r, [KIND]));

  await page.goto('/it/design/feedback');
  await expect(page.locator('.tb-msg')).toBeVisible();
  await expect(page.locator('.tb-err')).toHaveCount(0);
});

// ---- inbox ------------------------------------------------------------

const MSG = {
  id: 'msg-1', sender_id: 'system', recipient_id: EMP.id, kind: 'direct',
  subject: 'A thing needs you', body: 'Please look at the thing.',
  sent_at: '2026-08-20T10:00:00Z', read_at: null, entity_ref: null,
};

async function inboxMocks(page: Page) {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: 'platform-admin' }));
}

test('an inbox outage renders as a failure with Retry — never as "Nothing is waiting on you"', async ({ page }) => {
  await inboxMocks(page);
  let up = false;
  await page.route(/\/api\/messages\/inbox\//, (r) =>
    up ? json(r, [MSG]) : json(r, 'message store down', 500));

  await page.goto('/inbox');
  await expect(page.locator('.load-failed')).toBeVisible();
  await expect(page.locator('.load-failed')).toContainText('HTTP 500');
  // The header must not claim an empty inbox during an outage.
  await expect(page.getByText('Nothing is waiting on you')).toHaveCount(0);

  // The outage clears; Retry recovers without a reload.
  up = true;
  await page.getByRole('button', { name: 'Retry' }).click();
  await expect(page.getByText('1 waiting on you')).toBeVisible();
  await expect(page.locator('.load-failed')).toHaveCount(0);
});

test('a truly empty inbox still reads as empty', async ({ page }) => {
  await inboxMocks(page);
  await page.route(/\/api\/messages\/inbox\//, (r) => json(r, []));

  await page.goto('/inbox');
  await expect(page.getByText('Nothing is waiting on you')).toBeVisible();
  await expect(page.locator('.load-failed')).toHaveCount(0);
});

// ---- HR onboarding tasks ----------------------------------------------
//
// Backlog a704c5eb. `fetchTasks` collapsed three outcomes — no HR Job, a
// non-ok steps read, a thrown error — into `tasks = []`, behind a
// template guard of `tasks.length > 0`. A broken read therefore said
// "this employee has no onboarding tasks".
//
// /ux/hr IS crawled by route-smoke.mocked.spec.ts and PASSED the whole
// time, because the page rendered fine; it just rendered a falsehood. A
// crawl asserts no runtime crash and structurally cannot assert that an
// empty list means empty rather than unread. These two tests are that
// assertion: the same page, the same adversarial backend, once with the
// step read broken and once with it genuinely empty, and the two must
// not look alike.

const HR_WORKFLOW = {
  kind: 'onboarding', label: 'Onboarding', version: 1, status: 'active',
  subject_kinds: ['employee'], metadata: { surfaces: ['hr'] },
  metadata_schema: {}, entitlements: {}, owning_team: 'hr',
  authoring_job_id: null, created_at: '2026-01-01T00:00:00Z', steps: [],
};

/// One open onboarding Job about EMP, in the envelope the live endpoint
/// actually sends: `{data:[…]}`, with a NESTED subject.
const HR_JOB = {
  id: 'job-onboard-1', kind: 'onboarding', status: 'open',
  subject: { subject_kind: 'employee', id: EMP.id },
  owner_id: EMP.id, title: 'Onboard David', metadata: {},
};

const HR_STEP = {
  id: 'step-1', job_id: HR_JOB.id, kind: 'it-setup', title: 'Issue a laptop',
  status: 'ready', assignee_id: null, completed_on: null, metadata: {},
};

async function hrMocks(page: Page, steps: (r: Route) => Promise<void>) {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/tenant\/manifest$/, (r) =>
    json(r, { display_name: 'Algedonic Ales', modules: {}, labels: {} }));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: 'platform-admin' }));
  await page.route(/\/api\/workflows$/, (r) => json(r, [HR_WORKFLOW]));
  await page.route(/\/api\/jobs\?kind=onboarding/, (r) =>
    json(r, { data: [HR_JOB], total: 1 }));
  // Every step read — the progress column's and the tasks table's.
  await page.route(/\/api\/jobs\/job-onboard-1\/steps$/, steps);
}

async function openHrWorkflows(page: Page) {
  await page.goto('/ux/hr');
  await page.getByRole('tab', { name: 'Workflows' }).click();
  await expect(page.getByRole('button', { name: 'View tasks' })).toBeVisible();
  await page.getByRole('button', { name: 'View tasks' }).click();
}

test('a failed step read says so — never "this employee has no onboarding tasks"', async ({ page }) => {
  await hrMocks(page, (r) => json(r, 'steps unavailable', 503));
  await openHrWorkflows(page);

  const failures = page.locator('.load-failed');
  await expect(failures.first()).toBeVisible();
  await expect(failures.filter({ hasText: 'onboarding steps' })).toContainText('503');
  // The empty-state words belong to a successful read. None of them may
  // appear while the read is broken.
  await expect(page.getByText('This workflow has no steps')).toHaveCount(0);
  await expect(page.getByText('has no open HR workflow')).toHaveCount(0);
  // Same class one row up: a failed step read must not render as 0%.
  await expect(page.getByText('0/0 tasks')).toHaveCount(0);
  await expect(failures.filter({ hasText: 'Progress unknown' })).toBeVisible();
});

test('a Job that genuinely has no steps still reads as empty, not as a failure', async ({ page }) => {
  await hrMocks(page, (r) => json(r, []));
  await openHrWorkflows(page);

  await expect(page.getByText('This workflow has no steps')).toBeVisible();
  await expect(page.locator('.load-failed')).toHaveCount(0);
});

test('a healthy read renders the task', async ({ page }) => {
  await hrMocks(page, (r) => json(r, [HR_STEP]));
  await openHrWorkflows(page);

  await expect(page.getByText('Issue a laptop')).toBeVisible();
  await expect(page.locator('.load-failed')).toHaveCount(0);
  // The envelope + nested-subject read, proven end to end: before this
  // car the page looked for `payload.jobs` and flat `subject_kind`, so
  // this row could not exist and the tasks table was unreachable.
  await expect(page.getByText('0/1 tasks (0%)')).toBeVisible();
});
