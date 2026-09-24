// "My schedule" is a Home row for everyone the role's Class row lets
// see it — once, and decided by data, never by a closed role map
// (backlog c88fa303; David on page audit 0e4fef17, 2026-09-24: "Let's
// put it in the Home app").
//
// The audit's gap 3 read the row as reachable only through
// WORK_BY_ROLE (libs/web-kit/src/session/work-by-role.ts), whose
// `schedule` entries named seven brewery roles and none of the live
// roster. That read missed the second place the row lives: Home's
// "Mine" group (AppShell.svelte) lists it for every role, filtered only
// by canSeeRoute against the role's Class row `metadata.surfaces`. So
// the live audience already had the row, and the seven brewery roles
// had it TWICE — once in Work, once in Mine. These cases pin both
// halves: the live roster sees it once, a role the closed map used to
// list sees it once, and a Class row that leaves `schedule` out of its
// declared surfaces hides it — the tenant's data is the only gate.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { DEPARTMENT_CLASSES, installSmokeMocks } from './_smokeMocks';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

/// A role Class row. `surfaces` undefined = the row declares none,
/// which is the shape both live roles have (GET /api/classes?subject_kind=
/// employee, 2026-09-24: platform-admin and audit-readonly carry no
/// `surfaces` key).
function roleRow(code: string, surfaces?: ReadonlyArray<string>): Record<string, unknown> {
  return {
    subject_kind: 'employee', code, display_name: code, parent_code: null,
    member_attribute: 'role',
    metadata: surfaces === undefined ? {} : { surfaces },
    sort_order: 1, retired_at: null,
  };
}

/// Sign in as one rostered employee holding `role`, with `row` as that
/// role's Class row, and open Home (My Day at `/`).
async function homeAs(page: Page, role: string, row: Record<string, unknown>): Promise<void> {
  await installSmokeMocks(page);
  const emp = {
    id: 'emp-sched', name: 'Schedule Reader', email: 's@a', role,
    department: 'it', hire_date: '2023-01-01', status: 'active', location: 'HQ',
    employment_type: 'full-time', skills: [], certifications: [],
  };
  await page.route(/\/api\/people$/, (r) => json(r, [emp]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'reader', employee_id: emp.id, role }));
  await page.route(/\/api\/classes(\?|$)/, (r) => json(r, [row, ...DEPARTMENT_CLASSES]));
  await mountPage(page, '/');
  // The sidebar paints its groups once the session resolves a role; wait
  // for a row every role gets (Inbox is ungated) before counting.
  await expect(page.locator('.shell-sidebar a[href="/ux/inbox"]')).toHaveCount(1);
}

const scheduleRows = (page: Page) =>
  page.locator(`.shell-sidebar a[href="${ROUTE_CATALOG.schedule.path}"]`);

test.describe('My schedule is a Home row, gated by the role Class row alone', () => {
  // The live roster on 2026-09-24: emp-david (platform-admin) and
  // emp-audit (audit-readonly), neither Class row declaring surfaces.
  for (const role of ['platform-admin', 'audit-readonly']) {
    test(`${role} (the live roster) sees one My schedule row in Home`, async ({ page }) => {
      await homeAs(page, role, roleRow(role));
      await expect(scheduleRows(page)).toHaveCount(1);
      await expect(scheduleRows(page)).toHaveText('My schedule');
    });
  }

  // service-tech was one of the seven roles WORK_BY_ROLE listed
  // `schedule` for; it got the row in Work AND in Mine.
  test('a role the closed map used to list sees the row once, not twice', async ({ page }) => {
    await homeAs(page, 'service-tech', roleRow('service-tech'));
    await expect(scheduleRows(page)).toHaveCount(1);
  });

  test('a Class row whose declared surfaces leave schedule out hides the row', async ({ page }) => {
    await homeAs(page, 'service-tech', roleRow('service-tech', ['jobs', 'service']));
    await expect(scheduleRows(page)).toHaveCount(0);
  });

  test('a Class row declaring schedule shows it once', async ({ page }) => {
    await homeAs(page, 'senior-brewer', roleRow('senior-brewer', ['jobs', 'parts', 'schedule']));
    await expect(scheduleRows(page)).toHaveCount(1);
  });
});
