// Admin · Workflow authoring workspace (D6). The graphical heart we want
// to maintain: the trigger→outcome graph renders from the spec, the
// palette adds steps, the inspector edits the selected node, and the
// workflow rail drives the design Job author → validate → approve →
// publish. Backend fully mocked (see _mockApi.ts) — stateful so the
// rail can advance.
//
// NO WAIT HERE CARRIES A NUMBER OF ITS OWN (backlog e614c5de). The
// workspace paints its h1 and its rail only once the design Job's read
// answers, and every wait in this file used to cap itself at 5 000 or
// 10 000 ms — tighter than the 15 000 the suite states for exactly this
// load (playwright.mocked.config.ts). The rail test redded gate 2ab44d1d
// on 2026-09-24 at the mount ("h1 … element(s) not found, Timeout:
// 10000ms") on a car that did not touch this page, and passed on a
// re-gate of the same head. Every wait now inherits the stated budget.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';
import { installAuthoringMocks, JOB_ID, KIND_SLUG } from './_mockApi';

const WORKSPACE = `/it/registry/authoring/${JOB_ID}`;

test.beforeEach(async ({ page }) => {
  await installAuthoringMocks(page);
});

test.describe('Workflow authoring workspace — graph + inspector', () => {
  test('loads the design Job and renders the trigger→outcome graph', async ({ page }) => {
    await mountPage(page, WORKSPACE, { titleMatch: /Authoring/i });

    // Spec fields are seeded from the publish step's workflow_spec.
    // Located by placeholder, not by index: this used to be
    // `locator('input').nth(1)` with a comment pinning the DOM order
    // (slug · label · category), which silently counted every input
    // on the page — including the chrome bar's. Adding global search
    // to the chrome shifted the index by one and broke it.
    await expect(page.getByPlaceholder('Warranty Rework')).toHaveValue('Seasonal Release');

    // The graph (lazy-loaded Svelte Flow) renders the two seeded steps.
    await expect(page.locator('.jk-node')).toHaveCount(2);
    await expect(page.locator('.jk-trigger')).toHaveCount(1); // start (ready_when = true)
    await expect(page.locator('.jk-outcome')).toHaveCount(1); // finish (terminal)
  });

  test('palette adds a step to the canvas + opens the inspector', async ({ page }) => {
    await mountPage(page, WORKSPACE, { titleMatch: /Authoring/i });
    await expect(page.locator('.jk-node')).toHaveCount(2);

    // Add a step via the palette → a third node appears, and the new
    // node is selected (the inspector opens).
    await page.locator('.jk-chip').first().click();
    await expect(page.locator('.jk-node')).toHaveCount(3);
    await expect(page.locator('.jk-inspector')).toBeVisible();
  });

  test('selecting a node opens the inspector for that step', async ({ page }) => {
    await mountPage(page, WORKSPACE, { titleMatch: /Authoring/i });
    await expect(page.locator('.jk-node')).toHaveCount(2);

    await page.locator('.jk-node').first().click();
    const inspector = page.locator('.jk-inspector');
    await expect(inspector).toBeVisible();
    // The inspector's slug field carries the selected step's slug.
    await expect(inspector.locator('input.mono').first()).not.toHaveValue('');
  });
});

test.describe('Workflow authoring workspace — workflow rail', () => {
  test('drives author → validate → approve → publish, then routes to the kind', async ({ page }) => {
    await mountPage(page, WORKSPACE, { titleMatch: /Authoring/i });

    const authored = page.getByRole('button', { name: /mark authored/i });
    const validate = page.getByRole('button', { name: /validate & advance/i });
    const approve = page.getByRole('button', { name: /approve/i });
    const publish = page.getByRole('button', { name: /^\s*4 · Publish/i });

    // Only the first gate is actionable initially.
    await expect(authored).toBeEnabled();
    await expect(validate).toBeDisabled();

    await authored.click();
    await expect(validate).toBeEnabled();

    await validate.click();
    await expect(approve).toBeEnabled();

    await approve.click();
    await expect(publish).toBeEnabled();

    // Publishing completes the terminal step and routes to the kind.
    await Promise.all([
      page.waitForURL(new RegExp(`/it/registry/${KIND_SLUG}$`)),
      publish.click(),
    ]);
  });
});
