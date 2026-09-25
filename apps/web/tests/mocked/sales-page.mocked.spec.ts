// /ux/sales — "Sales pipeline" (department sales), every control and
// render state pinned as the page behaves TODAY (page audit 1e9283fc,
// step `test`).
//
// The page is not its own component: App.svelte mounts the generic
// JobsListPage with initialDepartment = ROUTE_CATALOG.sales.department,
// initialStatus "open" and pageTitle "Sales pipeline". So every control
// here is JobsListPage's, shared with /ux/jobs and /ux/service; the
// crawls (route-smoke, outage-crawl, interaction-crawl) reach the route,
// jobs-list-failed-read and jobs-subject-link pin one behaviour each on
// it, and until this spec nothing held the rest.
//
// The inventory, counted from the source (JobsListPage.svelte):
//   buttons  — "Clear ✕" (only while a filter is set), "Start a new Job",
//              "Create Ad Hoc Job" (only when the registry carries
//              `ad-hoc`), three status buttons (Open / Closed / All), and
//              in the form "Create Job" / "Creating…" and "Cancel";
//   controls — the Kind, Status and Subject id filters; in the form the
//              Kind, Subject kind, Subject id, Owner and Title fields;
//   form     — 1, the new-Job form, which POSTs the envelope;
//   links    — 2 per row (the short id to the Job, the Subject cell to
//              the Subject) and the row itself (rowLink, to the Job);
//   reads    — 4 paths: GET /api/jobs?{filters}, GET /api/workflows, the
//              subject autocomplete (one URL per subject kind) and
//              GET /api/people (the owner picker);
//   writes   — 1, POST /api/jobs.
//
// The four gaps the audit filed, each named on the line that pins its
// current behaviour, so the car that changes it edits an expectation
// here instead of passing silently:
//   GAP 1 26280838 the page filtered on kind=sale — FIXED on main by the
//                  423a531d car (2026-09-22): the read carries
//                  department=sales and no kind. The mount test pins it.
//   GAP 2 1b92d018 the code the page filters on now lives in the catalog
//                  (ROUTE_CATALOG.sales.department), which is half of the
//                  item; nothing yet holds that code to the department
//                  registry, and a mocked spec cannot.
//   GAP 3 887c6b42 the page is the generic list: "Work" eyebrow, a Kind
//                  filter offering the WHOLE registry, slugs in the Kind
//                  column.
//   GAP 4 dc06c0fc "Start a new Job" offers every registered kind, not
//                  the department's.
//   (unfiled)      found writing this spec: on open the form defaulted
//                  the subject kind and then offered only that one, so
//                  "Receive an inquiry" (an account kind) was not in the
//                  picker until another kind was chosen first. FIXED by
//                  backlog d0b93b80, which also moved the Subject links
//                  to /ux/, the Subject id placeholder off a brewery id,
//                  and the All count's word; the lines say so.

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { mountPage, settledReads } from './_helpers';
import { FAILURE_MARKER } from './_routes';
import { installSmokeMocks } from './_smokeMocks';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { parseRoute } from '../../src/router';

const PATH = ROUTE_CATALOG.sales.path;
const DEPARTMENT = ROUTE_CATALOG.sales.department ?? '';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const LIST = /\/api\/jobs\?/;
const REGISTRY = /\/api\/workflows$/;
const CREATE = /\/api\/jobs$/;
const ACCOUNTS = /\/api\/people\/accounts\?limit=1000$/;

// ── Fixtures ────────────────────────────────────────────────────────

const step = (kind: string, title_template: string, authority_role: string | null = null) => ({
  kind, title_template, sign_offs_required: authority_role ? [authority_role] : [], authority_role,
});

/// The registry, in the order the server answers it (NOT kind order):
/// the department's two protocols, the ad-hoc row, and one kind Sales
/// does not run — the one the page should never offer (GAP 4).
const WORKFLOWS = [
  {
    kind: 'receive-a-sponsorship', label: 'Receive a sponsorship', category: 'revenue',
    description: 'A sponsor paid; record it and thank them.', subject_kinds: ['custom'],
    steps: [step('trigger', 'Sponsorship received'), step('task', 'Thank the sponsor')],
  },
  {
    kind: 'receive-an-inquiry', label: 'Receive an inquiry', category: 'revenue',
    description: 'Someone asked about the product; answer them.', subject_kinds: ['account'],
    steps: [
      step('trigger', 'Inquiry received'),
      step('task', 'Answer the inquiry'),
      step('sign-off', 'Close the inquiry', 'sales-lead'),
    ],
  },
  {
    kind: 'ad-hoc', label: 'Ad hoc', category: 'operations', description: null,
    subject_kinds: ['custom', 'account'], steps: [step('task', 'Do the thing')],
  },
  {
    kind: 'page-audit', label: 'Page audit', category: 'it', description: 'Audit one page.',
    subject_kinds: ['custom'], steps: [step('task', 'Inventory the page')],
  },
];
const SALES_KINDS: ReadonlySet<string> = new Set(['receive-a-sponsorship', 'receive-an-inquiry']);

const job = (
  id: string, kind: string, title: string, subject: Record<string, string>,
  status: string, priority: string, opened_on: string,
) => ({
  id, kind, title, subject, owner_id: 'emp-001', status, priority, opened_on,
  due_on: null, closed_on: status === 'closed' ? opened_on : null, metadata: {}, tags: [], steps: [],
});

const INQUIRY = job(
  'aaaaaaaa-0000-4000-8000-00000000a001', 'receive-an-inquiry', 'Inquiry from Anchor Pub',
  { subject_kind: 'account', id: 'acct-anchor' }, 'open', 'high', '2026-09-20',
);
/// The one sponsorship the live instance has ever closed (needs_md).
const SPONSORED = job(
  'bbbbbbbb-0000-4000-8000-00000000b002', 'receive-a-sponsorship',
  'Sponsorship: 1.00 USD from an anonymous sponsor',
  { subject_kind: 'custom', id: 'sponsor-anon' }, 'closed', 'standard', '2026-09-17',
);
const SPONSOR_OPEN = job(
  'cccccccc-0000-4000-8000-00000000c003', 'receive-a-sponsorship',
  'Sponsorship: 25.00 USD from Zed Taproom',
  { subject_kind: 'custom', id: 'sponsor-zed' }, 'open', 'standard', '2026-09-22',
);
/// Another department's packet: the server's department join drops it.
const AUDIT = job(
  'dddddddd-0000-4000-8000-00000000d004', 'page-audit', 'Page audit: /ux/sales',
  { subject_kind: 'custom', id: '/ux/sales' }, 'open', 'standard', '2026-09-19',
);
const JOBS = [INQUIRY, SPONSORED, SPONSOR_OPEN, AUDIT];

/// The server's answer to a list read: the department resolves to the
/// kinds whose workflow row declares it, and every other filter is an
/// exact match — so each filter the page sends is visible in the rows.
function answerList(params: URLSearchParams): { data: unknown[]; total: number } {
  const rows = JOBS.filter((j) =>
    (!params.get('department') || SALES_KINDS.has(j.kind)) &&
    (!params.get('status') || j.status === params.get('status')) &&
    (!params.get('kind') || j.kind === params.get('kind')) &&
    (!params.get('subject_id') || j.subject.id === params.get('subject_id')));
  return { data: rows, total: rows.length };
}

const HEADINGS = ['ID', 'Kind', 'Title', 'Subject', 'Status', 'Priority', 'Opened'];
const cells = (j: (typeof JOBS)[number]): string[] => [
  j.id.replace(/-/g, '').slice(-8), j.kind, j.title, j.subject.id!, j.status, j.priority, j.opened_on,
];

/// The shell (installSmokeMocks), the registry above, the list answered
/// by answerList, the account autocomplete and each Job's detail read.
/// A spec re-routes any one after this to change it.
async function installSales(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(REGISTRY, (r) => json(r, WORKFLOWS));
  await page.route(LIST, (r) => json(r, answerList(new URL(r.request().url()).searchParams)));
  await page.route(ACCOUNTS, (r) => json(r, {
    data: [{ id: 'acct-anchor', name: 'Anchor Pub' }, { id: 'acct-zed', name: 'Zed Taproom' }], total: 2,
  }));
  await page.route(/\/api\/jobs\/[0-9a-f-]{36}$/, (r) => {
    const id = new URL(r.request().url()).pathname.split('/').pop();
    const j = JOBS.find((x) => x.id === id) ?? { ...INQUIRY, id, title: 'Created just now' };
    return json(r, j);
  });
}

/// The shell's own non-GET: App.svelte records every route open.
const SHELL_WRITES: ReadonlySet<string> = new Set(['/api/surface-opens']);

type Seen = { lists: URLSearchParams[]; registry: number; accounts: number; writes: Request[] };
function watch(page: Page): Seen {
  const seen: Seen = { lists: [], registry: 0, accounts: 0, writes: [] };
  page.on('request', (req) => {
    const url = new URL(req.url());
    if (!url.pathname.startsWith('/api/')) return;
    if (req.method() !== 'GET') {
      if (!SHELL_WRITES.has(url.pathname)) seen.writes.push(req);
      return;
    }
    if (url.pathname === '/api/jobs' && url.search) seen.lists.push(url.searchParams);
    if (url.pathname === '/api/workflows') seen.registry += 1;
    if (url.pathname === '/api/people/accounts') seen.accounts += 1;
  });
  return seen;
}

const params = (p: URLSearchParams | undefined): Record<string, string> => Object.fromEntries(p ?? []);

/// JobsListPage's own root; the chrome is outside it.
const root = (page: Page) => page.locator('div.catalog.theme-exec');
const header = (page: Page) => root(page).locator('header.exec-header');
const subtitle = (page: Page) => header(page).locator('p');
const filters = (page: Page) => root(page).locator('.job-filters');
const kindFilter = (page: Page) => filters(page).locator('label').first().locator('select');
const statusFilter = (page: Page) => filters(page).getByLabel('Status');
const subjectFilter = (page: Page) => filters(page).getByLabel('Subject id');
const clear = (page: Page) => filters(page).getByRole('button', { name: /Clear/ });
const statusButtons = (page: Page) => root(page).locator('aside.catalog-filters button');
const rows = (page: Page) => root(page).locator('table.data-table tbody tr');
const titles = (page: Page) => rows(page).locator('td:nth-child(3)');
const form = (page: Page) => root(page).locator('form.new-job-form');
/// The form's first label wraps its Kind select, as the filter bar's does
/// (kindFilter above). A label wrapping a select takes the options into
/// its accessible name, so getByLabel cannot tell "Kind" from "Subject
/// kind" exactly; position can.
const formKind = (page: Page) => form(page).locator('label').first().locator('select');
const startNew = (page: Page) => root(page).getByRole('button', { name: 'Start a new Job' });
const adHocButton = (page: Page) => root(page).getByRole('button', { name: 'Create Ad Hoc Job' });

async function mountSales(page: Page): Promise<void> {
  await mountPage(page, PATH);
  await expect(rows(page)).toHaveCount(2);
}

test.describe('/ux/sales — mount', () => {
  test('one list read for the department, one registry read, no write; header, filters, actions and table as painted', async ({ page }) => {
    const seen = watch(page);
    await installSales(page);
    await mountSales(page);

    expect(await settledReads(page, () => seen.lists.length, 1)).toBe(1);
    // GAP 1 (26280838), fixed on main by the 423a531d car: the read asks
    // for the department — the code the catalog entry carries (GAP 2,
    // 1b92d018: the catalog holds it; nothing holds it to the registry)
    // — and names no kind at all.
    expect(DEPARTMENT).toBe('sales');
    expect(params(seen.lists[0])).toEqual({ department: DEPARTMENT, status: 'open', limit: '200' });
    expect(seen.lists[0]!.has('kind')).toBe(false);
    expect(seen.registry).toBe(1);
    expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);

    // GAP 3 (887c6b42): the generic list's header — the eyebrow is
    // "Work", not the department's name.
    await expect(header(page).locator('.exec-eyebrow')).toHaveText('Work');
    await expect(header(page).locator('h1.exec-title')).toHaveText('Sales pipeline');
    await expect(subtitle(page)).toHaveText('2 open');

    await expect(filters(page).locator('label > span')).toHaveText(['Kind', 'Status', 'Subject id']);
    // GAP 3 (887c6b42): the Kind filter offers the whole registry, in
    // kind order — page-audit included, a kind Sales does not run.
    await expect(kindFilter(page).locator('option')).toHaveText([
      'All kinds', 'ad-hoc', 'page-audit', 'receive-a-sponsorship', 'receive-an-inquiry',
    ]);
    await expect(kindFilter(page)).toHaveValue('');
    await expect(statusFilter(page).locator('option')).toHaveText(['Open', 'Closed', 'All']);
    await expect(statusFilter(page)).toHaveValue('open');
    await expect(subjectFilter(page)).toHaveValue('');
    // d0b93b80: it named a brewery account id.
    await expect(subjectFilter(page)).toHaveAttribute('placeholder', 'An exact subject id');
    await expect(clear(page)).toHaveCount(0);

    await expect(root(page).locator('.job-actions button')).toHaveText(['Start a new Job', 'Create Ad Hoc Job']);
    await expect(form(page)).toHaveCount(0);

    await expect(root(page).locator('aside .filter-label')).toHaveText('Status');
    await expect(statusButtons(page)).toHaveText(['Open', 'Closed', 'All']);
    await expect(statusButtons(page).nth(0)).toHaveAttribute('aria-pressed', 'true');
    await expect(statusButtons(page).nth(1)).toHaveAttribute('aria-pressed', 'false');
    await expect(statusButtons(page).nth(2)).toHaveAttribute('aria-pressed', 'false');

    // The department join: both sales protocols, in the server's order;
    // the page-audit packet never arrives. GAP 3: the Kind column is the
    // slug, not the protocol's label.
    await expect(root(page).locator('thead th')).toHaveText(HEADINGS);
    await expect(rows(page).nth(0).locator('td')).toHaveText(cells(INQUIRY));
    await expect(rows(page).nth(1).locator('td')).toHaveText(cells(SPONSOR_OPEN));
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    // The inventory at mount: 5 buttons (2 actions + 3 status; Clear is
    // hidden), 2 selects, 1 input, 0 forms, 2 links per row, each row
    // itself a link.
    await expect(root(page).locator('button')).toHaveCount(5);
    await expect(root(page).locator('select')).toHaveCount(2);
    await expect(root(page).locator('input')).toHaveCount(1);
    await expect(root(page).locator('form')).toHaveCount(0);
    await expect(root(page).locator('a')).toHaveCount(4);
    await expect(rows(page).nth(0)).toHaveAttribute('role', 'link');
    await expect(rows(page).nth(0)).toHaveAttribute('aria-label', INQUIRY.title);
  });
});

test.describe('/ux/sales — links, and back', () => {
  test('the short id links the Job under the catalogued jobs path; one click lands on it once, and one Back returns', async ({ page }) => {
    await installSales(page);
    await mountSales(page);

    const target = `${ROUTE_CATALOG.jobs.path}/${INQUIRY.id}`;
    const link = rows(page).nth(0).locator('td').first().locator('a');
    await expect(link).toHaveText(cells(INQUIRY)[0]!);
    await expect(link).toHaveAttribute('href', target);
    expect(parseRoute(target)).toEqual({ kind: 'jobDetail', jobId: INQUIRY.id });
    await expect(rows(page).nth(1).locator('td').first().locator('a'))
      .toHaveAttribute('href', `${ROUTE_CATALOG.jobs.path}/${SPONSOR_OPEN.id}`);

    const depth = await page.evaluate(() => window.history.length);
    await link.click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(target);
    await expect(page.locator('h1.exec-title')).toHaveText(INQUIRY.title);
    expect(await page.evaluate(() => window.history.length)).toBe(depth + 1);

    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(page.locator('h1.exec-title')).toHaveText('Sales pipeline');
    await expect(titles(page)).toHaveText([INQUIRY.title, SPONSOR_OPEN.title]);
  });

  test('a click anywhere else on the row, or Enter on it, lands on the same Job; Back returns', async ({ page }) => {
    await installSales(page);
    await mountSales(page);
    const target = `${ROUTE_CATALOG.jobs.path}/${SPONSOR_OPEN.id}`;

    await titles(page).nth(1).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(target);
    await expect(page.locator('h1.exec-title')).toHaveText(SPONSOR_OPEN.title);
    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(rows(page)).toHaveCount(2);

    await rows(page).nth(1).focus();
    await page.keyboard.press('Enter');
    await expect.poll(() => new URL(page.url()).pathname).toBe(target);
    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(page.locator('h1.exec-title')).toHaveText('Sales pipeline');
  });

  // subjectPath answers the catalogued /ux/ spelling. d0b93b80: it
  // answered the router's un-prefixed alias (/accounts/…, /jobs?…),
  // which parses to the same route but put the alias in the address bar.
  test('the Subject cell links each subject to a route the catalog serves; a custom subject lands on its packets, and Back returns', async ({ page }) => {
    await installSales(page);
    await mountSales(page);

    const account = rows(page).nth(0).locator('td').nth(3).locator('a');
    await expect(account).toHaveText('acct-anchor');
    await expect(account).toHaveAttribute('href', `${ROUTE_CATALOG.accounts.path}/acct-anchor`);
    expect(parseRoute(`${ROUTE_CATALOG.accounts.path}/acct-anchor`)).toMatchObject({ kind: 'account', accountId: 'acct-anchor' });

    const custom = rows(page).nth(1).locator('td').nth(3).locator('a');
    await expect(custom).toHaveText('sponsor-zed');
    await expect(custom).toHaveAttribute('href', `${ROUTE_CATALOG.jobs.path}?subject_id=sponsor-zed`);
    expect(parseRoute(ROUTE_CATALOG.jobs.path, '?subject_id=sponsor-zed')).toMatchObject({ kind: 'jobs', jobSubjectId: 'sponsor-zed' });

    await custom.click();
    await expect.poll(() => new URL(page.url()).searchParams.get('subject_id')).toBe('sponsor-zed');
    await expect(page.locator('h1.exec-title')).toHaveText('All jobs');
    await expect(titles(page)).toHaveText([SPONSOR_OPEN.title]);

    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(page.locator('h1.exec-title')).toHaveText('Sales pipeline');
    await expect(titles(page)).toHaveText([INQUIRY.title, SPONSOR_OPEN.title]);
  });
});

test.describe('/ux/sales — the filters', () => {
  test('the status buttons and the Status select re-read with the department kept; Clear goes back to Open; the URL never changes', async ({ page }) => {
    const seen = watch(page);
    await installSales(page);
    await mountSales(page);
    expect(await settledReads(page, () => seen.lists.length, 1)).toBe(1);

    await statusButtons(page).nth(1).click();
    await expect(titles(page)).toHaveText([SPONSORED.title]);
    expect(params(seen.lists.at(-1))).toEqual({ department: DEPARTMENT, status: 'closed', limit: '200' });
    await expect(subtitle(page)).toHaveText('1 closed');
    await expect(statusButtons(page).nth(1)).toHaveAttribute('aria-pressed', 'true');
    await expect(statusButtons(page).nth(0)).toHaveAttribute('aria-pressed', 'false');
    await expect(statusFilter(page)).toHaveValue('closed');
    await expect(clear(page)).toHaveText('Clear ✕');
    await expect(clear(page)).toHaveAttribute('title', 'Clear all filters');

    await statusButtons(page).nth(2).click();
    await expect(rows(page)).toHaveCount(3);
    expect(params(seen.lists.at(-1))).toEqual({ department: DEPARTMENT, limit: '200' });
    // d0b93b80: it read "3 any-status".
    await expect(subtitle(page)).toHaveText('3 in all statuses');
    await expect(statusFilter(page)).toHaveValue('');

    await statusFilter(page).selectOption('open');
    await expect(rows(page)).toHaveCount(2);
    await expect(subtitle(page)).toHaveText('2 open');
    await expect(clear(page)).toHaveCount(0);

    await statusFilter(page).selectOption('closed');
    await expect(titles(page)).toHaveText([SPONSORED.title]);
    await clear(page).click();
    await expect(rows(page)).toHaveCount(2);
    expect(params(seen.lists.at(-1))).toEqual({ department: DEPARTMENT, status: 'open', limit: '200' });
    await expect(statusButtons(page).nth(0)).toHaveAttribute('aria-pressed', 'true');
    await expect(clear(page)).toHaveCount(0);

    // The filters live in page state only here: /ux/sales parses no
    // query, so the page writes none (writesFiltersToUrl is /jobs's).
    expect(new URL(page.url()).pathname).toBe(PATH);
    expect(new URL(page.url()).search).toBe('');
    expect(seen.writes).toHaveLength(0);
  });

  test('the Kind filter narrows within the department, and offers kinds the department does not run', async ({ page }) => {
    const seen = watch(page);
    await installSales(page);
    await mountSales(page);
    await expect(kindFilter(page).locator('option')).toHaveCount(5);

    await kindFilter(page).selectOption('receive-an-inquiry');
    await expect(titles(page)).toHaveText([INQUIRY.title]);
    expect(params(seen.lists.at(-1))).toEqual({
      kind: 'receive-an-inquiry', department: DEPARTMENT, status: 'open', limit: '200',
    });
    // The page's own title stands; the generic "<kind> jobs" does not.
    await expect(header(page).locator('h1.exec-title')).toHaveText('Sales pipeline');
    await expect(subtitle(page)).toHaveText('1 open');
    await expect(clear(page)).toBeVisible();

    // GAP 3 (887c6b42) / GAP 4 (dc06c0fc): another department's kind is
    // offered, and choosing it can only answer empty — truthfully.
    await kindFilter(page).selectOption('page-audit');
    await expect(root(page).locator('p.empty')).toHaveText('No jobs match.');
    expect(params(seen.lists.at(-1))).toEqual({
      kind: 'page-audit', department: DEPARTMENT, status: 'open', limit: '200',
    });
    await expect(subtitle(page)).toHaveText('0 open');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    await clear(page).click();
    await expect(rows(page)).toHaveCount(2);
    await expect(kindFilter(page)).toHaveValue('');
    expect(params(seen.lists.at(-1))).toEqual({ department: DEPARTMENT, status: 'open', limit: '200' });

    // Focusing the filter asks the registry again only after a failure;
    // with the registry loaded it reads nothing.
    await kindFilter(page).focus();
    expect(await settledReads(page, () => seen.registry, 1)).toBe(1);
  });

  test('the Subject id filter narrows by subject; Clear empties it', async ({ page }) => {
    const seen = watch(page);
    await installSales(page);
    await mountSales(page);

    await subjectFilter(page).fill('acct-anchor');
    await expect(titles(page)).toHaveText([INQUIRY.title]);
    expect(params(seen.lists.at(-1))).toEqual({
      department: DEPARTMENT, status: 'open', subject_id: 'acct-anchor', limit: '200',
    });

    await subjectFilter(page).fill('nobody');
    await expect(root(page).locator('p.empty')).toHaveText('No jobs match.');

    await clear(page).click();
    await expect(subjectFilter(page)).toHaveValue('');
    await expect(rows(page)).toHaveCount(2);
    expect(params(seen.lists.at(-1))).toEqual({ department: DEPARTMENT, status: 'open', limit: '200' });
  });
});

test.describe('/ux/sales — empty, loading, and failed reads', () => {
  // The honest state of the live department (needs_md): both protocols
  // at zero open packets.
  test('an empty backend paints "No jobs match." under "0 open" — never the failure marker', async ({ page }) => {
    await installSales(page);
    await page.route(LIST, (r) => json(r, { data: [], total: 0 }));
    await page.route(REGISTRY, (r) => json(r, []));
    await mountPage(page, PATH);

    await expect(root(page).locator('p.empty')).toHaveText('No jobs match.');
    await expect(subtitle(page)).toHaveText('0 open');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(root(page).locator('table')).toHaveCount(0);
    // An empty registry: the Kind filter offers only "All kinds", and
    // with no `ad-hoc` row there is no Ad Hoc button.
    await expect(kindFilter(page).locator('option')).toHaveText(['All kinds']);
    await expect(startNew(page)).toBeVisible();
    await expect(adHocButton(page)).toHaveCount(0);
  });

  test('a pending list read paints "Loading…", then the rows', async ({ page }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => { release = resolve; });
    await installSales(page);
    await page.route(LIST, async (r) => {
      await held;
      await json(r, answerList(new URL(r.request().url()).searchParams));
    });
    await mountPage(page, PATH);

    await expect(root(page).locator('p.empty')).toHaveText('Loading…');
    await expect(header(page).locator('h1.exec-title')).toHaveText('Sales pipeline');
    release();
    await expect(rows(page)).toHaveCount(2);
    await expect(root(page).getByText('Loading…', { exact: true })).toHaveCount(0);
  });

  test('a refused list read is said on the shared marker, and the header counts nothing', async ({ page }) => {
    await installSales(page);
    await page.route(LIST, (r) => json(r, { error: 'jobs down' }, 503));
    await mountPage(page, PATH);

    const failed = page.locator(FAILURE_MARKER);
    await expect(failed).toHaveText("Couldn't load jobs: HTTP 503");
    await expect(failed).toHaveAttribute('role', 'alert');
    await expect(subtitle(page)).toHaveText('Job count unknown — the read failed');
    await expect(root(page).getByText('No jobs match.')).toHaveCount(0);
    await expect(root(page).locator('table')).toHaveCount(0);
  });

  test("a network failure on the list names the browser's own message", async ({ page }) => {
    await installSales(page);
    await page.route(LIST, (r) => r.abort('failed'));
    await mountPage(page, PATH);
    await expect(page.locator(FAILURE_MARKER)).toHaveText("Couldn't load jobs: Failed to fetch");
    await expect(subtitle(page)).toHaveText('Job count unknown — the read failed');
  });

  test('a failed registry read: the filter offers only "All kinds", and opening the form asks once more and says why', async ({ page }) => {
    const seen = watch(page);
    await installSales(page);
    await page.route(REGISTRY, (r) => json(r, { error: 'registry down' }, 503));
    await mountSales(page);

    expect(await settledReads(page, () => seen.registry, 1)).toBe(1);
    await expect(kindFilter(page).locator('option')).toHaveText(['All kinds']);
    await expect(adHocButton(page)).toHaveCount(0);
    // The list is unaffected by the registry's failure.
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    await startNew(page).click();
    await expect(form(page).locator('p.form-error')).toHaveText('HTTP 503');
    expect(await settledReads(page, () => seen.registry, 2)).toBe(2);
    await expect(formKind(page)).toHaveValue('');
    await expect(form(page).getByRole('button', { name: 'Create Job' })).toBeDisabled();
  });
});

test.describe('/ux/sales — the new-Job form', () => {
  test('"Start a new Job" opens the form; it offers every registered kind, and creates the Job the fields describe', async ({ page }) => {
    const seen = watch(page);
    const posted: { body: Record<string, unknown> | null } = { body: null };
    const NEW_ID = 'eeeeeeee-0000-4000-8000-00000000e005';
    await installSales(page);
    await page.route(CREATE, (r) => {
      posted.body = r.request().postDataJSON() as Record<string, unknown>;
      return json(r, { id: NEW_ID }, 201);
    });
    await mountSales(page);

    await startNew(page).click();
    await expect(form(page)).toBeVisible();
    const kind = formKind(page);
    const subjectKind = form(page).getByLabel('Subject kind');
    const subjectId = form(page).getByLabel('Subject id');
    const owner = form(page).getByLabel('Owner');
    const title = form(page).getByLabel('Title (optional)');
    const submit = form(page).getByRole('button', { name: 'Create Job' });

    await expect(form(page).locator('label > span')).toHaveText([
      'Kind', 'Subject kind', 'Subject id', 'Owner', 'Title (optional)',
    ]);
    // With no kind picked the form defaults the subject kind to the
    // first any kind accepts, and the Kind picker narrows to the kinds
    // that take it — but the Subject kind select offers every subject
    // kind the registry names. d0b93b80: it offered ONLY the defaulted
    // one, so `account` (and "Receive an inquiry") could not be reached
    // until some kind accepting both was chosen first. Found writing
    // this spec (page audit 1e9283fc, step `test`).
    await expect(subjectKind).toHaveValue('custom');
    await expect(subjectKind.locator('option')).toHaveText(['— select —', 'custom', 'account']);
    await expect(form(page).locator('small.hint').first()).toHaveText(
      'Filtered to kinds that accept custom subjects (3 of 4)',
    );
    // GAP 4 (dc06c0fc): the picker is the registry, not the department's
    // two protocols — page-audit (IT's) is offered from "Sales pipeline",
    // and of Sales's own two only the sponsorship is.
    await expect(kind.locator('option')).toHaveText([
      '— select —',
      'Receive a sponsorship (receive-a-sponsorship)',
      'Ad hoc (ad-hoc)',
      'Page audit (page-audit)',
    ]);
    await expect(subjectId).toHaveAttribute('placeholder', 'Type the id by hand (no autocomplete for this kind)');
    await expect(owner.locator('option')).toHaveText(['— unassigned —', 'Demo CEO (ceo)']);
    await expect(title).toHaveAttribute('placeholder', 'Defaulted from kind + subject');
    await expect(submit).toBeDisabled();
    await expect(form(page).getByRole('button', { name: 'Cancel' })).toBeEnabled();

    // Choosing `account` straight away offers the inquiry.
    await subjectKind.selectOption('account');
    await expect(kind.locator('option')).toHaveText([
      '— select —', 'Receive an inquiry (receive-an-inquiry)', 'Ad hoc (ad-hoc)',
    ]);
    // d0b93b80: it read "accept a account subject".
    await expect(form(page).locator('small.hint').first()).toHaveText(
      'Filtered to kinds that accept account subjects (2 of 4)',
    );
    await kind.selectOption('receive-an-inquiry');
    await expect(form(page).locator('p.kind-description')).toHaveText(
      'revenue Someone asked about the product; answer them.',
    );
    await expect(form(page).locator('.kind-category')).toHaveText('revenue');
    await expect(form(page).locator('details.step-preview summary')).toHaveText('Step preview · 3 steps');
    const preview = form(page).locator('.step-preview-list li');
    await expect(preview).toHaveCount(3);
    await expect(preview.nth(0).locator('.step-preview-kind')).toHaveText('trigger');
    await expect(preview.nth(0).locator('.step-preview-title')).toHaveText('Inquiry received');
    await expect(preview.nth(2).locator('.step-preview-signoff')).toHaveText('✓ sales-lead');
    await expect(preview.nth(2).locator('.step-preview-signoff')).toHaveAttribute('title', 'Sign-off required');

    // The account autocomplete: one read, its options in the datalist.
    await expect(form(page).locator('datalist option')).toHaveCount(2);
    expect(seen.accounts).toBe(1);
    await expect(subjectId).toHaveAttribute('placeholder', 'Pick from the list or type an id');
    await expect(form(page).locator('label.grow small.hint')).toHaveText('2 account options');

    await subjectId.fill('acct-anchor');
    await expect(submit).toBeEnabled();
    // The owner defaults to the signed-in persona.
    const ownerId = await owner.inputValue();
    const listReads = seen.lists.length;
    await submit.click();

    await expect.poll(() => new URL(page.url()).pathname).toBe(`${ROUTE_CATALOG.jobs.path}/${NEW_ID}`);
    expect(posted.body).toMatchObject({
      kind: 'receive-an-inquiry',
      subject: { subject_kind: 'account', id: 'acct-anchor' },
      title: 'Receive an inquiry — Anchor Pub',
      status: 'open',
      priority: 'standard',
      metadata: {},
      tags: [],
    });
    expect(typeof posted.body?.['opened_on']).toBe('string');
    expect(posted.body?.['owner_id']).toBe(ownerId);
    expect(seen.writes.map((r) => `${r.method()} ${new URL(r.url()).pathname}`)).toEqual(['POST /api/jobs']);
    expect(seen.lists.length).toBe(listReads);
  });

  // A REFUSED write is said in the form, above its own buttons — where
  // the user was looking — and nothing the user typed is lost.
  test('a refused create shows the refusal in the form, keeps the fields, and stays on the page', async ({ page }) => {
    const seen = watch(page);
    await installSales(page);
    await page.route(CREATE, (r) => r.fulfill({
      status: 403, contentType: 'application/json', body: '{"error":"policy denied: create on job"}',
    }));
    await mountSales(page);

    await startNew(page).click();
    await formKind(page).selectOption('receive-a-sponsorship');
    await form(page).getByLabel('Subject id').fill('sponsor-zed');
    await form(page).getByLabel('Title (optional)').fill('Zed sponsors a keg');
    await form(page).getByRole('button', { name: 'Create Job' }).click();

    const refusal = form(page).locator('p.form-error');
    await expect(refusal).toHaveText('HTTP 403: {"error":"policy denied: create on job"}');
    // In the form, directly above its actions.
    await expect(form(page).locator('p.form-error + .form-actions')).toHaveCount(1);
    await expect(form(page).getByRole('button', { name: 'Create Job' })).toBeEnabled();
    await expect(formKind(page)).toHaveValue('receive-a-sponsorship');
    await expect(form(page).getByLabel('Subject id')).toHaveValue('sponsor-zed');
    await expect(form(page).getByLabel('Title (optional)')).toHaveValue('Zed sponsors a keg');
    expect(new URL(page.url()).pathname).toBe(PATH);
    expect(seen.writes).toHaveLength(1);
    expect(seen.writes[0]!.postDataJSON()).toMatchObject({
      kind: 'receive-a-sponsorship', title: 'Zed sponsors a keg',
    });
    // A custom subject is sent as the Custom variant, {custom_kind, ref_id}.
    expect(seen.writes[0]!.postDataJSON().subject).toEqual({
      subject_kind: 'custom', custom_kind: 'custom', ref_id: 'sponsor-zed',
    });
    // The failure is the form's, not the list's.
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(rows(page)).toHaveCount(2);
  });

  test('a create that never reaches the server names the browser\'s message in the form', async ({ page }) => {
    await installSales(page);
    await page.route(CREATE, (r) => r.abort('failed'));
    await mountSales(page);

    await startNew(page).click();
    await formKind(page).selectOption('receive-a-sponsorship');
    await form(page).getByLabel('Subject id').fill('sponsor-new');
    await form(page).getByRole('button', { name: 'Create Job' }).click();
    await expect(form(page).locator('p.form-error')).toHaveText('Failed to fetch');
    await expect(form(page).getByRole('button', { name: 'Create Job' })).toBeEnabled();
    expect(new URL(page.url()).pathname).toBe(PATH);
  });

  test('while the create is in flight the submit reads "Creating…" and both buttons are disabled', async ({ page }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => { release = resolve; });
    await installSales(page);
    await page.route(CREATE, async (r) => {
      await held;
      await json(r, { error: 'late refusal' }, 409);
    });
    await mountSales(page);

    await startNew(page).click();
    await formKind(page).selectOption('receive-a-sponsorship');
    await form(page).getByLabel('Subject id').fill('sponsor-new');
    await form(page).getByRole('button', { name: 'Create Job' }).click();

    await expect(form(page).locator('button[type=submit]')).toHaveText('Creating…');
    await expect(form(page).locator('button[type=submit]')).toBeDisabled();
    await expect(form(page).getByRole('button', { name: 'Cancel' })).toBeDisabled();
    release();
    await expect(form(page).locator('p.form-error')).toHaveText('HTTP 409: {"error":"late refusal"}');
    await expect(form(page).locator('button[type=submit]')).toHaveText('Create Job');
  });

  test('Cancel closes the form and writes nothing', async ({ page }) => {
    const seen = watch(page);
    await installSales(page);
    await mountSales(page);

    await startNew(page).click();
    await form(page).getByLabel('Subject id').fill('half-typed');
    await form(page).getByRole('button', { name: 'Cancel' }).click();
    await expect(form(page)).toHaveCount(0);
    expect(new URL(page.url()).pathname).toBe(PATH);
    expect(new URL(page.url()).search).toBe('');
    expect(seen.writes).toHaveLength(0);

    // Reopening starts clean.
    await startNew(page).click();
    await expect(form(page).getByLabel('Subject id')).toHaveValue('');
  });

  test('"Create Ad Hoc Job" opens the same form with the ad-hoc kind chosen', async ({ page }) => {
    await installSales(page);
    await mountSales(page);

    await adHocButton(page).click();
    await expect(form(page)).toBeVisible();
    await expect(formKind(page)).toHaveValue('ad-hoc');
    await expect(form(page).getByLabel('Subject kind').locator('option')).toHaveText(['— select —', 'custom', 'account']);
    await expect(form(page).locator('details.step-preview summary')).toHaveText('Step preview · 1 steps');
    await expect(form(page).locator('p.kind-description')).toHaveCount(0);
  });
});

test.describe('/ux/sales — a guest', () => {
  test('reads the list; both entry buttons are disabled, and the way to act is named', async ({ page }) => {
    await installSales(page);
    await page.route(/\/api\/people$/, (r) => json(r, []));
    await page.route(/\/api\/session$/, (r) => json(r, { username: 'guest@algedonic.dev', role: 'audit-readonly' }));
    await mountSales(page);

    await expect(startNew(page)).toBeDisabled();
    await expect(adHocButton(page)).toBeDisabled();
    await expect(root(page).locator('.write-gate-note')).toHaveText('Read-only session — sign in to act.');
    await expect(root(page).locator('.write-gate-note a')).toHaveAttribute('href', `/login?next=${encodeURIComponent(PATH)}`);
    // Reading is not a write: the filters stay live.
    await expect(kindFilter(page)).toBeEnabled();
    await expect(statusButtons(page).nth(1)).toBeEnabled();
    await expect(form(page)).toHaveCount(0);
  });
});
