// The chrome bar is the one piece of furniture every app shares, and
// it was rendered from three call sites that repeated their props by
// hand. They drifted twice: the step-focus bar shipped without
// `searchAppKinds`, so global search lost its app scoping on exactly
// the surface built for focused reading, and the Simulator's bar
// carried a hardcoded brand that a second tenant would have rendered
// as someone else's name.
//
// These pin the property the user actually notices: the same bar,
// with the same tenant identity, on every surface — including the two
// that render outside AppShell.

import { test, expect } from '@playwright/test';
import { mountPage } from '../smoke/_helpers';
import { AA_FLOOR, describeUnreadable, measureContrast } from './_contrast';

/// Surfaces that render the chrome through different code paths:
/// a normal AppShell route, the full-page step route (rendered
/// OUTSIDE AppShell), and an IT surface.
const SURFACES = ['/ux/jobs', '/it', '/ux/views'] as const;

const MANIFEST = {
  display_name: 'Algedonic Ales',
  tenant_id: 'brewery',
  modules: {},
  labels: {},
};

test.describe('chrome bar', () => {
  test.beforeEach(async ({ page }) => {
    // `mountPage` does not install the smoke-mock backend, so the
    // manifest is mocked here — the brand comes from it now.
    await page.route(/\/api\/tenant\/manifest$/, (r) => r.fulfill({ json: MANIFEST }));
  });

  for (const path of SURFACES) {
    test(`shows the tenant's own name on ${path}`, async ({ page }) => {
      await mountPage(page, path);
      // The brand is split into wordmark + suffix, so assert on the
      // bar's text rather than a single node.
      const bar = page.locator('.perspective-tabs').first();
      await expect(bar).toContainText('Algedonic');
      await expect(bar).toContainText('Ales');
    });
  }

  // The bar is an unconditionally dark surface that sets no `color`,
  // so every control inside it has to declare its own. One used
  // `color: inherit` and so rendered in the DOCUMENT text colour —
  // invisible in light theme, fine in dark, which is why it survived
  // review. Reported from the field as "I can't read this feedback
  // button".
  //
  // Contrast is computable, so this checks the rendered result rather
  // than the stylesheet: whatever colour each control ends up with,
  // against whatever surface it actually sits on.
  test('every chrome control stays legible in light theme', async ({ page }) => {
    // Light is the theme it broke in: `inherit` resolves dark there.
    await page.emulateMedia({ colorScheme: 'light' });
    await mountPage(page, '/ux/jobs');

    const measured = await measureContrast(
      page.locator('.perspective-tabs').first(),
      'button, a',
    );

    expect(measured.length, 'no labelled controls found in the chrome bar').toBeGreaterThan(3);

    const unreadable = measured.filter((m) => m.ratio < AA_FLOOR);
    expect(
      unreadable,
      `chrome controls below ${AA_FLOOR}:1 contrast in light theme:\n${describeUnreadable(
        unreadable,
      )}`,
    ).toEqual([]);
  });

  test('offers the same app tabs everywhere', async ({ page }) => {
    // The bar must not change shape as you navigate. Apps are
    // departments now, so the full list is as long as the org chart —
    // the bar pins Home, Simulator and YOUR department, and folds the
    // rest into More.
    //
    // The count assertion is the point: an earlier version of that
    // design also pinned whichever app you were currently in, which
    // made the set grow by one whenever you left your own department.
    // That is a second, drifted bar by another name, and this caught
    // it.
    const counts: number[] = [];
    for (const path of SURFACES) {
      await mountPage(page, path);
      counts.push(await page.locator('.perspective-tabs a[href]').count());
    }
    expect(new Set(counts).size, `tab counts differed across surfaces: ${counts}`).toBe(
      1,
    );
    // Home + Simulator at minimum; a signed-in operator also gets
    // their own department. This used to assert `> 4`, which encoded
    // the old seven-invented-app bar rather than anything true.
    expect(counts[0]).toBeGreaterThanOrEqual(2);
  });

  test('folds the other departments into More rather than dropping them', async ({
    page,
  }) => {
    // The apps not on the bar must still be reachable in one click —
    // "very few people need most of the Apps" is a reason to demote
    // them, never a reason to hide them.
    await mountPage(page, '/ux/jobs');
    const more = page.locator('.perspective-more-btn');
    await expect(more).toBeVisible();
    await more.click();
    const items = page.locator('.perspective-more-item');
    await expect(items.first()).toBeVisible();
    // Every department app that is not pinned shows up here.
    expect(await items.count()).toBeGreaterThan(4);
  });

  test('falls back to BOSS when the tenant has not named itself', async ({ page }) => {
    // A deployment with no [meta] in tenant.toml should read "BOSS",
    // not blank and not a brewery's name.
    await page.route(/\/api\/tenant\/manifest$/, (r) =>
      r.fulfill({ json: { modules: {}, labels: {} } }),
    );
    await mountPage(page, '/ux/jobs');
    const bar = page.locator('.perspective-tabs').first();
    await expect(bar).toContainText('BOSS');
    await expect(bar).not.toContainText('Algedonic');
  });
});
