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

// One second of settling: the unbounded JobsListPage loop managed 554
// reads in it, so a bounded page's count is unambiguous.
const SETTLE_MS = 1_000;

/**
 * Wait until `expected` reads have ARRIVED, then give a loop the
 * settling window to show itself, then take the exact count.
 *
 * A window that starts at mount bounds two things at once: how long a
 * loop gets to climb, and how long the FIRST read may take to reach
 * the route handler. Only the first is a spec's claim. On a loaded gate
 * runner the second lost — Expected 1, Received 0 on a correct page, in
 * mount-refetch-audit (backlog 28a60028, 2026-09-20) and then in
 * jobs-kinds-failed-read, which carried the same fixed-wait shape
 * (backlog 3571be7f). A count BELOW the claim is a late read, not a
 * fixed page, and asserting a bound instead (at most one) would pass a
 * page that stopped reading at all — so the count stays exact and the
 * wait is for the condition, under the suite's stated expect budget
 * (playwright.mocked.config.ts), before the window opens. One
 * definition, so the next spec that counts reads takes this rather
 * than a copy of the old shape.
 */
export async function settledReads(
  page: Page,
  reads: () => number,
  expected: number,
): Promise<number> {
  await expect
    .poll(reads, { message: `waiting for the ${expected} mount-time read(s) to arrive` })
    .toBeGreaterThanOrEqual(expected);
  await page.waitForTimeout(SETTLE_MS);
  return reads();
}

/**
 * Mount a page and wait for the AppShell + the page-level h1 to
 * render. Returns once the SPA's first paint has settled, so
 * subsequent role lookups don't race against hydration.
 *
 * Both waits run under the suite's STATED expect budget
 * (playwright.mocked.config.ts), never a number of their own. They
 * carried 10 000 ms each until backlog e614c5de: a page whose h1 paints
 * only once its first read answers (the workflow authoring workspace)
 * missed that cap under gate load on 2026-09-24 — "h1 … element(s) not
 * found, Timeout: 10000ms" on gate 2ab44d1d, green on a re-gate of the
 * same head — while the budget the suite had declared for exactly that
 * load was 15 000. Every spec mounts through here, so a cap tighter than
 * the budget here is one tighter than the budget everywhere.
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
  await expect(page.locator(opts.root ?? '.app-shell')).toBeVisible();
  if (opts.titleMatch) {
    await expect(page.locator('h1').first()).toContainText(opts.titleMatch);
  }
}
