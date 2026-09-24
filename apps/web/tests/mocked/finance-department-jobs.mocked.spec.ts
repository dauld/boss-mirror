// /ux/finance — the finance department's own packets, as In / Working /
// Out, under every tab (backlog 4d4dc204, page audit 3f964c57 gap 2,
// measured against origin/main 642c0171).
//
// GET /api/jobs?department=finance answered one open packet at the
// audit's measure step — receive-a-payout 931c3bfe, "Payout: 1.00 USD
// arriving 2026-09-21", its post step ready since 2026-09-21T01:10Z —
// and /ux/finance made no jobs read and linked nowhere that did, so the
// one piece of live work finance held was on no finance surface. The
// page now makes the department read every department's own view makes
// (departments/department.ts), keyed by the catalog entry's
// `department`, draws the same three thirds /ux/parts does (044dffa1),
// and says which step each live packet stands at.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage, settledReads } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const PATH = ROUTE_CATALOG.finance.path;
const DEPT_JOBS = /\/api\/jobs\?(.*&)?department=finance(&|$)/;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const SUMMARY = {
  revenue_ttm: [], total_revenue_ttm_cents: 0, total_cogs_ttm_cents: 0,
  total_gross_margin_ttm_cents: 0, ar_aging: [], total_outstanding_cents: 0,
  total_invoice_count: 0, revenue_by_month: [], currency: 'USD',
};

const panel = (page: Page) => page.locator('.department-jobs');

type StepFixture = Readonly<{ title: string; status: string }>;

const job = (
  id: string, kind: string, title: string, status: string,
  steps: ReadonlyArray<StepFixture>, closed_on: string | null = null,
) => ({
  id, kind, title, status, priority: 'standard',
  subject: { subject_kind: 'custom', id: 'algedonic' }, owner_id: 'emp-1',
  opened_on: '2026-09-21', due_on: null, closed_on, metadata: {}, tags: [],
  steps: steps.map((s, i) => ({ id: `s${i}`, kind: 'task', title: s.title, status: s.status, sort_order: i })),
});

const PAYOUT = job(
  '931c3bfe-0000-0000-0000-000000000001', 'receive-a-payout', 'Payout: 1.00 USD arriving 2026-09-21', 'open',
  [{ title: 'Record the payout', status: 'completed' }, { title: 'Post the payout', status: 'ready' }],
);
const JOBS = [
  PAYOUT,
  job('22222222-0000-0000-0000-000000000002', 'receive-a-bill', 'Bill from the maltster', 'open',
    [{ title: 'Check the bill', status: 'ready' }, { title: 'Pay the bill', status: 'pending' }]),
  job('33333333-0000-0000-0000-000000000003', 'receive-a-payout', 'Payout: 9.00 USD', 'closed',
    [{ title: 'Post the payout', status: 'completed' }], '2026-09-22'),
];

/// The shell, then the page's own reads with the smallest honest
/// answers, so the only thing a test varies is the department read.
async function install(page: Page, deptJobs: (r: Route) => Promise<void>): Promise<string[]> {
  await installSmokeMocks(page);
  await page.route(/\/api\/commerce\/invoices/, (r) =>
    json(r, { data: [], total: 0, limit: 1000, offset: 0 }));
  await page.route(/\/api\/commerce\/summary$/, (r) => json(r, SUMMARY));
  const urls: string[] = [];
  await page.route(DEPT_JOBS, (r) => {
    urls.push(r.request().url());
    return deptJobs(r);
  });
  return urls;
}

test('mount reads the finance department once and draws In / Working / Out, naming where each waits', async ({ page }) => {
  const urls = await install(page, (r) => json(r, { data: JOBS, total: JOBS.length }));
  await mountPage(page, PATH);

  await expect(panel(page).locator('h2')).toHaveText('Finance jobs');
  await expect(panel(page).locator('h3')).toHaveText(['In (1)', 'Working (1)', 'Out (1)']);
  const sections = panel(page).locator('section.list-section');
  await expect(sections.nth(0).locator('tbody tr td:nth-child(3)')).toHaveText(['Bill from the maltster']);
  await expect(sections.nth(1).locator('tbody tr td:nth-child(3)')).toHaveText([PAYOUT.title]);
  await expect(sections.nth(2).locator('tbody tr td:nth-child(3)')).toHaveText(['Payout: 9.00 USD']);

  // The step each live packet stands at; a departed one waits on nothing.
  await expect(sections.nth(0).locator('td.waiting-at')).toHaveText(['Check the bill']);
  await expect(sections.nth(1).locator('td.waiting-at')).toHaveText(['Post the payout']);
  await expect(sections.nth(2).locator('td.waiting-at')).toHaveText(['']);

  // One click from the tab: the id opens the packet.
  await expect(panel(page).getByRole('link', { name: '00000001', exact: true })).toHaveAttribute(
    'href', `/ux/jobs/${PAYOUT.id}`,
  );

  expect(await settledReads(page, () => urls.length, 1)).toBe(1);
  const q = new URL(urls[0]!).searchParams;
  expect([q.get('department'), q.get('closed_within'), q.get('limit')]).toEqual(['finance', '30', '200']);
});

test('the packets stand under every tab, and a tab click does not re-read them', async ({ page }) => {
  const urls = await install(page, (r) => json(r, { data: JOBS, total: JOBS.length }));
  await mountPage(page, PATH);
  await expect(panel(page).locator('h3')).toHaveText(['In (1)', 'Working (1)', 'Out (1)']);

  await page.getByRole('tab', { name: 'Invoices', exact: true }).click();
  await expect(panel(page).locator('h3')).toHaveText(['In (1)', 'Working (1)', 'Out (1)']);
  expect(await settledReads(page, () => urls.length, 1)).toBe(1);
});

test('a department with no packets says so, and is not a failure', async ({ page }) => {
  await install(page, (r) => json(r, { data: [], total: 0 }));
  await mountPage(page, PATH);
  await expect(panel(page).locator('p.empty')).toHaveText(
    'No jobs in Finance: no packet of a kind whose workflow declares this department is live or closed in the last 30 days.',
  );
  await expect(panel(page).locator('table')).toHaveCount(0);
  await expect(panel(page).locator(FAILURE_MARKER)).toHaveCount(0);
});

test('a failed department read is said, and the statements still render', async ({ page }) => {
  await install(page, (r) => json(r, { error: 'down' }, 503));
  await mountPage(page, PATH);
  await expect(panel(page).locator(FAILURE_MARKER)).toContainText("Couldn't load this department's jobs:");
  await expect(panel(page).locator(FAILURE_MARKER)).toContainText('HTTP 503');
  await expect(page.getByRole('tab', { name: 'Overview', exact: true })).toBeVisible();
});
