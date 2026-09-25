// A SUBJECT LINK LANDS ON THE SUBJECT, NEVER ON HOME (backlog 4af37dd8).
//
// subjectPath answered `'#'` for a `custom` subject and for every kind
// it did not name; `href('#')` is `/#`, which parses as `/`, so a click
// on the Subject cell took the operator to Home. Every open packet on
// the live instance is `custom` (286 of 286 at page audit 473f4f92 GAP
// 2), so every Subject cell on /ux/jobs linked somewhere else — and on
// /ux/service and /ux/sales, which mount the same list. The interaction
// crawl counted `/#` as served, so nothing caught it. A custom subject
// now lands on its own packets, as a campaign already did.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const job = (id: string, subjectId: string): Record<string, unknown> => ({
  id,
  kind: 'page-audit',
  subject: { subject_kind: 'custom', id: subjectId },
  title: `Audit ${subjectId}`,
  owner_id: 'emp-1',
  status: 'open',
  priority: 'standard',
  opened_on: '2026-09-23',
  due_on: null,
  closed_on: null,
  metadata: {},
  tags: [],
});

async function openList(page: Page, route: string): Promise<void> {
  await installSmokeMocks(page);
  // Registered after the smoke mocks, so it wins for the list read.
  await page.route(/\/api\/jobs\?/, (r: Route) =>
    r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        data: [
          job('11111111-1111-4111-8111-111111111111', '/ux/jobs'),
          job('22222222-2222-4222-8222-222222222222', 'estate'),
        ],
        total: 2,
      }),
    }),
  );
  await mountPage(page, route);
  await expect(page.locator('table.data-table tbody tr')).toHaveCount(2);
}

for (const route of ['/ux/jobs', '/ux/service', '/ux/sales']) {
  test.describe(`${route} — a custom Subject links to its own packets`, () => {
    test('the Subject cell names the subject_id filter', async ({ page }) => {
      await openList(page, route);
      await expect(page.getByRole('link', { name: '/ux/jobs', exact: true })).toHaveAttribute(
        'href',
        '/ux/jobs?subject_id=%2Fux%2Fjobs',
      );
      await expect(page.getByRole('link', { name: 'estate', exact: true })).toHaveAttribute(
        'href',
        '/ux/jobs?subject_id=estate',
      );
    });

    test('no link in the table resolves to Home', async ({ page }) => {
      await openList(page, route);
      const hrefs = await page
        .locator('table.data-table a')
        .evaluateAll((as) => as.map((a) => (a as HTMLAnchorElement).getAttribute('href') ?? ''));
      expect(hrefs.length).toBeGreaterThanOrEqual(4);
      for (const h of hrefs) {
        expect(h, `a row link reads ${JSON.stringify(h)}`).not.toMatch(/^\/?#/);
        expect(new URL(h, 'http://x').pathname).not.toBe('/');
      }
    });

    test('a click on the Subject lands on its packets, not on Home', async ({ page }) => {
      await openList(page, route);
      await page.getByRole('link', { name: 'estate', exact: true }).click();
      await expect.poll(() => new URL(page.url()).searchParams.get('subject_id')).toBe('estate');
      expect(new URL(page.url()).pathname).not.toBe('/');
    });
  });
}
