// /ux/qa — the page as it stands, pinned (page audit 7c8757ea, step `test`).
//
// A HOLDING PIN, not a destination. The audit's `measure` step decided
// to delete QaPage.svelte and render /ux/qa as the QA department's
// thirds (DepartmentThirds over /api/jobs?department=qa), and that car
// is backlog ca2f18ec, which carries its own mocked spec for the page
// that replaces this one. Until it lands, this file pins what exists:
//
//   - the module gate: the live manifest (qa = false) renders the
//     ModuleDisabled notice, whose one button goes home and back
//     returns; a manifest listing ONLY qa renders the page;
//   - with qa on: the four tab buttons, the four static links and the
//     two row-link families (job, employee) land on catalogued routes,
//     back returns; the `surfaces` filter decides which kinds count;
//   - an empty backend paints its empty lines, a failed read paints the
//     failure marker and none of them — never the same paint;
//   - the page writes nothing, so no refusal can be swallowed.
//
// It deliberately does NOT pin the defects the audit filed against the
// page (the zero subtitle beside the failure line, the tab lost on
// reload, every link going to every open job): ca2f18ec removes them
// with the page, and a pin on a defect is a vote to keep it. When that
// car deletes QaPage.svelte it deletes this file with it.

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { FAILURE_MARKER } from './_routes';
import {
  installSmokeMocks, installTenantManifest, LIVE_MANIFEST_RECORDED_AT, MODULES_LIVE,
} from './_smokeMocks';
import { parseRoute } from '../../src/router';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { sectionForRoute } from '../../src/shell/sections';

const PAGE = '/ux/qa';
const TITLE = 'Quality Assurance';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

/// The page's four reads, as QaPage.svelte spells them (one Promise.all).
const PEOPLE = /\/api\/people$/;
const SUMMARY = /\/api\/jobs\/summary\?status=open$/;
const LIVE = /\/api\/jobs\/live$/;
const WORKFLOWS = /\/api\/workflows$/;

const workflow = (kind: string, surfaces: string[]) => ({
  kind, version: 1, label: kind, description: '', subject_kinds: [], steps: [],
  metadata: { surfaces },
});

/// One kind the registry flags for QA, one it does not — so a count or
/// a row that ignored the `surfaces` filter shows up as the other kind.
const WORKFLOW_ROWS = [workflow('qa-check', ['qa']), workflow('backlog-item', [])];
const COUNTS = { 'qa-check': 2, 'backlog-item': 5 };

const QA_JOB = {
  id: 'job-qa000001-aaaa', kind: 'qa-check', title: 'Gravity check, batch 12',
  status: 'open', priority: 'standard', opened_on: '2026-09-20',
};
const OTHER_JOB = {
  id: 'job-bl000002-bbbb', kind: 'backlog-item', title: 'Not QA work',
  status: 'open', priority: 'standard', opened_on: '2026-09-21',
};

/// A lab-tech (in the staffing roster) with one certification that
/// expires far outside every horizon, and someone who is not.
const LAB = {
  id: 'emp-lab', name: 'Lab Tech One', email: 'lab@demo', role: 'lab-tech',
  department: 'qa', hire_date: '2020-01-01', status: 'active', location: 'Lab',
  employment_type: 'full-time', skills: [],
  certifications: [{ name: 'Cicerone', issuing_body: 'Cicerone Program', expires_on: '2099-01-01' }],
};
const OTHER = { ...LAB, id: 'emp-ceo', name: 'Demo CEO', role: 'ceo', department: 'exec', certifications: [] };

async function installQaReads(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(PEOPLE, (r) => json(r, [LAB, OTHER]));
  await page.route(SUMMARY, (r) => json(r, { counts: COUNTS, total: 7 }));
  await page.route(LIVE, (r) =>
    json(r, { counts: COUNTS, open_total: 7, recent: [OTHER_JOB, QA_JOB], sim_clock: {} }),
  );
  await page.route(WORKFLOWS, (r) => json(r, WORKFLOW_ROWS));
}

/// Is `path` a catalogued route: the router serves it and the section it
/// lights is a ROUTE_CATALOG key. Read from nav-catalog.ts and
/// sections.ts, never from a second list here.
function catalogued(path: string): string | null {
  const [pathname, search = ''] = path.split('?');
  const section = sectionForRoute(parseRoute(pathname!, search ? `?${search}` : ''));
  return section in ROUTE_CATALOG ? section : null;
}

/// Every non-GET the page sends, minus the shell's own route-open record.
function watchWrites(page: Page): Request[] {
  const writes: Request[] = [];
  page.on('request', (req) => {
    const u = new URL(req.url());
    if (req.method() !== 'GET' && u.pathname.startsWith('/api/') && u.pathname !== '/api/surface-opens') {
      writes.push(req);
    }
  });
  return writes;
}

const tab = (page: Page, name: string) => page.getByRole('tab', { name, exact: true });

test.describe('/ux/qa module gate', () => {
  test(`the live manifest recorded ${LIVE_MANIFEST_RECORDED_AT} (qa = false) renders the notice, and its one button goes home`, async ({ page }) => {
    expect(MODULES_LIVE['qa'], 'the recording lists qa off').toBe(false);
    await installQaReads(page);
    await installTenantManifest(page, MODULES_LIVE);
    await mountPage(page, PAGE);

    const notice = page.locator('.module-disabled');
    await expect(notice.locator('h1')).toHaveText('Not enabled for this tenant');
    await expect(notice.locator('strong')).toHaveText(ROUTE_CATALOG.qa.label);
    await expect(notice.getByRole('button')).toHaveText(['Back to home']);
    await expect(page.getByRole('tab')).toHaveCount(0);

    await notice.getByRole('button', { name: 'Back to home' }).click();
    await expect(page).toHaveURL((u) => u.pathname === '/');
    await page.goBack();
    await expect(page).toHaveURL((u) => u.pathname === PAGE);
    await expect(page.locator('.module-disabled h1')).toHaveText('Not enabled for this tenant');
  });

  test('a manifest listing ONLY qa renders the page', async ({ page }) => {
    await installQaReads(page);
    await installTenantManifest(page, { qa: true });
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    await expect(page.locator('.module-disabled')).toHaveCount(0);
  });
});

test.describe('/ux/qa controls (qa on)', () => {
  test('the four tabs switch the view, and only kinds flagged for QA are counted or listed', async ({ page }) => {
    const writes = watchWrites(page);
    await installQaReads(page);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    await expect(page.getByRole('tab')).toHaveText([
      'Overview', 'Batch QC', 'Compliance', 'Equipment preventive maintenance',
    ]);
    await expect(tab(page, 'Overview')).toHaveAttribute('aria-selected', 'true');
    // 2 of the 7 open packets are a kind flagged `surfaces: ['qa']`.
    const kv = page.locator('dl.kv').first();
    await expect(kv.locator('dt').first()).toHaveText('Open QA jobs');
    await expect(kv.locator('dd').first()).toHaveText('2');

    await tab(page, 'Batch QC').click();
    await expect(tab(page, 'Batch QC')).toHaveAttribute('aria-selected', 'true');
    await expect(tab(page, 'Overview')).toHaveAttribute('aria-selected', 'false');
    const tables = page.locator('table.data-table');
    await expect(tables.first().locator('tbody tr')).toHaveCount(1);
    await expect(tables.first()).toContainText(QA_JOB.title);
    await expect(page.getByText(OTHER_JOB.title)).toHaveCount(0);
    // The staffing roster: the lab-tech, not the CEO.
    await expect(tables.nth(1).locator('tbody tr')).toHaveCount(1);
    await expect(tables.nth(1)).toContainText(LAB.name);

    await tab(page, 'Compliance').click();
    await expect(tab(page, 'Compliance')).toHaveAttribute('aria-selected', 'true');
    await expect(page.locator('table.data-table tbody tr')).toHaveText([/Cicerone Program\s*1/]);
    await expect(page.getByText('No certifications expiring within 90 days.')).toBeVisible();

    await tab(page, 'Equipment preventive maintenance').click();
    await expect(tab(page, 'Equipment preventive maintenance')).toHaveAttribute('aria-selected', 'true');
    await expect(page.locator('table.data-table tbody tr')).toHaveCount(1);
    await expect(page.getByRole('columnheader', { name: 'Job' })).toBeVisible();

    expect(writes.map((w) => `${w.method()} ${w.url()}`), 'the page writes nothing').toEqual([]);
  });

  test('the four static links land on the catalogued jobs route, and back returns here', async ({ page }) => {
    await installQaReads(page);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const link = (name: string) => page.locator('.catalog a', { hasText: name });
    const hrefs: string[] = [];
    await expect(link('View open jobs')).toHaveCount(2);
    hrefs.push(...(await link('View open jobs').evaluateAll((as) => as.map((a) => a.getAttribute('href') ?? ''))));
    await tab(page, 'Batch QC').click();
    await expect(link('Open all jobs')).toHaveCount(1);
    hrefs.push((await link('Open all jobs').getAttribute('href')) ?? '');
    await tab(page, 'Equipment preventive maintenance').click();
    await expect(link('Open all jobs')).toHaveCount(1);
    hrefs.push((await link('Open all jobs').getAttribute('href')) ?? '');

    expect(hrefs).toEqual(Array(4).fill('/ux/jobs?status=open'));
    expect(catalogued(hrefs[0]!)).toBe(ROUTE_CATALOG.jobs.id);

    await tab(page, 'Overview').click();
    await link('View open jobs').first().click();
    await expect(page).toHaveURL((u) => u.pathname === '/ux/jobs' && u.searchParams.get('status') === 'open');
    await page.goBack();
    await expect(page).toHaveURL((u) => u.pathname === PAGE);
    await expect(page.locator('h1').first()).toHaveText(TITLE);
  });

  test('a job row and an employee row link to their catalogued detail pages, and back returns here', async ({ page }) => {
    await installQaReads(page);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });
    await tab(page, 'Batch QC').click();

    const jobLink = page.getByRole('link', { name: QA_JOB.id.slice(0, 8), exact: true });
    const empLink = page.getByRole('link', { name: LAB.name, exact: true });
    await expect(jobLink).toHaveAttribute('href', `/ux/jobs/${QA_JOB.id}`);
    await expect(empLink).toHaveAttribute('href', `/ux/people/${LAB.id}`);
    expect(catalogued(`/ux/jobs/${QA_JOB.id}`), 'a job detail lights the All jobs row').toBe(ROUTE_CATALOG.jobs.id);
    expect(catalogued(`/ux/people/${LAB.id}`), 'an employee detail lights the People row').toBe(ROUTE_CATALOG.people.id);

    await jobLink.click();
    await expect(page).toHaveURL((u) => u.pathname === `/ux/jobs/${QA_JOB.id}`);
    await page.goBack();
    await expect(page).toHaveURL((u) => u.pathname === PAGE);
    await expect(page.locator('h1').first()).toHaveText(TITLE);

    await tab(page, 'Batch QC').click();
    await empLink.click();
    await expect(page).toHaveURL((u) => u.pathname === `/ux/people/${LAB.id}`);
    await page.goBack();
    await expect(page).toHaveURL((u) => u.pathname === PAGE);
    await expect(page.locator('h1').first()).toHaveText(TITLE);
  });
});

test.describe('/ux/qa empty vs failed (qa on)', () => {
  test('an empty backend paints the empty lines, and no failure marker', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(PEOPLE, (r) => json(r, []));
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    await expect(page.locator('dl.kv').first().locator('dd').first()).toHaveText('0');
    await tab(page, 'Batch QC').click();
    await expect(page.getByText('No lab-tech or head-brewer staff on record.')).toBeVisible();
    await tab(page, 'Compliance').click();
    await expect(page.getByText('No certifications on record.')).toBeVisible();
    await expect(page.getByText('No certifications expiring within 90 days.')).toBeVisible();
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  for (const [name, read] of [['people', PEOPLE], ['summary', SUMMARY], ['live', LIVE], ['workflows', WORKFLOWS]] as const) {
    test(`a failed ${name} read paints the failure line on every tab, never an empty one`, async ({ page }) => {
      await installQaReads(page);
      await page.route(read, (r) => json(r, { error: `${name} down` }, 503));
      await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

      for (const t of ['Overview', 'Batch QC', 'Compliance', 'Equipment preventive maintenance']) {
        await tab(page, t).click();
        await expect(page.locator(`.catalog ${FAILURE_MARKER}[role=alert]`)).toHaveText("Couldn't load QA data — HTTP 503");
        await expect(page.locator('table.data-table')).toHaveCount(0);
        await expect(page.locator('dl.kv')).toHaveCount(0);
      }
      await expect(page.getByText('No lab-tech or head-brewer staff on record.')).toHaveCount(0);
      await expect(page.getByText('No certifications on record.')).toHaveCount(0);
    });
  }
});
