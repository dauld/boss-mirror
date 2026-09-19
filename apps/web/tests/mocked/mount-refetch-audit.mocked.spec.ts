// A MOUNT-TIME READ THAT FAILS IS READ ONCE (backlog ed3392b4).
//
// JobsListPage re-fetched /api/workflows without bound when the read
// failed: its guard — `kinds.length > 0 || kindsLoading` — was read
// INSIDE the mount $effect, so both were effect dependencies, and a
// failure that reset kindsLoading re-triggered the effect forever.
// Measured on a 503 mock: 554 reads of /api/workflows from ONE mount
// of /ux/jobs in one second (backlog 06038ed8).
//
// That car fixed one page and audited no other. These are the three
// pages the audit named as suspects. Each is measured here rather
// than reasoned about: a 503 on the read each one makes at mount,
// then a second of settling, then the count. None of them carries the
// shape today — FleetPage and SubjectsClassesPage load from onMount
// (which runs once and tracks nothing), and SystemModelLiveView's two
// $effects read only `selectedKind` and `spec`, neither of which a
// failed read writes. This spec is what makes that an artifact instead
// of a reading: if any of them is later rewritten onto a mount $effect
// whose guard is its own loading flag, the count moves and the check
// names the page.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

/// Fail every request to `pattern` with a 503, counting them. Registered
/// AFTER the smoke mocks so it wins — Playwright matches handlers in
/// reverse registration order.
async function failAndCount(page: Page, pattern: RegExp): Promise<() => number> {
  let reads = 0;
  await page.route(pattern, (r: Route) => {
    reads += 1;
    return r.fulfill({
      status: 503,
      contentType: 'application/json',
      body: '{"error":"backend down"}',
    });
  });
  return () => reads;
}

// One second is the same settling window the JobsListPage spec uses:
// the unbounded loop managed 554 reads in it, so a bounded page's
// count is unambiguous.
const SETTLE_MS = 1_000;

test.describe('a failed mount-time read is not retried without bound', () => {
  test('bottlenecks reads the Workflow registry once', async ({ page }) => {
    await installSmokeMocks(page);
    const reads = await failAndCount(page, /\/api\/workflows$/);

    await mountPage(page, '/it/operate/bottlenecks');
    await page.waitForTimeout(SETTLE_MS);

    // onMount, once. The page's 10s poll re-reads the fleet view, not
    // the registry, so nothing here should climb.
    expect(reads()).toBe(1);
  });

  test('subjects and classes reads the SubjectKind taxonomy once', async ({ page }) => {
    await installSmokeMocks(page);
    const reads = await failAndCount(page, /\/api\/subject-kinds$/);

    await mountPage(page, '/it/registry/subjects');
    await page.waitForTimeout(SETTLE_MS);

    expect(reads()).toBe(1);
  });

  test('the live system-model view reads the Workflow registry once', async ({ page }) => {
    await installSmokeMocks(page);
    const reads = await failAndCount(page, /\/api\/workflows$/);

    // The landing page is the router's catch-all, so an unrouted /ux
    // path is how a mocked mount reaches SystemModelLiveView.
    await mountPage(page, '/ux/not-a-route');
    await page.waitForTimeout(SETTLE_MS);

    // A failed registry read leaves `selectedKind` empty, which is
    // what both of this component's $effects key off — so neither
    // re-runs, and the 1s live poll reads /api/jobs/live, not this.
    expect(reads()).toBe(1);
  });

  test('the live system-model view reads a failing Workflow spec a bounded number of times', async ({
    page,
  }) => {
    await installSmokeMocks(page);
    // The registry answers, so a kind IS selected — this is the one
    // $effect among the three pages that drives a fetch. It reads
    // `spec`, which a failed read never writes, so the effect does not
    // re-run; onMount's own loadSpec call is the second read.
    const reads = await failAndCount(page, /\/api\/workflows\/[^/?]+$/);

    await mountPage(page, '/ux/not-a-route');
    await page.waitForTimeout(SETTLE_MS);

    expect(reads()).toBe(2);
  });
});
