// /it/design as IN, WORKING and OUT (backlog 08372fdb, from page audit
// 79db48ae, 2026-09-23).
//
// The page rendered only an untouched IN queue: a review saved but not
// completed looked exactly like one never opened (the review step stays
// `ready` through a Save, so its answers are the only trace), a design
// left the page the moment its review completed, and nothing settled was
// shown. design-review v2 carries its members' steps; the `decided`
// panel reads the `design-decided` station for the other two thirds.
// Pinned here: what each third SAYS, and that the decided panel's failed
// read is its own failure line rather than an empty WORKING and OUT.
//
// The page's full controls inventory (every link, the Review button's
// destination and fallback, the queue's error line) is backlog 6bef2baa,
// a spec of its own.

import { expect, test, type Page, type Route } from '@playwright/test';
import { servePeopleRows } from './_smokeMocks';

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const EMP = { id: 'emp-david', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

const Q = (n: number) =>
  Array.from({ length: n }, (_, i) => ({ anchor: `Q${i + 1}`, title: `q${i + 1}`, proposal: 'p' }));

const D = (id: string, title: string, status: string, extra: Record<string, unknown> = {}) => ({
  id, kind: 'design-doc', title, status, priority: 'standard', opened_on: '2026-09-20',
  closed_on: null, tags: [], metadata: {}, simulated: false, ...extra,
});

const review = (id: string, status: string, questions: unknown[], resolutions: unknown[], completed_on: string | null = null) =>
  ({ id, kind: 'review-design', spec_slug: 'review', status, completed_on, metadata: { questions, resolutions } });

const fold = (id: string, status: string, metadata: Record<string, unknown> = {}) =>
  ({ id, kind: 'task', spec_slug: 'fold', status, metadata });

const IN = {
  station: 'design-review', kind: 'batch', discipline: ['priority', 'age'], over_limit: false,
  total: 2,
  lens: { title: 'Design review', panels: ['queue', 'decided'], with_steps: true },
  data: [D('in-saved', 'A half-answered design', 'open'), D('in-fresh', 'An untouched design', 'open')],
  steps: {
    'in-saved': [review('rs-saved', 'ready', Q(3), [{ anchor: 'Q1', decision: 'yes' }, { anchor: 'Q2', decision: 'no' }])],
    'in-fresh': [review('rs-fresh', 'ready', Q(2), [])],
  },
};

const DECIDED = {
  station: 'design-decided', kind: 'batch', discipline: ['recency'], over_limit: false,
  terminal_window_days: 7, total: 2,
  lens: { title: 'Decided designs', with_steps: true },
  data: [
    D('w-fold', 'A design being folded', 'open'),
    D('o-done', 'A settled design', 'closed', { closed_on: '2026-09-23', metadata: { outcome: 'published' } }),
  ],
  steps: {
    'w-fold': [review('r1', 'completed', Q(1), [{ anchor: 'Q1', decision: 'ok' }], '2026-09-23'), fold('f1', 'active')],
    'o-done': [review('r2', 'completed', Q(1), [{ anchor: 'Q1', decision: 'ok' }], '2026-09-22'),
      fold('f2', 'completed', { folded_into: 'docs/architecture-decisions.md §Design docs' })],
  },
};

async function mocks(page: Page, decided: (r: Route) => Promise<void>): Promise<void> {
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await servePeopleRows(page, [EMP]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: 'emp-david', role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/live$/, (r) => json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(/\/api\/stations\/design-review\/queue/, (r) => json(r, IN));
  await page.route(/\/api\/stations\/design-decided\/queue/, decided);
}

test('IN says how far each review has got — a saved one does not read as untouched', async ({ page }) => {
  await mocks(page, (r) => json(r, DECIDED));
  await page.goto('/it/design');

  const row = (title: string) => page.locator('.design-table tr').filter({ hasText: title });
  await expect(row('A half-answered design')).toContainText('saved · 2 of 3 answered');
  await expect(row('An untouched design')).toContainText('not started · 2 questions');
});

test('WORKING and OUT render what the decided station holds', async ({ page }) => {
  await mocks(page, (r) => json(r, DECIDED));
  await page.goto('/it/design');

  await expect(page.getByText('Decided, being folded (1)')).toBeVisible();
  const working = page.locator('.decided-table tr').filter({ hasText: 'A design being folded' });
  await expect(working).toContainText('being folded');

  await expect(page.getByText('Settled in the last 7 days (1)')).toBeVisible();
  const settled = page.locator('.decided-table tr').filter({ hasText: 'A settled design' });
  await expect(settled).toContainText('published');
  await expect(settled).toContainText('docs/architecture-decisions.md §Design docs');
});

test('a failed decided read says so, and the review queue still renders', async ({ page }) => {
  await mocks(page, (r) => json(r, 'down', 503));
  await page.goto('/it/design');

  await expect(page.locator('.load-failed')).toContainText('Could not read the decided designs: HTTP 503');
  // Not an empty WORKING and OUT: a failed read is never "nothing decided".
  await expect(page.getByText('Nothing decided is waiting to be folded.')).toHaveCount(0);
  await expect(page.locator('.design-table')).toContainText('A half-answered design');
});
