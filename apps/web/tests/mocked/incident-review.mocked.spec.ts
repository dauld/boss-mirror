// incident-review.js — the custom Step UX for the post-mortem's
// "Human review of the findings" step.
//
// Two feedback packets said the review step renders the findings
// unusably (one asked for "a custom step UX that presented the
// findings that I needed to sign-off on"). The findings live in two
// places — the Job's semi-structured metadata and the sibling steps'
// field answers — and the generic surface shows neither. This bundle
// renders both as one readable document, then offers the completion
// the step requires.
//
// Rendered rather than reasoned about (same argument as
// design-doc-packet.mocked.spec.ts): the real bundle file is served
// into a mocked page, because nothing else mounts plugin JS in CI.

import { test, expect } from '@playwright/test';
import { readFileSync } from 'fs';
import { mountPage } from '../smoke/_helpers';
import { installSmokeMocks } from './_smokeMocks';

const PLUGIN = readFileSync(
  new URL('../../../../infra/step-plugins/incident-review.js', import.meta.url),
  'utf8',
);

const REVIEW_STEP = {
  id: 'step-review',
  job_id: 'job-ipm-1',
  spec_slug: 'review',
  title: 'Human review of the findings',
  kind: 'incident-review',
  status: 'ready',
  assignee_id: 'emp-david',
  sort_order: 6,
  blocked_by: [],
  sign_offs_required: [],
  fields: [],
  completed_on: null,
  metadata: { authority_role: 'platform-admin' },
};

const JOB = {
  id: 'job-ipm-1',
  kind: 'incident-post-mortem',
  workflow_version: 1,
  subject: { subject_kind: 'custom', id: 'incident-2026-08-22-etcd' },
  title: 'Post-mortem: cp-2 etcd degradation',
  owner_id: 'emp-david',
  status: 'open',
  priority: 'standard',
  opened_on: '2026-08-22',
  due_on: null,
  closed_on: null,
  tags: [],
  metadata: {
    // The semi-structured shape: known keys + one the renderer has
    // never heard of, which must still render as a labeled block.
    incident_at: '2026-08-22, two windows',
    summary: 'Six queued gates killed while etcd degraded on cp-2.',
    evidence: 'readyz verbose (etcd failed) captured 18:1xZ.',
    gate_packets: '0180d213 / 10309c6d / 1954d79f tracked the killed work.',
  },
  steps: [
    {
      id: 's-t0', job_id: 'job-ipm-1', spec_slug: 'opened', title: 'Incident opened',
      kind: 'trigger', status: 'completed', assignee_id: null, sort_order: 0,
      blocked_by: [], completed_on: '2026-08-22',
      metadata: { trigger_kind: 'operator', trigger_name: 'incident-declared' },
    },
    {
      id: 's-t1', job_id: 'job-ipm-1', spec_slug: 'timeline',
      title: 'Establish the timeline from evidence',
      kind: 'task', status: 'completed', assignee_id: 'claude@algedonic.dev',
      sort_order: 1, blocked_by: [], completed_on: '2026-08-22',
      metadata: {
        authority_role: 'platform-admin',
        first_symptom: 'gates vanished from the queue',
        symptom_at: '16:38Z',
        detection_lag_minutes: 94,
      },
    },
    REVIEW_STEP,
  ],
};

async function installIncidentReviewMocks(
  page: import('@playwright/test').Page,
  step: typeof REVIEW_STEP,
) {
  await installSmokeMocks(page);
  const job = { ...JOB, steps: [JOB.steps[0]!, JOB.steps[1]!, step] };
  await page.route('**/api/jobs/job-ipm-1', (r) => r.fulfill({ json: job }));
  await page.route('**/api/jobs/step-plugins', (r) =>
    r.fulfill({
      json: [
        {
          kind: 'incident-review',
          label: 'Incident review',
          category: 'platform',
          version: 1,
          frontend_url: '/plugins/incident-review.js',
          owning_team: 'platform',
        },
      ],
    }),
  );
  await page.route('**/plugins/incident-review.js', (r) =>
    r.fulfill({ contentType: 'application/javascript', body: PLUGIN }),
  );
}

test('the findings render as one document: job metadata + what each step found', async ({
  page,
}) => {
  const errs: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));
  await installIncidentReviewMocks(page, REVIEW_STEP);

  await mountPage(page, '/jobs/job-ipm-1/steps/step-review', { root: '.step-focus' });
  await page.waitForTimeout(1500);

  expect(errs, `plugin threw: ${errs.join(' | ')}`).toEqual([]);

  // The Job's semi-structured metadata, known keys as sections…
  await expect(page.getByText('Six queued gates killed while etcd degraded on cp-2.')).toBeVisible();
  await expect(page.getByText(/readyz verbose/)).toBeVisible();
  // …and the unknown key as a labeled block — content never dropped.
  await expect(page.getByText('Gate packets', { exact: true })).toBeVisible();
  await expect(page.getByText(/0180d213/)).toBeVisible();

  // What the sibling steps found, labeled by their fields.
  await expect(page.getByText('Establish the timeline from evidence')).toBeVisible();
  await expect(page.getByText('First symptom', { exact: true })).toBeVisible();
  await expect(page.getByText('gates vanished from the queue')).toBeVisible();
  // Non-string field answers survive too.
  await expect(page.getByText('94')).toBeVisible();

  // Plumbing keys are not findings.
  await expect(page.getByText('Authority role', { exact: true })).toHaveCount(0);
});

test('completing the review PUTs status=completed and refreshes', async ({ page }) => {
  await installIncidentReviewMocks(page, REVIEW_STEP);
  const puts: Array<Record<string, unknown>> = [];
  // The plugin reads the row back before completing (the completion
  // PUT replaces metadata wholesale, so it must carry the row as it
  // stands, never the page-load snapshot) — the job's steps list is
  // the read the API offers.
  await page.route('**/api/jobs/job-ipm-1/steps', (r) => r.fulfill({ json: [REVIEW_STEP] }));
  await page.route('**/api/jobs/job-ipm-1/steps/step-review', (r) => {
    if (r.request().method() !== 'PUT') return r.fallback();
    const body = JSON.parse(r.request().postData() ?? '{}') as Record<string, unknown>;
    puts.push(body);
    return r.fulfill({ json: { ...REVIEW_STEP, status: 'completed' } });
  });

  await mountPage(page, '/jobs/job-ipm-1/steps/step-review', { root: '.step-focus' });
  await page.getByRole('button', { name: /complete review/i }).click();
  await expect.poll(() => puts.length).toBeGreaterThan(0);
  expect(puts[0]?.['status']).toBe('completed');
});

test('a completed review is read-only — the record, not another form', async ({ page }) => {
  await installIncidentReviewMocks(page, {
    ...REVIEW_STEP,
    status: 'completed',
    completed_on: '2026-08-22',
  });

  await mountPage(page, '/jobs/job-ipm-1/steps/step-review', { root: '.step-focus' });
  await page.waitForTimeout(1000);

  // The findings still render (the archive value of the surface)…
  await expect(page.getByText('Six queued gates killed while etcd degraded on cp-2.')).toBeVisible();
  // …but there is nothing left to press.
  await expect(page.getByRole('button', { name: /complete review/i })).toHaveCount(0);
  await expect(page.getByText(/review recorded/i)).toBeVisible();
});
