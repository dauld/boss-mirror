// A CHOSEN FILTER SURVIVES A RELOAD (backlog f8027805).
//
// parseRoute reads `status` (and `kind`, `subject_id`) off the /jobs
// query, but the page's filters changed only page state: choosing All
// and reloading /ux/jobs came back as Open, and a filtered view could
// not be shared as a link. The filters now replaceState the query the
// router already reads. The pure write is pinned in
// src/jobs/filterQuery.test.ts; this pins the page wiring end to end —
// click, reload, and the page asks for what was chosen.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

/// Every list read's query, newest last — the request the page made
/// for what it shows. Registered after the smoke mocks, so it wins.
async function recordListReads(page: Page): Promise<URLSearchParams[]> {
  await installSmokeMocks(page);
  const reads: URLSearchParams[] = [];
  await page.route(/\/api\/jobs\?/, (r: Route) => {
    const params = new URL(r.request().url()).searchParams;
    if (params.get('limit') === '200') reads.push(params);
    return r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ data: [], total: 0 }),
    });
  });
  return reads;
}

const statusButton = (page: Page, label: string) =>
  page.locator('.catalog-filters').getByRole('button', { name: label, exact: true });

test.describe('the jobs list filters live in the URL', () => {
  test('All survives a reload and asks for every status', async ({ page }) => {
    const reads = await recordListReads(page);
    await mountPage(page, '/ux/jobs');
    await expect(page.locator('.filter-button-active')).toHaveText('Open');

    await statusButton(page, 'All').click();
    await expect.poll(() => new URL(page.url()).searchParams.get('status')).toBe('');

    await page.reload();
    await expect(page.locator('.filter-button-active')).toHaveText('All');
    await expect.poll(() => reads.at(-1)?.has('status')).toBe(false);
  });

  test('Closed survives a reload, and Open again takes the parameter back out', async ({ page }) => {
    const reads = await recordListReads(page);
    await mountPage(page, '/ux/jobs?owner_id=emp-1');

    await statusButton(page, 'Closed').click();
    await expect.poll(() => new URL(page.url()).search).toBe('?owner_id=emp-1&status=closed');

    await page.reload();
    await expect(page.locator('.filter-button-active')).toHaveText('Closed');
    await expect.poll(() => reads.at(-1)?.get('status')).toBe('closed');

    await statusButton(page, 'Open').click();
    await expect.poll(() => new URL(page.url()).search).toBe('?owner_id=emp-1');
  });

  test('mounting from a deep link rewrites nothing', async ({ page }) => {
    await recordListReads(page);
    await mountPage(page, '/ux/jobs?status=open&owner_id=emp-1');
    await expect(page.locator('.filter-button-active')).toHaveText('Open');
    expect(new URL(page.url()).search).toBe('?status=open&owner_id=emp-1');
  });
});
