// The scope step of ship-a-change, rendered by its own plugin.
//
// David, on one of these: "I don't know how to input my decision within
// this UX / I think the wrong step UX is showing." The step is a bare
// `task` with two required string fields, so the generic surface gave
// him two unlabelled textareas named `summary` and `excludes` and no
// account of what either one is for.
//
// Rendered rather than reasoned about, for the reason
// design-doc-packet.mocked.spec.ts gives: a change to plugin JS is
// otherwise verified by a human opening the page. These mount the real
// bundle from infra/step-plugins/ and drive it.

import { test, expect } from '@playwright/test';
import { readFileSync } from 'fs';
import { mountPage } from '../smoke/_helpers';
import { installSmokeMocks } from './_smokeMocks';

const PLUGIN = readFileSync(
  new URL('../../../../infra/step-plugins/scope-declaration.js', import.meta.url),
  'utf8',
);

const SPEC = [
  {
    kind: 'scope-declaration',
    label: 'Scope declaration',
    category: 'platform',
    version: 1,
    frontend_url: '/plugins/scope-declaration.js',
    owning_team: 'platform',
  },
];

const SCOPE_STEP = {
  id: 'step-scope',
  spec_slug: 'scope',
  title: 'scope',
  kind: 'scope-declaration',
  status: 'ready',
  assignee_id: 'emp-001',
  blocked_by: [],
  metadata: {},
};

/// A gate step that has already run — the case where the boundary is
/// being declared or edited after the work.
const GATE_STEP = {
  id: 'step-gate',
  spec_slug: 'gate',
  title: 'gate',
  kind: 'task',
  status: 'completed',
  assignee_id: 'emp-001',
  blocked_by: [],
  metadata: {
    gates: 'infra/gate.sh --auto, 28 of 28',
    verified: 'Opened /ux/marketing-assets and read the tags.',
    receipt: '{"verdict":"passed","mode":"auto","commit":"a794446d"}',
  },
};

const JOB = {
  id: 'job-car-1',
  kind: 'ship-a-change',
  workflow_version: 1,
  // The Subject id IS the branch for a ship-a-change Job.
  subject: { subject_kind: 'custom', id: 'fix/one-palette-ours' },
  title: 'Commit the app to one palette',
  owner_id: 'emp-001',
  status: 'open',
  priority: 'standard',
  opened_on: '2026-08-23',
  due_on: null,
  closed_on: null,
  metadata: { backlog_item: 'fb-9f21' },
  steps: [SCOPE_STEP, GATE_STEP],
};

async function mountScope(page: import('@playwright/test').Page, job: unknown) {
  await installSmokeMocks(page);
  await page.route('**/api/jobs/job-car-1', (r) => r.fulfill({ json: job }));
  await page.route('**/api/jobs/step-plugins', (r) => r.fulfill({ json: SPEC }));
  await page.route('**/plugins/scope-declaration.js', (r) =>
    r.fulfill({ contentType: 'application/javascript', body: PLUGIN }),
  );
  await mountPage(page, '/jobs/job-car-1/steps/step-scope', { root: '.step-focus' });
  await page.waitForTimeout(1500);
}

test('the scope step asks its two questions in words, and names the car it is scoping', async ({
  page,
}) => {
  const errs: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));

  await mountScope(page, JOB);
  expect(errs, `plugin threw: ${errs.join(' | ')}`).toEqual([]);

  // The two halves, as questions rather than as field names.
  await expect(page.getByText('What this car DOES')).toBeVisible();
  await expect(page.getByText('What it deliberately does NOT do')).toBeVisible();
  // And WHY the second one is required — the part the generic surface
  // could never say.
  await expect(page.getByText(/keeps a change small/)).toBeVisible();

  // Context the surface fetched rather than made the author remember.
  await expect(page.getByText('fix/one-palette-ours')).toBeVisible();
  await expect(page.getByRole('link', { name: 'fb-9f21' })).toBeVisible();

  // The gate has already run on this branch, so its receipt is here —
  // collapsed, because it is a check on the claim, not the question.
  await expect(page.getByText(/The gate has already run/)).toBeVisible();
  await expect(page.getByText(/"verdict":"passed"/)).toBeHidden();
  await page.getByText(/The gate has already run/).click();
  await expect(page.getByText(/"verdict":"passed"/)).toBeVisible();
});

test('the boundary cannot be declared with half of it missing', async ({ page }) => {
  await mountScope(page, JOB);

  const declare = page.getByRole('button', { name: 'Declare the boundary' });
  await expect(declare).toBeDisabled();
  await expect(page.getByText('Both halves are required')).toBeVisible();

  // One half is not enough — `excludes` is the field the step exists for.
  await page.locator('#ssd-summary').fill('Fix the marketing-asset tag chips.');
  await expect(declare).toBeDisabled();

  await page.locator('#ssd-excludes').fill('Not sweeping the other pages — a sibling car owns them.');
  await expect(declare).toBeEnabled();
});

test('declaring the boundary writes both fields and completes the step', async ({ page }) => {
  let patch: Record<string, unknown> | null = null;
  let put: Record<string, unknown> | null = null;

  await installSmokeMocks(page);
  await page.route('**/api/jobs/job-car-1', (r) => r.fulfill({ json: JOB }));
  await page.route('**/api/jobs/step-plugins', (r) => r.fulfill({ json: SPEC }));
  await page.route('**/plugins/scope-declaration.js', (r) =>
    r.fulfill({ contentType: 'application/javascript', body: PLUGIN }),
  );
  // The declaration rides the metadata PATCH (only the keys this
  // surface owns; the server merges and answers 204 with no body)…
  await page.route('**/api/jobs/job-car-1/steps/step-scope/metadata', (r) => {
    patch = r.request().postDataJSON() as Record<string, unknown>;
    return r.fulfill({ status: 204, body: '' });
  });
  // …then the plugin reads the row back and completes with THAT — so
  // this mock serves back exactly what the PATCH landed, and the
  // completion assertions below attest the true merged shape.
  await page.route('**/api/jobs/job-car-1/steps', (r) =>
    r.fulfill({ json: [{ ...SCOPE_STEP, metadata: patch ?? {} }] }),
  );
  await page.route('**/api/jobs/job-car-1/steps/step-scope', (r) => {
    put = r.request().postDataJSON() as Record<string, unknown>;
    return r.fulfill({ json: { ...SCOPE_STEP, status: 'completed' } });
  });

  await mountPage(page, '/jobs/job-car-1/steps/step-scope', { root: '.step-focus' });
  await page.waitForTimeout(1500);

  await page.locator('#ssd-summary').fill('Fix the marketing-asset tag chips.');
  await page.locator('#ssd-excludes').fill('  Not the other pages — a sibling car owns them.  ');
  await page.getByRole('button', { name: 'Declare the boundary' }).click();
  await expect.poll(() => put).not.toBeNull();

  const merged = patch as unknown as Record<string, string>;
  expect(merged['summary']).toBe('Fix the marketing-asset tag chips.');
  // Trimmed: the stored declaration is the sentence, not the whitespace
  // around it.
  expect(merged['excludes']).toBe('Not the other pages — a sibling car owns them.');

  const body = put as unknown as { status: string; metadata: Record<string, string> };
  expect(body.status).toBe('completed');
  // The completion carried the read-back row, not the snapshot.
  expect(body.metadata['summary']).toBe('Fix the marketing-asset tag chips.');
  expect(body.metadata['excludes']).toBe('Not the other pages — a sibling car owns them.');
});

test('a declared boundary reads back as the two halves it was asked in', async ({ page }) => {
  await mountScope(page, {
    ...JOB,
    steps: [
      {
        ...SCOPE_STEP,
        status: 'completed',
        metadata: {
          summary: 'Fix the marketing-asset tag chips.',
          excludes: 'Not the other pages — a sibling car owns them.',
        },
      },
      GATE_STEP,
    ],
  });

  await expect(page.getByText('This car does')).toBeVisible();
  await expect(page.getByText('It deliberately does not')).toBeVisible();
  await expect(page.getByText('Fix the marketing-asset tag chips.')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Declare the boundary' })).toHaveCount(0);
});
