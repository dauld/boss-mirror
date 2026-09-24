// /ux/vendors/{id} — naming the employees on a vendor's page.
//
// Backlog 1e73bd93 (the last car). The vendor page read the WHOLE
// employee roster beside its vendor, orders and invoices reads to name
// the handful of employees it shows — the account team, the contract
// signers, the people who logged interactions. Since backlog 223ebcd6
// it said so when that read failed; it now reads one row per PERSON
// shown through the shared reader (src/data/ownerNames.ts) — never a
// machine actor, which has no people row — and the same line names the
// row that failed.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const VID = 'vnd-hops-001';
const PATH = `/ux/vendors/${VID}`;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const VENDOR = {
  id: VID, name: 'Cascade Hop Farm', contact_name: 'Rhea Okafor', contact_email: null,
  city: 'Yakima', state: 'WA', lead_time_days: 14, payment_terms: 'Net 30', category: 'hop-supplier',
};

const TEAM = [
  { id: 'at-1', vendor_id: VID, employee_id: 'emp-buyer-1', role: 'primary', assigned_on: '2026-01-01', notes: null, created_at: '2026-01-01' },
];

const CONTRACTS = [
  {
    id: 'ct-1', vendor_id: VID, kind: 'master-supply', title: 'Hop supply 2026', effective_on: '2026-01-01',
    expires_on: null, auto_renew: true, terms: {}, document_uri: null, status: 'active',
    signed_by_employee_id: 'emp-cfo-2', signed_at: '2026-01-01', notes: null,
    created_at: '2026-01-01', updated_at: '2026-01-01',
  },
];

/// One interaction by a person, one by a dispatch rule. The rule must
/// never be looked up — it has no people row.
const interaction = (id: string, actor_id: string, body: string) => ({
  id, vendor_id: VID, vendor_contact_id: null, actor_id, kind: 'call', body, commitments: [],
  linked_po_id: null, linked_part_sku: null, linked_job_id: null,
  occurred_at: '2026-09-20T12:00:00Z', created_at: '2026-09-20T12:00:00Z',
});
const INTERACTIONS = [
  interaction('i-1', 'emp-buyer-1', 'Asked about the Citra allocation'),
  interaction('i-2', 'automation:rule:po-follow-up', 'Chased the late shipment'),
];

/// Every read the page makes, answered; `signer` decides the one under
/// test. The ROSTER names both people differently, so a page that still
/// reads the whole roster shows the wrong names and fails the first
/// test rather than passing it by accident.
async function install(page: Page, signer: (r: Route) => Promise<void>): Promise<string[]> {
  await installSmokeMocks(page);
  await page.route(/\/api\/inventory\/vendors$/, (r) => json(r, [VENDOR]));
  await page.route(/\/api\/inventory\/orders$/, (r) => json(r, []));
  await page.route(/\/api\/inventory\/vendor-invoices$/, (r) => json(r, []));
  await page.route(new RegExp(`/api/inventory/vendors/${VID}/contacts$`), (r) => json(r, []));
  await page.route(new RegExp(`/api/inventory/vendors/${VID}/account-team$`), (r) => json(r, TEAM));
  await page.route(new RegExp(`/api/inventory/vendors/${VID}/contracts$`), (r) => json(r, CONTRACTS));
  await page.route(new RegExp(`/api/inventory/vendors/${VID}/interactions$`), (r) => json(r, INTERACTIONS));
  await page.route(/\/api\/people$/, (r) =>
    json(r, [{ id: 'emp-buyer-1', name: 'Roster Buyer' }, { id: 'emp-cfo-2', name: 'Roster Signer' }]),
  );
  await page.route(/\/api\/people\/emp-buyer-1$/, (r) => json(r, { id: 'emp-buyer-1', name: 'Ben Lusk' }));
  await page.route(/\/api\/people\/emp-cfo-2$/, signer);
  // One-row reads only: the shell's session loader reads the roster for
  // itself, so a bare /api/people here is not the page's read. Any
  // one-row read counts, so a machine actor asked about would show.
  const asked: string[] = [];
  page.on('request', (req) => {
    const path = decodeURIComponent(new URL(req.url()).pathname);
    if (/^\/api\/people\/[^/]+$/.test(path)) asked.push(path);
  });
  return asked;
}

const section = (page: Page, title: RegExp) =>
  page.locator('section', { has: page.getByRole('heading', { name: title }) });

test.describe('/ux/vendors/{id} — naming the employees shown', () => {
  test('answered, each employee is named from their own row, a rule is never looked up, and nothing says a read failed', async ({ page }) => {
    const asked = await install(page, (r) => json(r, { id: 'emp-cfo-2', name: 'Iris Vale' }));
    await mountPage(page, PATH);

    await expect(section(page, /^Account team/).getByRole('link', { name: 'Ben Lusk' })).toBeVisible();
    await expect(section(page, /^Active contracts/).getByRole('link', { name: 'Iris Vale' })).toBeVisible();
    await expect(section(page, /^Interactions/)).toContainText('by Ben Lusk');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    expect([...new Set(asked)].sort()).toEqual(['/api/people/emp-buyer-1', '/api/people/emp-cfo-2']);
  });

  test('refused, the signer keeps the id, the names that loaded stay, and the page names the read that failed', async ({ page }) => {
    await install(page, (r) => json(r, { error: 'people down' }, 503));
    await mountPage(page, PATH);

    await expect(section(page, /^Active contracts/).getByRole('link', { name: 'emp-cfo-2' })).toBeVisible();
    await expect(section(page, /^Account team/).getByRole('link', { name: 'Ben Lusk' })).toBeVisible();
    await expect(page.locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      "Couldn't load people — /api/people/emp-cfo-2: HTTP 503. Employees below show as ids rather than names.",
    );
  });
});
