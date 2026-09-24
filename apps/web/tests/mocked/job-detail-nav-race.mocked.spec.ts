// Job Detail's landing guard (2026-08-21 UX audit, defect c).
//
// The page's fallback fetches race: the 30s poll, post-action
// refetches, and fast A→B navigation can all have answers in flight
// at once, and without a guard the SLOWEST one wins — packet A
// rendering under B's URL. Every question now carries a ticket
// (monotonic seq + the jobId it was asked about) checked before any
// assignment, so a stale answer is dropped instead of landed.

import { expect, test, type Page, type Route } from '@playwright/test';

const EMP = { id: 'emp-david', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

const A = '00000000-0000-0000-0000-00000000000a';
const B = '00000000-0000-0000-0000-00000000000b';

const jobBody = (id: string, title: string) => ({
  id, kind: 'user-feedback', title, status: 'open',
  subject: { subject_kind: 'custom', id: '/ux/jobs' },
  owner_id: 'emp-david', priority: 'standard', opened_on: '2026-08-14',
  due_on: null, closed_on: null, metadata: {}, tags: [], steps: [],
});

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

async function mocks(page: Page) {
  await page.route('**/api/**', (r) => json(r, { data: [], total: 0 }));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: 'emp-david', role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/job-edges$/, (r) => json(r, []));
  // Packet A answers SLOWLY — slower than the whole trip to B.
  await page.route(new RegExp(`/api/jobs/${A}$`), async (r) => {
    await new Promise((resolve) => setTimeout(resolve, 1200));
    return json(r, jobBody(A, 'Packet A, the slow one'));
  });
  await page.route(new RegExp(`/api/jobs/${B}$`), (r) =>
    json(r, jobBody(B, 'Packet B, where the reader went')));
}

test('a fast A→B navigation never renders A under B\'s URL', async ({ page }) => {
  await mocks(page);

  // Land on A (its fetch is now in flight and will take 1.2s), then
  // move to B the way the SPA does before A ever answers.
  await page.goto(`/jobs/${A}`);
  await page.evaluate((b) => {
    window.history.pushState({}, '', `/jobs/${b}`);
    window.dispatchEvent(new PopStateEvent('popstate'));
  }, B);

  // B renders.
  await expect(page.locator('h1')).toContainText('Packet B');

  // …and KEEPS rendering after A's stale answer finally arrives. This
  // is the assertion that fails without the ticket check: A's slow
  // response used to land last and repaint the page.
  await page.waitForTimeout(1600);
  await expect(page.locator('h1')).toContainText('Packet B');
  await expect(page.getByText('Packet A, the slow one')).toHaveCount(0);
});
