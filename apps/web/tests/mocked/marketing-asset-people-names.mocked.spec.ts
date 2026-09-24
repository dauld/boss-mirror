// /ux/marketing-assets/{id} — naming the asset's owner and brand reviewer.
//
// Backlog 1e73bd93 (the last car). The page read the WHOLE employee
// roster to name two people, and a refusal was an `if (pResp.ok)` with
// no else, so the owner and the reviewer silently became raw ids. It
// now reads one row per person shown through the shared reader
// (src/data/ownerNames.ts) and says so when a name cannot load.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks, MARKETING_ASSET_DETAIL } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const PATH = '/ux/marketing-assets/ma-owned';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const ASSET = {
  id: 'ma-owned', title: 'Autumn tap list', kind: null, description: null, file_url: null,
  tags: [], linked_device_skus: [], linked_account_ids: [], linked_campaign_ids: [],
  owner_id: 'emp-mkt-1', brand_reviewed_by: 'emp-brand-2', brand_reviewed_at: null,
  supersedes_id: null, retired_at: null,
  created_at: '2026-09-01T00:00:00Z', updated_at: '2026-09-02T00:00:00Z',
};

/// Every read the page makes, answered; `reviewer` decides the one
/// under test. The ROSTER names both people differently, so a page that
/// still reads the whole roster shows the wrong names and fails the
/// first test rather than passing it by accident.
async function install(page: Page, reviewer: (r: Route) => Promise<void>): Promise<string[]> {
  await installSmokeMocks(page);
  await page.route(MARKETING_ASSET_DETAIL, (r) => json(r, ASSET));
  await page.route(/\/api\/catalog\/marketing-assets\/[^/]+\/history$/, (r) => json(r, [ASSET]));
  await page.route(/\/api\/people$/, (r) =>
    json(r, [{ id: 'emp-mkt-1', name: 'Roster Owner' }, { id: 'emp-brand-2', name: 'Roster Reviewer' }]),
  );
  await page.route(/\/api\/people\/emp-mkt-1$/, (r) => json(r, { id: 'emp-mkt-1', name: 'Mara Lind' }));
  await page.route(/\/api\/people\/emp-brand-2$/, reviewer);
  // One-row reads only: the shell's session loader reads the roster for
  // itself, so a bare /api/people here is not the page's read.
  const asked: string[] = [];
  page.on('request', (req) => {
    const path = new URL(req.url()).pathname;
    if (/^\/api\/people\/emp-[^/]+$/.test(path)) asked.push(path);
  });
  return asked;
}

const profile = (page: Page) => page.locator('section', { has: page.getByRole('heading', { name: 'Profile' }) });
const cellAfter = (page: Page, label: string) => profile(page).locator(`dt:text-is("${label}") + dd`);

test.describe('/ux/marketing-assets/{id} — naming the people on the asset', () => {
  test('answered, owner and reviewer are named from their own rows, one read each, and nothing says a read failed', async ({ page }) => {
    const asked = await install(page, (r) => json(r, { id: 'emp-brand-2', name: 'Theo Brandt' }));
    await mountPage(page, PATH);

    await expect(cellAfter(page, 'Owner').getByRole('link')).toHaveText('Mara Lind');
    await expect(cellAfter(page, 'Brand-reviewed').getByRole('link')).toHaveText('Theo Brandt');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    expect([...new Set(asked)].sort()).toEqual(['/api/people/emp-brand-2', '/api/people/emp-mkt-1']);
  });

  test('refused, the reviewer keeps the id, the owner keeps the name that loaded, and the page says which read failed', async ({ page }) => {
    await install(page, (r) => json(r, { error: 'people down' }, 503));
    await mountPage(page, PATH);

    await expect(cellAfter(page, 'Owner').getByRole('link')).toHaveText('Mara Lind');
    await expect(cellAfter(page, 'Brand-reviewed').getByRole('link')).toHaveText('emp-brand-2');
    await expect(profile(page).locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      "Couldn't load the names on this asset — /api/people/emp-brand-2: HTTP 503. People show as ids.",
    );
  });
});
