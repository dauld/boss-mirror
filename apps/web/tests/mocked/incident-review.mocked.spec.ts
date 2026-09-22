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
import { mountPage } from './_helpers';
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

/// `plugin` and `jobDelayMs` serve the throw pin at the bottom of this
/// file, and nothing else: a plugin that throws only once its data
/// arrives, with the data held back past any wall-clock guess.
async function installIncidentReviewMocks(
  page: import('@playwright/test').Page,
  step: typeof REVIEW_STEP,
  opts: { plugin?: string; jobDelayMs?: number } = {},
) {
  await installSmokeMocks(page);
  const job = { ...JOB, steps: [JOB.steps[0]!, JOB.steps[1]!, step] };
  await page.route('**/api/jobs/job-ipm-1', async (r) => {
    if (opts.jobDelayMs) await new Promise((done) => setTimeout(done, opts.jobDelayMs));
    await r.fulfill({ json: job });
  });
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
    r.fulfill({ contentType: 'application/javascript', body: opts.plugin ?? PLUGIN }),
  );
}

/// THE PLUGIN'S OWN END OF WORK, OBSERVED — not a clock (backlog
/// b1b7021c). This guard used to be `waitForTimeout(1500)` followed by
/// a PLAIN `expect` over `errs`. A plain expect does not retry, because
/// there is nothing to re-poll in an array already collected, so the
/// one check in this file whose whole job is to be loud about a plugin
/// throwing only ever saw a plugin that threw inside the first 1 500 ms
/// — and recorded a throw at 1 600 ms as no throw at all. Measured on
/// this branch with the pin at the bottom of the file: a plugin that
/// throws when its data arrives, with the read held to 1 800 ms, passed
/// the sleep-then-assert guard.
///
/// So wait for what the plugin DOES instead. It paints a placeholder
/// synchronously, fetches the Job once, and repaints the document from
/// the answer; `.sir-head` is in that second paint and nothing else,
/// which makes it the observable end of every path the mount can throw
/// from. Polling it also reads `errs` on every tick, so a plugin that
/// died in mount reports as the throw it was rather than spending the
/// whole visibility budget on a heading that was never going to appear.
///
/// No budget of its own: `expect.poll` inherits the one
/// playwright.mocked.config.ts states for the suite.
async function findingsRendered(
  page: import('@playwright/test').Page,
  errs: readonly string[],
): Promise<void> {
  await expect
    .poll(async () => {
      // Thrown, not returned: a returned mismatch keeps polling and
      // spends the whole budget before it says anything, and a plugin
      // that has already thrown is never going to paint.
      if (errs.length > 0) throw new Error(`plugin threw: ${errs.join(' | ')}`);
      return (await page.locator('.sir-head').count()) > 0;
    })
    .toBe(true);
}

test('the findings render as one document: job metadata + what each step found', async ({
  page,
}) => {
  const errs: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));
  await installIncidentReviewMocks(page, REVIEW_STEP);

  await mountPage(page, '/jobs/job-ipm-1/steps/step-review', { root: '.step-focus' });
  await findingsRendered(page, errs);

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

  // Read again at the LAST moment this test can look: a throw that
  // arrived while the assertions above ran used to be discarded, the
  // array having been read once and never again.
  expect(errs, `plugin threw: ${errs.join(' | ')}`).toEqual([]);
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

  // No settle sleep: the first assertion below RETRIES until the
  // document paints, which is the condition 1 000 ms was guessing at,
  // and it gates the two after it — the absence checks would otherwise
  // pass vacuously against a surface that has not loaded yet.
  // The findings still render (the archive value of the surface)…
  await expect(page.getByText('Six queued gates killed while etcd degraded on cp-2.')).toBeVisible();
  // …but there is nothing left to press.
  await expect(page.getByRole('button', { name: /complete review/i })).toHaveCount(0);
  await expect(page.getByText(/review recorded/i)).toBeVisible();
});

// A PLUGIN THAT THROWS ONLY WHEN ITS DATA ARRIVES — the shape of the
// false green (backlog b1b7021c). Registers, paints the placeholder,
// and throws from the callback that renders the document, so the throw
// cannot land before the read this plugin waits on.
const THROWS_WHEN_ITS_DATA_ARRIVES = `
window.__boss_register_step_plugin('incident-review', function (container, props) {
  var root = document.createElement('div');
  root.className = 'step-surface step-incident-review';
  root.innerHTML = '<p class="sir-empty">Loading the findings\u2026</p>';
  container.appendChild(root);
  fetch('/api/jobs/' + props.jobId).then(function () {
    root.innerHTML = '<div class="sir-head"><h3>Post-mortem findings</h3></div>';
    setTimeout(function () { throw new Error('late plugin throw'); }, 0);
  });
});
`;

test('the throw guard catches a plugin that throws when its data arrives', async ({ page }) => {
  const errs: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));
  await installIncidentReviewMocks(page, REVIEW_STEP, {
    plugin: THROWS_WHEN_ITS_DATA_ARRIVES,
    jobDelayMs: 1_800,
  });

  await mountPage(page, '/jobs/job-ipm-1/steps/step-review', { root: '.step-focus' });

  // The guard must FAIL here, and name the throw. `waitForTimeout(1500)`
  // in its place read the array 300 ms before the throw existed and
  // passed — the false green, reproduced on this branch before the fix.
  await expect(findingsRendered(page, errs)).rejects.toThrow(/late plugin throw/);
});
