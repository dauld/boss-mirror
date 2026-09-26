// David, 2026-08-23, on /ux/marketing-assets/ma-seasonal-winter-2026:
// "I can't read the text on the tags on this page... Might be
// light/dark theme support, which we should eliminate. Everything
// should be our UI decision so we can make sure it looks good."
//
// There was no theme support to blame. The tag chips hand-rolled
// `background:#e7e5e4` — a light ground from the pre-Design-Language
// prototype — under text the app paints in --fog (#e8ecef). Light on
// light: a measured 1.03:1, i.e. invisible. The RETIRED pill on the
// list page was the same shape.
//
// The fix is the house `.chip` / `.chips` classes, which resolve to
// the one palette. These pin the property David actually reported —
// can you read it — rather than the class names, because a component
// can always hand-roll its way back to unreadable while keeping the
// markup we asked for.
//
// `infra/lint/one-palette.sh` guards the other door: a
// `prefers-color-scheme` block reappearing in apps/ or libs/.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { AA_FLOOR, describeUnreadable, measureContrast } from './_contrast';
import { servePeopleRows } from './_smokeMocks';

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const EMP = {
  id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'marketing', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [],
};

const ASSET = {
  id: 'ma-seasonal-winter-2026',
  title: 'Winter 2026 seasonal launch kit',
  kind: 'deck',
  description: 'Deck, one-pager and hero stills for the winter seasonal.',
  file_url: null,
  // The reported element. Several, because a single chip could pass on
  // an accident of where it landed.
  tags: ['seasonal', 'winter-2026', 'launch', 'brand-reviewed'],
  linked_device_skus: [],
  linked_account_ids: [],
  linked_campaign_ids: [],
  owner_id: EMP.id,
  brand_reviewed_by: null,
  brand_reviewed_at: null,
  supersedes_id: null,
  retired_at: null,
  created_at: '2026-01-04T09:00:00Z',
  updated_at: '2026-01-11T09:00:00Z',
};

const RETIRED = {
  ...ASSET,
  id: 'ma-seasonal-winter-2025',
  title: 'Winter 2025 seasonal launch kit',
  retired_at: '2026-01-04T09:00:00Z',
};

async function mocks(page: Page) {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/tenant\/manifest$/, (r) =>
    json(r, { display_name: 'Algedonic Ales', modules: { 'marketing-assets': true }, labels: {} }));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await servePeopleRows(page, [EMP]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: 'platform-admin' }));
}

test('the tags on a marketing asset are readable', async ({ page }) => {
  await mocks(page);
  await page.route(/\/api\/catalog\/marketing-assets\/[^/]+$/, (r) => json(r, ASSET));
  await page.route(/\/api\/catalog\/marketing-assets\/[^/]+\/history$/, (r) => json(r, [ASSET]));

  await mountPage(page, `/ux/marketing-assets/${ASSET.id}`);
  const tags = page.locator('.tab-section').filter({ hasText: 'Tags (' });

  // Measured over every span the section renders, not over `.chip`:
  // the defect was markup that opted OUT of the house classes, and a
  // selector naming them would have found nothing and passed.
  const measured = await measureContrast(tags, 'span');
  expect(measured.length, 'no tags rendered').toBeGreaterThanOrEqual(ASSET.tags.length);
  const unreadable = measured.filter((m) => m.ratio < AA_FLOOR);
  expect(
    unreadable,
    `marketing-asset tags below ${AA_FLOOR}:1 contrast:\n${describeUnreadable(unreadable)}`,
  ).toEqual([]);

  // And they get there the house way, so the next edit inherits the
  // palette instead of re-deriving it.
  await expect(tags.locator('.chips .chip')).toHaveCount(ASSET.tags.length);
});

test('the RETIRED pill in the asset list is readable', async ({ page }) => {
  await mocks(page);
  await page.route(/\/api\/catalog\/marketing-assets\?/, (r) => json(r, [RETIRED]));

  await mountPage(page, '/ux/marketing-assets');
  // The list only shows retired assets once you ask for them.
  await page.getByRole('button', { name: 'Include retired' }).click();
  await expect(page.locator('.chip')).toHaveText(['RETIRED']);

  const measured = await measureContrast(page.locator('.catalog'), '.chip');
  const unreadable = measured.filter((m) => m.ratio < AA_FLOOR);
  expect(
    unreadable,
    `asset-list chips below ${AA_FLOOR}:1 contrast:\n${describeUnreadable(unreadable)}`,
  ).toEqual([]);
});
