// The founder's watch list is "Yours to decide", and it ages
// (backlog 3bc896be, design 5877860d, decided 2026-09-21).
//
// David routed a drive purchase to design review because /it/design
// was the one surface that showed him what was waiting on him; it sat
// there two days looking handled. The design chose the per-actor queue
// that already exists — these verdicts on My Day, which is `/` — and
// made age not optional: oldest first, and a row past its threshold
// reads differently, not only with a bigger number (q2).
//
// This spec is the page half of assignments.test.ts's `orderVerdicts`
// and `waitingOf`: an urgent verdict opened today must sit BELOW a
// standard one that has waited twenty days, and the old one must read
// in the stale band.

import { expect, test, type Page, type Route } from '@playwright/test';

const EMP = { id: 'emp-david', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

const DAY_MS = 86_400_000;
const daysAgo = (n: number) => new Date(Date.now() - n * DAY_MS).toISOString().slice(0, 10);

const verdict = (id: string, title: string, openedOn: string, priority: string) => ({
  job_id: id, job_title: title, due_on: null, opened_on: openedOn,
  workflow: 'backlog-item', subject_kind: 'custom', subject_id: 'bosspipeline',
  priority, simulated: false, tags: [],
  step: {
    id: `${id}-step`, job_id: id, kind: 'answer-question', title: 'Decide',
    assignee_id: 'emp-david', status: 'ready', metadata: {},
    completion: 'human', decision_shaped: true,
  },
});

const json = (r: Route, b: unknown) =>
  r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });

async function mocks(page: Page) {
  // Catch-all FIRST: Playwright matches routes in reverse registration
  // order, so later, more specific routes win over this one.
  await page.route('**/api/**', (r) => json(r, { data: [], total: 0 }));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: 'emp-david', role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/assignments/, (r) => json(r, { data: [
    verdict('11111111-0000-4000-8000-000000000001', 'Urgent and new', daysAgo(0), 'urgent'),
    verdict('22222222-0000-4000-8000-000000000002', 'Three drives, still waiting', daysAgo(20), 'standard'),
  ] }));
}

test('the oldest verdict leads and reads stale; the new urgent one reads fresh below it', async ({ page }) => {
  await mocks(page);
  await page.goto('/');

  const rows = page.locator('.myday-verdict-row');
  await expect(rows).toHaveCount(2);

  await expect(rows.nth(0)).toContainText('Three drives, still waiting');
  await expect(rows.nth(0).locator('.myday-age')).toHaveText(/open 20 d/i);
  await expect(rows.nth(0).locator('.myday-age')).toHaveAttribute('data-band', 'stale');

  await expect(rows.nth(1)).toContainText('Urgent and new');
  await expect(rows.nth(1).locator('.myday-age')).toHaveAttribute('data-band', 'fresh');
});
