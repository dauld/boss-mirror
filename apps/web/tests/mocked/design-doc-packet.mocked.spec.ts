// A design doc that carries its own questions is reviewable the moment
// the packet exists — no file on deployed main, no reindex, no round
// trip.
//
// This is the property the whole change is for. `review-design.js` used
// to carry a 404 message apologising that "review Jobs are instant data
// but docs ride trains, so a review can exist before its doc reaches
// deployed main"; David, 2026-08-16: "our lack of good protocol around
// design docs, and the plumbing being broken too, is causing major
// slowdowns in my design review handling speed." That message and the
// fetch behind it are gone (2026-09-10) — the second test below is what
// replaced them.
//
// Rendered rather than reasoned about, because nothing else mounts this
// bundle: nine step-plugin bundles ship and CI mounts exactly one, in
// the live-backend suite (workflow-ux-as-data Q4). A change to plugin
// JS is otherwise verified by a human opening the page.

import { test, expect } from '@playwright/test';
import { readFileSync } from 'fs';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const PLUGIN = readFileSync(
  new URL('../../../../infra/step-plugins/review-design.js', import.meta.url),
  'utf8',
);

const STEP = {
  id: 'step-1',
  spec_slug: 'review',
  title: 'review',
  kind: 'review-design',
  status: 'ready',
  assignee_id: 'emp-david',
  blocked_by: [],
  metadata: {
    title: 'Design: the physical layer, virtualized',
    markdown: '# The claim\n\nBOSS models its own physical world nowhere.',
    questions: [
      { anchor: 'Q1', title: 'New Subject kinds, or Classes of asset?', proposal: 'New kinds.' },
      { anchor: 'Q2', title: 'Tree, or database?', proposal: 'Declared in the tree.' },
    ],
  },
};

const JOB = {
  id: 'job-dd-1',
  kind: 'design-doc',
  workflow_version: 1,
  subject: { subject_kind: 'custom', id: 'bossnet-physical-topology' },
  title: 'Design: the physical layer, virtualized',
  owner_id: 'emp-david',
  status: 'open',
  priority: 'standard',
  opened_on: '2026-08-16',
  due_on: null,
  closed_on: null,
  metadata: {},
  steps: [STEP],
};

test('a self-carried design doc renders its questions without touching the docs API', async ({
  page,
}) => {
  const docsApiCalls: string[] = [];
  const errs: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));

  // The shared installer first: it registers the catch-all and the
  // chrome's reads, and Playwright matches routes in REVERSE
  // registration order, so everything below overrides it.
  await installSmokeMocks(page);
  // The assertion that matters: this must never be called.
  await page.route('**/api/design/**', (r) => {
    docsApiCalls.push(r.request().url());
    return r.fulfill({ status: 500, body: 'the packet must not need me' });
  });
  await page.route('**/api/jobs/job-dd-1', (r) => r.fulfill({ json: JOB }));
  await page.route('**/api/jobs/step-plugins', (r) =>
    r.fulfill({
      json: [
        {
          kind: 'review-design',
          label: 'Review Design',
          category: 'platform',
          version: 1,
          frontend_url: '/plugins/review-design.js',
          owning_team: 'platform',
        },
      ],
    }),
  );
  await page.route('**/plugins/review-design.js', (r) =>
    r.fulfill({ contentType: 'application/javascript', body: PLUGIN }),
  );

  // The step surface deliberately renders OUTSIDE `.app-shell` to take
  // the whole viewport, so it passes its own root.
  await mountPage(page, '/jobs/job-dd-1/steps/step-1', { root: '.step-focus' });
  await page.waitForTimeout(2000);

  expect(errs, `plugin threw: ${errs.join(' | ')}`).toEqual([]);
  // Both questions on the surface, by their own text.
  await expect(page.getByText('New Subject kinds, or Classes of asset?')).toBeVisible();
  await expect(page.getByText('Tree, or database?')).toBeVisible();
  // The prose rode with the packet.
  await expect(page.getByText(/BOSS models its own physical world nowhere/)).toBeVisible();
  // And it said what it is, rather than three `undefined`s where a
  // file's path/status/word-count would go.
  await expect(page.getByText('carried by this packet · not yet a file')).toBeVisible();
  // The whole point: no docs API, so no dependence on the doc having shipped.
  expect(docsApiCalls, 'the packet reached for the docs API').toEqual([]);
});

// THE POINTER-ONLY PACKET, now that the docs API is gone.
//
// This test used to assert the opposite: that a step carrying only
// `doc_path` still fetched `/api/design/docs/{path}` and rendered the
// questions parsed out of the file. That fallback — and the corpus
// index behind it — was deleted on 2026-09-10 (backlog f5da586c); the
// packet is the doc. What is worth pinning is that such a packet fails
// LEGIBLY rather than hanging on a fetch that 404s, and that the
// message says what to do instead. A surface that dead-ends is the
// defect this file has caught three times.
test('a step carrying only a pointer says so, and never calls a docs API', async ({ page }) => {
  const docsApiCalls: string[] = [];
  const errs: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));

  await installSmokeMocks(page);
  // Any call to the deleted service is a failure, so it answers 500.
  await page.route('**/api/design/**', (r) => {
    docsApiCalls.push(r.request().url());
    return r.fulfill({ status: 500, body: 'the docs API is deleted' });
  });
  const legacyStep = {
    ...STEP,
    metadata: { doc_path: 'docs/design/legacy.md' }, // no `questions`, no `markdown`
  };
  await page.route('**/api/jobs/job-dd-1', (r) =>
    r.fulfill({ json: { ...JOB, kind: 'design-doc-review', steps: [legacyStep] } }),
  );
  await page.route('**/api/jobs/step-plugins', (r) =>
    r.fulfill({
      json: [
        {
          kind: 'review-design',
          label: 'Review Design',
          category: 'platform',
          version: 1,
          frontend_url: '/plugins/review-design.js',
          owning_team: 'platform',
        },
      ],
    }),
  );
  await page.route('**/plugins/review-design.js', (r) =>
    r.fulfill({ contentType: 'application/javascript', body: PLUGIN }),
  );

  await mountPage(page, '/jobs/job-dd-1/steps/step-1', { root: '.step-focus' });
  await page.waitForTimeout(2000);

  expect(errs, `plugin threw: ${errs.join(' | ')}`).toEqual([]);
  // It names the pointer it was given and the verb that replaces it.
  await expect(page.getByText(/carries only a pointer/)).toBeVisible();
  await expect(page.getByText(/docs\/design\/legacy\.md/)).toBeVisible();
  expect(docsApiCalls, 'nothing may call the deleted docs API').toEqual([]);
});

// EXHIBITS, IN A REAL BROWSER (design 26a89f11, backlog 73ef81fa).
//
// The unit test pins the frame's attributes; this pins what they DO. The
// review surface runs in the reviewer's authenticated session and any
// actor may write step metadata, so an exhibit is untrusted markup that
// must run its own script (a motion prototype is the first customer)
// and reach NOTHING of the page around it. The exhibit below tries the
// three things that would matter — this page's DOM, this origin's
// storage, and the API as the reviewer — and writes what happened into
// its own body, where the test reads it back through the frame.
test('an exhibit runs its own script in a sandbox that reaches nothing of the page', async ({
  page,
}) => {
  const errs: string[] = [];
  const apiFromExhibit: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));

  const PROBE =
    '<p id="r">pending</p>' +
    '<script>' +
    'var out = [];' +
    'try { out.push(parent.document.title !== undefined ? "parent:reachable" : "parent:none"); }' +
    ' catch (e) { out.push("parent:blocked"); }' +
    'try { localStorage.getItem("x"); out.push("storage:reachable"); }' +
    ' catch (e) { out.push("storage:blocked"); }' +
    'fetch("/api/jobs/exhibit-probe").then(' +
    ' function () { out.push("fetch:reachable"); },' +
    ' function () { out.push("fetch:blocked"); }' +
    ').then(function () { document.getElementById("r").textContent = out.join(" "); });' +
    '</script>';
  const step = {
    ...STEP,
    metadata: {
      ...STEP.metadata,
      questions: [{ ...STEP.metadata.questions[0], exhibits: ['E1'] }],
      exhibits: [{ anchor: 'E1', title: 'The isolation probe', html: PROBE }],
    },
  };

  await installSmokeMocks(page);
  await page.route('**/api/jobs/exhibit-probe', (r) => {
    apiFromExhibit.push(r.request().url());
    return r.fulfill({ json: { reached: true } });
  });
  await page.route('**/api/jobs/job-dd-1', (r) => r.fulfill({ json: { ...JOB, steps: [step] } }));
  await page.route('**/api/jobs/step-plugins', (r) =>
    r.fulfill({
      json: [
        {
          kind: 'review-design',
          label: 'Review Design',
          category: 'platform',
          version: 1,
          frontend_url: '/plugins/review-design.js',
          owning_team: 'platform',
        },
      ],
    }),
  );
  await page.route('**/plugins/review-design.js', (r) =>
    r.fulfill({ contentType: 'application/javascript', body: PLUGIN }),
  );

  await mountPage(page, '/jobs/job-dd-1/steps/step-1', { root: '.step-focus' });

  const frame = page.frameLocator('iframe[title^="Exhibit E1"]');
  // Its own script RAN (allow-scripts), and each reach was refused.
  await expect(frame.locator('#r')).toHaveText('parent:blocked storage:blocked fetch:blocked', {
    timeout: 10_000,
  });
  expect(apiFromExhibit, 'the exhibit reached the API').toEqual([]);
  // The bound question names its exhibit beside it.
  await expect(page.getByRole('button', { name: 'E1' })).toBeVisible();
  expect(errs, `the surface threw: ${errs.join(' | ')}`).toEqual([]);
});
