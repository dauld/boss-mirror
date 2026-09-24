// /ux/people/<id> when the ROSTER read fails (backlog 03c88448, found
// on run c8dc81e9 while fixing 548a1e8d, 2026-09-24). The page reads the
// employee's own row AND the whole roster, because direct reports and
// the reporting chain are computed from the roster. It parsed a refused
// roster as `[]`, so a /api/people outage painted "Direct reports 0" and
// "No manager — reports to board." — two confident statements the data
// never made, with no failure line anywhere.
//
// A failed roster read is now carried as a ReadState (data/readState.ts)
// and said in the two sections that depend on it, naming the read; the
// count it feeds says it is unknown rather than zero.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { FAILURE_MARKER } from './_routes';
import { installSmokeMocks } from './_smokeMocks';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const DETAIL = /\/api\/people\/emp-002$/;
const ROSTER = /\/api\/people$/;

const row = (id: string, name: string, manager_id: string | null) => ({
  id, name, email: `${id}@demo`, role: 'brewer', department: 'operations',
  skill_level: 3, skills: [], hire_date: '2021-01-01', location: 'HQ',
  manager_id, employment_type: 'full-time', status: 'active', certifications: [],
});

/// A middle manager: reports to emp-001, and emp-003 reports to them.
const BOSS = row('emp-001', 'Demo Boss', null);
const SELF = row('emp-002', 'Demo Manager', 'emp-001');
const REPORT = row('emp-003', 'Demo Report', 'emp-002');

async function openEmployee(
  page: Page,
  roster: (r: Route) => Promise<void>,
): Promise<string[]> {
  const errs: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));
  await installSmokeMocks(page);
  await page.route(ROSTER, roster);
  await page.route(DETAIL, (r) => json(r, SELF));
  await mountPage(page, '/ux/people/emp-002');
  return errs;
}

const directReports = (page: Page) =>
  page.locator('.detail-meta-item', { hasText: 'Direct reports' }).locator('.detail-meta-value');

test.describe('/ux/people/<id> names a failed roster read instead of an empty team', () => {
  test('control: a roster that answers renders the chain and the team, with no failure line', async ({ page }) => {
    const errs = await openEmployee(page, (r) => json(r, [BOSS, SELF, REPORT]));
    await expect(page.locator('h1.detail-title')).toHaveText('Demo Manager');
    await expect(directReports(page)).toHaveText('1');
    await expect(page.locator('ol.checklist li')).toContainText(['Demo Boss']);
    await expect(page.getByRole('heading', { name: 'Team (1 direct report)' })).toBeVisible();
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    expect(errs).toEqual([]);
  });

  test('a 500 on /api/people says so in the chain and the team, and states no zero', async ({ page }) => {
    const errs = await openEmployee(page, (r) => json(r, { error: 'down' }, 500));
    await expect(page.locator('h1.detail-title')).toHaveText('Demo Manager');
    await expect(page.locator(FAILURE_MARKER)).toHaveText([
      "Couldn't load the reporting chain — /api/people: HTTP 500",
      "Couldn't load direct reports — /api/people: HTTP 500",
    ]);
    await expect(directReports(page)).toHaveText('unknown');
    await expect(page.getByText('No manager — reports to board.')).toHaveCount(0);
    expect(errs).toEqual([]);
  });
});
