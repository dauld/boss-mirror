// Job Detail's landing guard (2026-08-21 UX audit, defect c).
//
// The page's fallback fetches race: the 30s poll, post-action
// refetches, and fast A→B navigation can all have answers in flight
// at once, and without a guard the SLOWEST one wins — packet A
// rendering under B's URL. Every question now carries a ticket
// (monotonic seq + the jobId it was asked about) checked before any
// assignment, so a stale answer is dropped instead of landed.

import { expect, test, type Page, type Route } from '@playwright/test';
import { answerRead, recordPageRequests } from './_helpers';

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

/// Packet A's answer, held until the test releases it — after B has
/// rendered, so the stale answer lands LAST, every time. `asked`
/// turns true when A's question reaches the route and is held there.
///
/// The page does not ask A's question at mount: it opens A's stream
/// first and fetches only once that stream has CLOSED (here, on the
/// catch-all's non-stream answer), which is some time after the goto
/// returns. Navigating to B before then is not a race the page loses —
/// A's effect is torn down and A is never asked, correctly — but it
/// leaves this spec nothing to release, and the wait for A's answer
/// below timed out on it: 2 of 4 full-suite runs on 2026-09-25, and
/// 2 of 40 / 1 of 40 of this spec alone, where the page's record of
/// the failing run held no request to A at all (backlog 815c176e).
async function mocks(page: Page): Promise<{ asked: () => boolean; release: () => void }> {
  let release: () => void = () => {};
  let arrived = false;
  const held = new Promise<void>((resolve) => { release = resolve; });
  await recordPageRequests(page);
  await page.route('**/api/**', (r) => json(r, { data: [], total: 0 }));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: 'emp-david', role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/job-edges$/, (r) => json(r, []));
  // Packet A answers SLOWLY — slower than the whole trip to B. It was a
  // 1 200 ms timer, with a 1 600 ms sleep after B to outlast it; under
  // load neither number says which answer landed last (backlog 840c5a76).
  await page.route(new RegExp(`/api/jobs/${A}$`), async (r) => {
    arrived = true;
    await held;
    return json(r, jobBody(A, 'Packet A, the slow one'));
  });
  await page.route(new RegExp(`/api/jobs/${B}$`), (r) =>
    json(r, jobBody(B, 'Packet B, where the reader went')));
  return { asked: () => arrived, release };
}

test('a fast A→B navigation never renders A under B\'s URL', async ({ page }) => {
  const { asked: askedA, release: releaseA } = await mocks(page);

  // Land on A and wait until its fetch is in flight and held — the
  // route's own record of it, not the goto, since the page asks only
  // after A's stream closes — then move to B the way the SPA does
  // before A ever answers.
  await page.goto(`/jobs/${A}`);
  await expect
    .poll(askedA, { intervals: [25], message: 'waiting for the page to ask for packet A, held at its route' })
    .toBe(true);
  await page.evaluate((b) => {
    window.history.pushState({}, '', `/jobs/${b}`);
    window.dispatchEvent(new PopStateEvent('popstate'));
  }, B);

  // B renders.
  await expect(page.locator('h1')).toContainText('Packet B');

  // …and KEEPS rendering after A's stale answer finally arrives. This
  // is the assertion that fails without the ticket check: A's slow
  // response used to land last and repaint the page. Released now, and
  // read by the page — body parsed, and what the page does with it done
  // — before anything is asserted.
  releaseA();
  await answerRead(page, new RegExp(`/api/jobs/${A}$`));
  await expect(page.locator('h1')).toContainText('Packet B');
  await expect(page.getByText('Packet A, the slow one')).toHaveCount(0);
});
