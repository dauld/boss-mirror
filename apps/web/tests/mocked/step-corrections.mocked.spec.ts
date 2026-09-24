// A step is drawn with its corrections beside it (design 4105b020,
// backlog 56727f95). A completed step is never rewritten: packet
// f3e091f0's triage evidence still reads "Ordering trap confirmed:  is
// required of every rule", and until the corrections door the fix sat
// in a job-metadata key no reader of the step ever saw. The job GET now
// hands each step its own `corrections`; this is the rendered proof
// that the step surface draws them — the field named, both texts
// verbatim, the signature, a withdrawn one struck and labelled — and
// that the ORIGINAL is still drawn, never replaced. An uncorrected
// step draws no marker at all.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

const MANIFEST = { display_name: 'Algedonic Ales', modules: {}, labels: {} };

const JOB_ID = 'f3e091f0-0000-4000-8000-000000000000';
const STEP_ID = '7a1a9e00-0000-4000-8000-000000000001';
const DAMAGED = 'Ordering trap confirmed:  is required of every rule';

const baseStep = {
  id: STEP_ID,
  job_id: JOB_ID,
  kind: 'task',
  title: 'Measure the claim, choose a route',
  spec_slug: 'triage',
  assignee_id: null,
  status: 'completed',
  sort_order: 1,
  blocked_by: [],
  sign_offs_required: [],
  sign_offs: [],
  completed_on: '2026-09-19',
  notes: null,
  fields: [],
  metadata: { disposition: 'build', evidence: DAMAGED } as Record<string, unknown>,
};

const CORRECTIONS = [
  {
    index: 0,
    step: STEP_ID,
    field: 'evidence',
    reads: 'confirmed:  is',
    should_read: 'confirmed: `why` is',
    why: 'the shell ran the backticked word (2376b89e)',
    by: 'agent-claude',
    at: '2026-09-24T01:00:00Z',
  },
  {
    index: 1,
    step: STEP_ID,
    field: 'evidence',
    reads: 'every rule',
    should_read: 'every rule file',
    why: '',
    by: 'agent-claude',
    at: '2026-09-24T01:05:00Z',
  },
  {
    index: 2,
    step: STEP_ID,
    field: 'evidence',
    withdraws: 1,
    why: 'the original was right',
    by: 'emp-david',
    at: '2026-09-24T01:10:00Z',
  },
];

const baseJob = {
  id: JOB_ID,
  kind: 'backlog-item',
  title: 'Ordering trap',
  status: 'closed',
  subject: { subject_kind: 'custom', id: '/it/backlog' },
  owner_id: 'emp-bootstrap-admin',
  metadata: {} as Record<string, unknown>,
  steps: [baseStep],
};

async function mockApi(page: import('@playwright/test').Page, step: Record<string, unknown>) {
  // Catch-all FIRST: Playwright matches routes in reverse registration
  // order, so later, more specific routes win over this one.
  await page.route('**/api/**', (r) => r.fulfill({ json: { data: [], total: 0 } }));
  await page.route(/\/api\/tenant\/manifest$/, (r) => r.fulfill({ json: MANIFEST }));
  await page.route(/\/api\/people$/, (r) => r.fulfill({ json: [] }));
  await page.route(/\/api\/jobs\/step-plugins.*/, (r) => r.fulfill({ json: [] }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) =>
    r.fulfill({ json: { ...baseJob, steps: [step] } }),
  );
}

test('a corrected step shows each correction beside the original, which stays as recorded', async ({
  page,
}) => {
  await mockApi(page, { ...baseStep, corrections: CORRECTIONS });
  await mountPage(page, `/jobs/${JOB_ID}/steps/${STEP_ID}`, { root: '.step-focus' });

  const marker = page.getByTestId('step-corrections');
  await expect(marker).toBeVisible();

  const entries = marker.getByTestId('step-correction');
  await expect(entries).toHaveCount(2);
  const first = entries.nth(0);
  await expect(first).toContainText('evidence');
  await expect(first).toContainText('confirmed: `why` is');
  await expect(first).toContainText('the shell ran the backticked word');
  await expect(first).toContainText('by agent-claude');

  // The withdrawn one is still drawn, and says by which entry.
  await expect(entries.nth(1)).toContainText('withdrawn by [2]');
  await expect(marker.getByTestId('step-correction-withdrawal')).toContainText('withdraws [1]');

  // The damaged sentence is still on the page: joined, never replaced.
  await expect(page.getByText(DAMAGED, { exact: false }).first()).toBeVisible();
});

test('an uncorrected step draws no marker', async ({ page }) => {
  await mockApi(page, baseStep);
  await mountPage(page, `/jobs/${JOB_ID}/steps/${STEP_ID}`, { root: '.step-focus' });

  await expect(page.getByTestId('step-corrections')).toHaveCount(0);
});
