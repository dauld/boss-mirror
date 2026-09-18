// The mocked suite's shared mount helper.
//
// Lived under tests/smoke until 2026-09-18, beside the live-backend
// suite that directory held — 35 specs no CI job, gate check or chore
// ran since 2026-06-18, deleted with design 0e07ce64 (the nightly
// playground crawl under tests/live is what replaced them). This
// helper was the one file that stayed, because twelve mocked specs
// imported `mountPage` from `../smoke/_helpers`; it moved here with
// them (backlog ac3270c7), and tests/smoke is gone —
// crates/core/boss-testing/tests/the_playground_is_crawled_nightly.rs
// pins that it stays gone.
//
// The four helpers only the deleted specs used (pinPersona,
// clickButton, clickAndExpectNavigation, expectTableRow) went with them.

import type { Page } from '@playwright/test';
import { expect } from '@playwright/test';

/**
 * Mount a page and wait for the AppShell + the page-level h1 to
 * render. Returns once the SPA's first paint has settled, so
 * subsequent role lookups don't race against hydration.
 */
export async function mountPage(
  page: Page,
  path: string,
  opts: { titleMatch?: RegExp; root?: string } = {},
): Promise<void> {
  await page.goto(path);
  // AppShell renders for every authed route — except the handful that
  // deliberately render outside it to take the whole viewport (login,
  // the full-page step surface). Those pass their own root; waiting
  // for `.app-shell` there fails on a page that is working correctly.
  await expect(page.locator(opts.root ?? '.app-shell')).toBeVisible({ timeout: 10_000 });
  if (opts.titleMatch) {
    await expect(page.locator('h1').first()).toContainText(opts.titleMatch, {
      timeout: 10_000,
    });
  }
}
