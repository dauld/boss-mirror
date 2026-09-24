// /ux/finance PO Approvals — a refused Approve says so, beside its row
// (backlog 1acaa10e, page audit 3f964c57 gap 11).
//
// ApprovalsTab stored `error: <status>` on a failed PUT
// /api/inventory/orders/{id}/status, but its template rendered only
// 'Approved' or the Approve button, so a 403, 409 or 500 looked exactly
// like a click that did nothing — the refusal was recorded and never
// shown. The refusal now renders beside the row with the failure marker
// as an alert, carrying the status AND the server's reason, and the
// button stays so the operator can try again.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const PATH = '/ux/finance';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const DRAFT_PO = {
  id: 'po-0001',
  vendor: 'vendor-0001',
  status: 'draft',
  placed_on: '2026-09-20',
  expected_on: '2026-09-30',
  lines: [{ part_sku: 'HOPS-CASCADE', qty: 4, unit_cost_cents: 2_500, currency: 'USD' }],
};

/// The Overview tab the page opens on is not under test. Its two reads
/// are refused, which it renders as failure lines — the API floor's `[]`
/// is not their shape, and parsing it as one throws in the console.
const notUnderTest = (r: Route): Promise<void> =>
  r.fulfill({ status: 503, contentType: 'text/plain', body: 'not under test' });

/// The shell, one draft PO, and the Approve PUT answered by `approve`.
async function install(page: Page, approve: (r: Route) => Promise<void>): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/commerce\/summary$/, notUnderTest);
  await page.route(/\/api\/inventory\/ap-aging$/, notUnderTest);
  await page.route(/\/api\/inventory\/orders$/, (r) => json(r, [DRAFT_PO]));
  await page.route(/\/api\/inventory\/orders\/po-0001\/status$/, approve);
  await mountPage(page, PATH);
  await page.getByRole('tab', { name: 'PO Approvals', exact: true }).click();
}

const row = (page: Page) => page.locator('tr', { hasText: 'HOPS-CASCADE x4' });

test('a refused Approve renders the refusal beside its row, as an alert', async ({ page }) => {
  await install(page, (r) =>
    r.fulfill({ status: 409, contentType: 'text/plain', body: 'po-0001 is not a draft' }));
  await row(page).getByRole('button', { name: 'Approve' }).click();

  const refusal = row(page).locator(FAILURE_MARKER);
  await expect(refusal).toBeVisible();
  await expect(refusal).toHaveAttribute('role', 'alert');
  await expect(refusal).toContainText('HTTP 409');
  await expect(refusal).toContainText('po-0001 is not a draft');
  // Still pending, and still approvable: a refusal is not an approval.
  await expect(row(page).getByText('Approved', { exact: true })).toHaveCount(0);
  await expect(row(page).getByRole('button', { name: 'Approve' })).toBeEnabled();
});

test('an Approve that never reaches the server says so too', async ({ page }) => {
  await install(page, (r) => r.abort('connectionrefused'));
  await row(page).getByRole('button', { name: 'Approve' }).click();

  await expect(row(page).locator(FAILURE_MARKER)).toBeVisible();
});

test('an accepted Approve moves the PO to Recently approved with no failure line', async ({ page }) => {
  await install(page, (r) => json(r, { ...DRAFT_PO, status: 'submitted' }));
  await row(page).getByRole('button', { name: 'Approve' }).click();

  await expect(page.getByText('No purchase orders awaiting approval.')).toBeVisible();
  await expect(page.getByText('Recently approved')).toBeVisible();
  await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
});
