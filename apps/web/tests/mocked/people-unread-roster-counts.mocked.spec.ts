// /ux/people prints a count only for a roster it has read (backlog
// 47eadca3; page audit 0c0265a3 GAP 3, 2026-09-23). The header and the
// Status and Department buttons counted the `[]` the roster starts as,
// so while /api/people was in flight — and above "Couldn't load the
// roster" when it answered 500 — the page stated "0 active employees",
// "0 certifications expiring in 90 days", Active (0) and All (0): the
// false-empty class, in the header of a page whose body was honest.
// The pure labels are pinned in src/people/roster-counts.test.ts and
// src/data/readState.test.ts (countLabel, backlog a97d4cf2); this
// spec pins the page wiring through a forced 500 and a held read.
//
// Its own file on purpose: outage-crawl and false-empty pin the body's
// failure line for this route, and a car editing both was parked when
// this one was built.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { DEPARTMENT_CLASSES, installSmokeMocks } from './_smokeMocks';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

function emp(id: string, status: string): Record<string, unknown> {
  return {
    id, name: id, email: `${id}@a`, role: 'ceo', department: 'it',
    hire_date: '2023-01-01', status, location: 'HQ',
    employment_type: 'full-time', skills: [], certifications: [],
  };
}

/// The three live status Classes, as 01-registries.sql seeds them —
/// so the Status buttons render whatever the roster read does.
const STATUS_CLASSES = [
  ['active', 'Active', 10], ['on-leave', 'On Leave', 20], ['terminated', 'Terminated', 30],
].map(([code, display_name, sort_order]) => ({
  subject_kind: 'employee', code, display_name, parent_code: null,
  member_attribute: 'status', metadata: {}, sort_order, retired_at: null,
}));

async function openRoster(page: Page, people: (r: Route) => Promise<void>): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/people$/, people);
  await page.route(/\/api\/classes(\?|$)/, (r) => json(r, [
    { subject_kind: 'employee', code: 'ceo', display_name: 'CEO', parent_code: null, member_attribute: 'role', metadata: {}, sort_order: 1, retired_at: null },
    ...DEPARTMENT_CLASSES,
    ...STATUS_CLASSES,
  ]));
  await mountPage(page, '/ux/people');
}

const title = (page: Page) => page.locator('.exec-title');
const header = (page: Page) => page.locator('header.exec-header');
const filters = (page: Page) => page.locator('aside.catalog-filters');

/// No filter button may carry a parenthesised count.
async function expectNoCountedButton(page: Page): Promise<void> {
  await expect(filters(page).getByRole('button', { name: /\(\d+\)/ })).toHaveCount(0);
}

test.describe('/ux/people counts only a roster it has read', () => {
  test('a failed roster read states no count in the header or the filters', async ({ page }) => {
    await openRoster(page, (r) => json(r, 'people store down', 500));

    await expect(page.locator('.load-failed')).toContainText("Couldn't load the roster");
    await expect(title(page)).toHaveText('Active employees');
    await expect(header(page)).toContainText('Counts unknown: the roster did not load');
    await expect(header(page)).not.toContainText('0 active employees');
    await expect(header(page)).not.toContainText('0 certifications');
    // The registry still names every status, so the buttons stay — bare.
    for (const name of ['Active', 'On Leave', 'Terminated']) {
      await expect(filters(page).getByRole('button', { name, exact: true })).toBeVisible();
    }
    await expect(filters(page).getByRole('button', { name: 'All', exact: true })).toHaveCount(2);
    await expectNoCountedButton(page);
  });

  test('a roster still loading states no count, then counts once it is read', async ({ page }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    await openRoster(page, async (r) => {
      await held;
      await json(r, [emp('emp-001', 'active'), emp('emp-002', 'on-leave')]);
    });

    await expect(page.getByText('Loading…')).toBeVisible();
    await expect(title(page)).toHaveText('Active employees');
    await expect(header(page)).toContainText('Loading the roster…');
    await expect(filters(page).getByRole('button', { name: 'Active', exact: true })).toBeVisible();
    await expectNoCountedButton(page);

    release();

    await expect(title(page)).toHaveText('1 active employees');
    await expect(header(page)).toContainText('0 certifications expiring in 90 days');
    for (const name of ['Active (1)', 'On Leave (1)', 'Terminated (0)', 'All (2)']) {
      await expect(filters(page).getByRole('button', { name, exact: true })).toBeVisible();
    }
  });
});
