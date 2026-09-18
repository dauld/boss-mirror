// The decision-context panel (19db52de): a step surface must present
// the packet's case to whoever acts on it. Rendered proof for the two
// shapes that were blind in David's queue on 2026-08-18 — a sign-off
// showing only its stamp button, and a Decide-the-design task whose
// filed message sat unrendered in job metadata.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

const MANIFEST = { display_name: 'Algedonic Ales', modules: {}, labels: {} };

const JOB_ID = '99cfb52b-fca1-4e69-8798-f02575faf592';
const STEP_ID = 'ab8036d5-0326-46be-9585-90c4636d9116';

const baseStep = {
  id: STEP_ID,
  job_id: JOB_ID,
  kind: 'sign-off',
  title: 'Approve publishing to the public mirror',
  assignee_id: 'emp-bootstrap-admin',
  status: 'ready',
  sort_order: 1,
  blocked_by: [],
  sign_offs_required: ['platform-admin'],
  sign_offs: [],
  completed_on: null,
  notes: null,
  fields: [],
  metadata: { authority_role: 'platform-admin' } as Record<string, unknown>,
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

async function mockApi(
  page: import('@playwright/test').Page,
  step: typeof baseStep,
  job: typeof baseJob,
) {
  // Catch-all FIRST: Playwright matches routes in reverse registration
  // order, so later, more specific routes win over this one.
  await page.route('**/api/**', (r) => r.fulfill({ json: { data: [], total: 0 } }));
  await page.route(/\/api\/tenant\/manifest$/, (r) => r.fulfill({ json: MANIFEST }));
  await page.route(/\/api\/people$/, (r) => r.fulfill({ json: [] }));
  await page.route(/\/api\/jobs\/step-plugins.*/, (r) => r.fulfill({ json: [] }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => r.fulfill({ json: job }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/${STEP_ID}$`), (r) =>
    r.fulfill({ json: step }),
  );
}

test('a sign-off with step-level context renders the case above the stamp', async ({
  page,
}) => {
  const step = {
    ...baseStep,
    metadata: {
      ...baseStep.metadata,
      context_md:
        'APPROVE: 62 commits / 554 files, secrets scan clean, all 22 newly-public files read.',
    },
  };
  await mockApi(page, step, { ...baseJob, steps: [step] });
  await mountPage(page, `/jobs/${JOB_ID}/steps/${STEP_ID}`, { root: '.step-focus' });

  await expect(page.locator('.step-decision-context')).toBeVisible();
  await expect(page.locator('.sdc-body')).toContainText('62 commits');
  await expect(page.locator('.sdc-source')).toHaveText('written for this step');
});

test('a task with no context of its own surfaces the packet as filed', async ({
  page,
}) => {
  const step = {
    ...baseStep,
    kind: 'task',
    title: 'Decide the design',
    sign_offs_required: [],
  };
  const job = {
    ...baseJob,
    kind: 'user-feedback',
    metadata: { message: 'My Day cannot say whether a packet needs me.' },
    steps: [step],
  };
  await mockApi(page, step, job);
  await mountPage(page, `/jobs/${JOB_ID}/steps/${STEP_ID}`, { root: '.step-focus' });

  await expect(page.locator('.step-decision-context')).toBeVisible();
  await expect(page.locator('.sdc-body')).toContainText('My Day cannot say');
  await expect(page.locator('.sdc-source')).toHaveText('the packet as filed');
});

test('no context anywhere renders no panel, not an empty card', async ({ page }) => {
  await mockApi(page, baseStep, baseJob);
  await mountPage(page, `/jobs/${JOB_ID}/steps/${STEP_ID}`, { root: '.step-focus' });

  await expect(page.locator('.step-decision-context')).toHaveCount(0);
});
