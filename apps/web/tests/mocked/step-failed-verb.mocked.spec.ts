// A step a failed verb answer annotated must look troubled (backlog
// 074e1287). jobs.complete_linked_step (v6, f47861a5) leaves the step
// OPEN and writes the verb's last FAILED line on it with the alert it
// filed; measured 2026-09-19 on publish 254177e2, the step surface
// then drew that open-pr exactly like any ready step for hours.
// Rendered proof: the note, the line verbatim, the alert as a link —
// and nothing on a step that was since completed, whose note is
// history.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

const MANIFEST = { display_name: 'Algedonic Ales', modules: {}, labels: {} };

const JOB_ID = '254177e2-0000-4000-8000-000000000000';
const STEP_ID = 'ab8036d5-0326-46be-9585-90c4636d9116';
const ALERT_ID = 'a1e57000-0000-4000-8000-000000000000';
const REQUEST_ID = 'c98a782f-0000-4000-8000-000000000000';
const LINE =
  'publish-github-pr: FAILED — pushing publish/2026-09-18 to the forge as david: fatal: detected dubious ownership';

const baseStep = {
  id: STEP_ID,
  job_id: JOB_ID,
  kind: 'task',
  title: 'open-pr',
  assignee_id: null,
  status: 'ready',
  sort_order: 5,
  blocked_by: [],
  sign_offs_required: [],
  sign_offs: [],
  completed_on: null,
  notes: null,
  fields: [],
  metadata: {
    ops_verb: 'publish-github-pr',
    failed: LINE,
    failed_exit: '1',
    failed_source: REQUEST_ID,
    alert: ALERT_ID,
  } as Record<string, unknown>,
};

const baseJob = {
  id: JOB_ID,
  kind: 'publish-to-github',
  title: 'Publish to the public GitHub mirror',
  status: 'open',
  subject: { subject_kind: 'custom', id: 'github-mirror' },
  owner_id: 'emp-bootstrap-admin',
  metadata: {} as Record<string, unknown>,
  steps: [baseStep],
};

async function mockApi(page: import('@playwright/test').Page, step: typeof baseStep) {
  // Catch-all FIRST: Playwright matches routes in reverse registration
  // order, so later, more specific routes win over this one.
  await page.route('**/api/**', (r) => r.fulfill({ json: { data: [], total: 0 } }));
  await page.route(/\/api\/tenant\/manifest$/, (r) => r.fulfill({ json: MANIFEST }));
  await page.route(/\/api\/people$/, (r) => r.fulfill({ json: [] }));
  await page.route(/\/api\/jobs\/step-plugins.*/, (r) => r.fulfill({ json: [] }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) =>
    r.fulfill({ json: { ...baseJob, steps: [step] } }),
  );
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/${STEP_ID}$`), (r) =>
    r.fulfill({ json: step }),
  );
}

test('an open step whose verb FAILED shows the line, the exit and the alert as a link', async ({
  page,
}) => {
  await mockApi(page, baseStep);
  await mountPage(page, `/jobs/${JOB_ID}/steps/${STEP_ID}`, { root: '.step-focus' });

  const note = page.getByTestId('step-failed-verb');
  await expect(note).toBeVisible();
  await expect(note).toContainText('FAILED (exit 1)');
  await expect(note).toContainText('detected dubious ownership');
  const alert = note.getByRole('link', { name: ALERT_ID.slice(0, 8) });
  await expect(alert).toHaveAttribute('href', new RegExp(`/jobs/${ALERT_ID}$`));
  await expect(note.getByRole('link', { name: REQUEST_ID.slice(0, 8) })).toHaveCount(1);
});

test('the same note on a completed step is history, not an alarm', async ({ page }) => {
  await mockApi(page, { ...baseStep, status: 'completed', completed_on: '2026-09-19' });
  await mountPage(page, `/jobs/${JOB_ID}/steps/${STEP_ID}`, { root: '.step-focus' });

  await expect(page.getByTestId('step-failed-verb')).toHaveCount(0);
});
