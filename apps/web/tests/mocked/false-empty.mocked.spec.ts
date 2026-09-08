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
