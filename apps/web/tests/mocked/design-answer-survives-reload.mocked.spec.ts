// A design answer being typed must survive its packet reloading
// (backlog fec57f5f).
//
// David, 2026-09-23: "I am trying to type a somewhat long response into
// a design question, and the text box keeps clearing my entry." The job
// page replaces its packet on every SSE frame and on the 30s fallback
// poll, so the step reaching StepSurface is a NEW object each time. The
// plugin probe read `step.kind` inside its effect, which made the whole
// step object a dependency: every reload reset the probe to "unknown",
// the `{#if}` unmounted the review-design plugin, and the plugin
// re-mounted from the step's SAVED resolutions — so the box went back
// to the last saved text, usually nothing.
//
// Two tests, one per defence. The first is the fix: a same-kind reload
// never unmounts a mounted plugin. The second is the belt-and-braces: a
// GENUINE remount (a page reload, a navigation) restores unsaved text
// from a per-step, per-question sessionStorage draft, and a successful
// save clears it.
//
// Rendered rather than reasoned about — the defect lived in the
// interaction between the host's reactivity and a served bundle, which
// no unit test of either half can see.

import { test, expect, type Page } from '@playwright/test';
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
  status: 'active',
  assignee_id: 'emp-david',
  sort_order: 1,
  blocked_by: [],
  notes: null,
  metadata: {
    title: 'Design: a long answer',
    markdown: '# The claim\n\nAn answer takes minutes to write.',
    questions: [{ anchor: 'Q1', title: 'Which way?', proposal: 'This way.' }],
    resolutions: [],
  },
};

const JOB = {
  id: 'job-dd-1',
  kind: 'design-doc',
  workflow_version: 1,
  subject: { subject_kind: 'custom', id: 'a-long-answer' },
  title: 'Design: a long answer',
  owner_id: 'emp-david',
  status: 'open',
  priority: 'standard',
  opened_on: '2026-09-23',
  due_on: null,
  closed_on: null,
  metadata: {},
  steps: [STEP],
};

const ANSWER = 'A long answer, typed but not yet saved — it must outlive every reload.';

async function routePlugin(page: Page): Promise<void> {
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
}

test('a same-kind reload of the packet keeps the typed answer', async ({ page }) => {
  const errs: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));

  // The draft's writes REFUSED, the way a full quota or blocked site
  // data refuses them: the draft cannot rescue the answer, so this test
  // judges the host fix alone — and proves the bundle's storage calls
  // fail soft rather than throwing. Only the draft's keys: the dev
  // client reads sessionStorage too, and blocking it whole stops the
  // SPA from rendering at all.
  await page.addInitScript(() => {
    const setItem = Storage.prototype.setItem;
    Storage.prototype.setItem = function (key: string, value: string) {
      if (key.startsWith('boss.review-design.')) throw new Error('storage refused');
      return setItem.call(this, key, value);
    };
  });
  await installSmokeMocks(page);
  await routePlugin(page);
  await page.route('**/api/jobs/job-dd-1', (r) => r.fulfill({ json: JOB }));

  // The job's stream, served as real SSE. The first connection delivers
  // the packet; every later one is HELD until the answer is typed, then
  // delivers the same packet again — a fresh JSON parse, so a NEW step
  // object of the same kind, exactly what a live SSE frame is. `retry:
  // 50` makes the browser reconnect fast, so once released the page
  // takes a reload every ~50ms.
  const frame = `retry: 50\ndata: ${JSON.stringify(JOB)}\n\n`;
  let streamHits = 0;
  let release: () => void = () => {};
  const released = new Promise<void>((r) => (release = r));
  await page.route('**/api/jobs/job-dd-1/stream', async (r) => {
    streamHits += 1;
    if (streamHits > 1) await released;
    try {
      await r.fulfill({
        status: 200,
        headers: { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' },
        body: frame,
      });
    } catch {
      // The page closed while this connection was held.
    }
  });

  await mountPage(page, '/jobs/job-dd-1');
  const box = page.locator('.step-review-design textarea').first();
  await expect(box).toBeVisible();
  await box.fill(ANSWER);

  const before = streamHits;
  release();
  // Several reloads land — each one a new step object of the same kind.
  await expect.poll(() => streamHits).toBeGreaterThan(before + 3);

  await expect(page.locator('.step-review-design textarea').first()).toHaveValue(ANSWER);
  expect(errs, `page threw: ${errs.join(' | ')}`).toEqual([]);
});

test('a genuine remount restores the unsaved answer, and a save clears the draft', async ({
  page,
}) => {
  const errs: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));

  await installSmokeMocks(page);
  await routePlugin(page);
  await page.route('**/api/jobs/job-dd-1', (r) => r.fulfill({ json: JOB }));
  const saved: unknown[] = [];
  await page.route('**/api/jobs/job-dd-1/steps/step-1/metadata', (r) => {
    saved.push(r.request().postDataJSON());
    return r.fulfill({ status: 204 });
  });

  // The full-page step route mounts the plugin directly.
  await mountPage(page, '/jobs/job-dd-1/steps/step-1', { root: '.step-focus' });
  const box = page.locator('.step-review-design textarea').first();
  await expect(box).toBeVisible();
  await box.fill(ANSWER);

  // A real remount: the whole page reloads, the plugin mounts again
  // from the step's saved metadata — which holds no resolution.
  await page.reload();
  const again = page.locator('.step-review-design textarea').first();
  await expect(again).toHaveValue(ANSWER);

  // Saving lands the answer on the step, and the draft is then spent.
  await page.getByRole('button', { name: 'Save progress' }).click();
  await expect.poll(() => saved.length).toBe(1);
  expect(JSON.stringify(saved[0])).toContain(ANSWER);
  await expect
    .poll(() =>
      page.evaluate(() =>
        Object.keys(window.sessionStorage).filter((k) => k.includes('review-design')),
      ),
    )
    .toEqual([]);
  expect(errs, `page threw: ${errs.join(' | ')}`).toEqual([]);
});

// The other side of the fix. StepSurface's probe was the only thing
// that remounted a plugin for a DIFFERENT step, so with it keyed on the
// kind alone, picking a second step of the same kind on the rail would
// have left the first step's plugin on screen. StepPluginMount now
// remounts on the step's id (and status), so the surface follows the
// pick while a same-step reload still leaves it alone.
test('picking a different step of the same kind shows that step', async ({ page }) => {
  const errs: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));

  const second = {
    ...STEP,
    id: 'step-2',
    title: 'second review',
    status: 'ready',
    sort_order: 2,
    metadata: {
      ...STEP.metadata,
      questions: [{ anchor: 'Q1', title: 'The other question?', proposal: 'That way.' }],
    },
  };
  await installSmokeMocks(page);
  await routePlugin(page);
  await page.route('**/api/jobs/job-dd-1', (r) =>
    r.fulfill({ json: { ...JOB, steps: [STEP, second] } }),
  );

  await mountPage(page, '/jobs/job-dd-1');
  await expect(page.getByText('Which way?')).toBeVisible();

  await page.locator('.rail-row', { hasText: 'second review' }).click();
  await expect(page.getByText('The other question?')).toBeVisible();
  await expect(page.getByText('Which way?')).toHaveCount(0);
  expect(errs, `page threw: ${errs.join(' | ')}`).toEqual([]);
});
