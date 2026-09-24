// The handoff step surface — naming who hands off and who receives.
//
// Backlog 1e73bd93 (the last car). HandoffSurface read the WHOLE
// employee roster to name two ids, and a refusal became `[]` while a
// network error fell into `.catch(() => {})`, so a person silently
// became a raw id. It now reads one row per id shown through the shared
// reader (src/data/ownerNames.ts) and says so when a name cannot load.
//
// A handoff's ends are a PERSON OR A TEAM — the StepType says so
// ("Person or team handing off"), and every seeded handoff names a
// team or a role (`inventory-clerk` → `shipping-clerk`). The people
// service answers such an id 404 "no employee with ID …": the read
// worked and the answer is "not a person", so the id is its label and
// no failure line is painted — the second test would otherwise be a
// false alarm on every brewery handoff.

import { expect, test, type Page, type Route } from '@playwright/test';

const JOB_ID = 'job-handoff-names';

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const EMP = { id: 'emp-001', name: 'Roster Copy', email: 'd@a', role: 'platform-admin' };

const STEP = {
  id: 's1', job_id: JOB_ID, title: 'cellar to packaging', kind: 'handoff', status: 'active',
  assignee_id: null, sort_order: 0, blocked_by: [], sign_offs_required: [], sign_offs: [],
  metadata: { from_id: 'emp-001', to_id: 'shipping-clerk', from_confirmed: false, to_confirmed: false },
  notes: null,
};

/// The job page with one handoff step; `person` answers the one person
/// on it. The ROSTER names that person differently, so a surface that
/// still reads the whole roster shows the wrong name and fails the
/// first test rather than passing it by accident.
async function install(page: Page, person: (r: Route) => Promise<void>): Promise<string[]> {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });
  const job = {
    id: JOB_ID, kind: 'user-feedback', title: 'Handoff fixture', status: 'open',
    opened_on: '2026-08-20', due_on: null, closed_on: null, owner_id: EMP.id,
    priority: 'standard', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'fixture' }, metadata: {},
  };
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'handoff', label: 'Handoff', category: 'generic', ux: 'inline', description: '', surface: 'handoff' },
  ]));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, { ...job, steps: [STEP] }));
  await page.route(/\/api\/people\/emp-001$/, person);
  // The people service's own answer for an id that is not a person
  // (boss-people http.rs `get_employee`): 404, plain text.
  await page.route(/\/api\/people\/shipping-clerk$/, (r) =>
    r.fulfill({ status: 404, contentType: 'text/plain', body: 'no employee with ID shipping-clerk' }));
  // One-row reads only: the shell's session loader reads the roster for
  // itself, so a bare /api/people here is not the surface's read.
  const asked: string[] = [];
  page.on('request', (req) => {
    const path = new URL(req.url()).pathname;
    if (/^\/api\/people\/[^/]+$/.test(path)) asked.push(path);
  });
  return asked;
}

const surface = (page: Page) => page.locator('.step-handoff');
const names = (page: Page) => surface(page).locator('.step-handoff-name');

test.describe('the handoff surface — naming both ends', () => {
  test('answered, a person is named from their own row, a team keeps its id, one read each, and nothing says a read failed', async ({ page }) => {
    const asked = await install(page, (r) => json(r, { id: 'emp-001', name: 'David Auld' }));
    await page.goto(`/ux/jobs/${JOB_ID}`);

    await expect(names(page).nth(0)).toHaveText('David Auld');
    await expect(names(page).nth(1)).toHaveText('shipping-clerk');
    await expect(surface(page).locator('.load-failed')).toHaveCount(0);
    expect([...new Set(asked)].sort()).toEqual(['/api/people/emp-001', '/api/people/shipping-clerk']);
  });

  test('refused, the person keeps the id and the surface says which read failed', async ({ page }) => {
    await install(page, (r) => json(r, { error: 'people down' }, 503));
    await page.goto(`/ux/jobs/${JOB_ID}`);

    await expect(names(page).nth(0)).toHaveText('emp-001');
    await expect(surface(page).locator('.load-failed[role=alert]')).toHaveText(
      "Couldn't load the names on this handoff — /api/people/emp-001: HTTP 503. People show as ids.",
    );
  });
});
