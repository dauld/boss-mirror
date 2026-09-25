// /ux/people — "Employees" (people), every control and render state
// pinned as the page behaves TODAY (page audit 0c0265a3, step `test`).
//
// Six narrower specs already pin one fix each, filed from this audit:
// people-unread-roster-counts (gap 3, the header while loading or
// failed), false-empty's roster pair (gap 4), people-department-filter
// (gap 6), people-status-filter (gap 7), people-row-link (gap 8) and
// people-class-labels (gap 10). None of them clicks the View buttons,
// renders the Hierarchy, types a search, sorts a column, reads a
// Tenure / Skills / Location cell or the certifications subtitle, or
// follows a link and comes back. This spec is the page's whole meaning
// in one place; the six stay, as the regression pins of their fixes.
//
// What the page is (controls_md on the packet, re-read against
// origin/main on 2026-09-25): 2 View buttons, 1 Status button per
// (employee, status) Class plus Unknown when a row has none plus All,
// 1 Department button per department the Status selection admits plus
// All, 8 sortable headers, 1 search input — no form; 1 link per
// rendered employee (the ID cell in List, the name in Hierarchy) and
// the row itself; ONE read, GET /api/people, and ZERO writes, so there
// is no refused write to show, and the watch below pins that the page
// sends none.
//
// Tenure is measured from the app clock and the certifications
// subtitle from a 90-day window on it, so the clock is fixed at NOW.
//
// Lines that pin a FILED gap's current behaviour name the gap and its
// item. They are meant to be edited by the car that fixes it, so the
// fix shows up here as a changed expectation instead of a silently
// passing one:
//   gap 5  c71498fe  Hierarchy ignores the Search, Status and
//                    Department filters it keeps rendering
//   gap 12 6a123f1f  "active employees" is whatever the roster read
//                    returns — a system account counts, agents do not
//   gap 13 9a0e18d5  the certifications subtitle counts the whole
//                    roster, terminated included, and links nowhere;
//                    Location prints the raw location id
// Answered, and pinned as answered: gaps 3, 4, 6, 7, 8, 10 (above).
// Gaps 2 and 9 closed as duplicates, gap 11 (salary on the roster
// read) landed server-side as 8cdad84c, gap 1 is design 36b79159.

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { mountPage, settledReads } from './_helpers';
import { DEPARTMENT_CLASSES, installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { parseRoute } from '../../src/router';

const PATH = '/ux/people';
const NOW = new Date('2026-09-25T12:00:00Z');

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const ROSTER = /\/api\/people$/;

type Cert = Readonly<{ name: string; issuing_body: string; issued_on: string; expires_on: string | null }>;
const cert = (name: string, expires_on: string | null): Cert =>
  ({ name, issuing_body: 'Brewers Guild', issued_on: '2024-01-01', expires_on });

function emp(
  id: string, name: string, role: string, department: string | null, status: string,
  hire_date: string, manager_id: string | null, skills: string[], certifications: Cert[],
  location = 'loc-hq',
): Record<string, unknown> {
  return {
    id, name, email: `${name.split(' ')[0]!.toLowerCase()}@algedonic.test`, role, department,
    skill_level: null, skills, hire_date, location, manager_id,
    employment_type: 'full-time', status, certifications,
  };
}

/// Five employees: three active (a CEO and two direct reports, one of
/// whom manages an on-leave report), one on leave, one terminated.
/// Certifications: three expire inside NOW + 90 days (2026-12-24) —
/// the CEO's, the on-leave report's and the TERMINATED one's — one
/// expires after it, and one has already expired.
const ADA = emp('emp-001', 'Ada Brewer', 'ceo', 'executive', 'active', '2020-03-01', null,
  ['brewing', 'sensory'], [cert('Cicerone', '2026-10-15')]);
const BO = emp('emp-002', 'Bo Cellar', 'head-of-sales', 'sales', 'active', '2023-06-01', 'emp-001',
  ['sales'], [cert('Forklift', '2027-06-01')], 'loc-taproom');
const CY = emp('emp-003', 'Cy Tapp', 'brewer', 'it', 'active', '2025-01-15', 'emp-001',
  [], [cert('Food safety', '2026-01-01')]);
const DI = emp('emp-004', 'Di Leave', 'brewer', 'sales', 'on-leave', '2022-01-10', 'emp-002',
  ['packaging'], [cert('Food safety', '2026-11-01')]);
const ED = emp('emp-005', 'Ed Gone', 'brewer', 'it', 'terminated', '2019-01-07', 'emp-001',
  [], [cert('Cicerone', '2026-10-01')], 'loc-remote-default');
const PEOPLE = [ADA, BO, CY, DI, ED];

function klass(member_attribute: string, code: string, display_name: string, sort_order: number) {
  return {
    subject_kind: 'employee', code, display_name, parent_code: null,
    member_attribute, metadata: {}, sort_order, retired_at: null,
  };
}

const CLASSES = [
  klass('role', 'ceo', 'CEO', 1),
  klass('role', 'head-of-sales', 'Head of sales', 2),
  klass('role', 'brewer', 'Brewer', 3),
  ...DEPARTMENT_CLASSES,
  klass('status', 'active', 'Active', 10),
  klass('status', 'on-leave', 'On Leave', 20),
  klass('status', 'terminated', 'Terminated', 30),
];

/// The shell, the registry, the fixed clock, then the page's one read.
/// Given a function the read is routed to it; anything else is its 200
/// body. The detail page's own read of one employee answers from the
/// same fixtures, so a followed link lands on a rendered page.
async function install(page: Page, roster: unknown): Promise<void> {
  await page.clock.setFixedTime(NOW);
  await installSmokeMocks(page);
  await page.route(/\/api\/classes(\?|$)/, (r) => json(r, CLASSES));
  await page.route(/\/api\/people\/emp-[^/]+$/, (r) => {
    const id = new URL(r.request().url()).pathname.split('/').pop();
    const row = PEOPLE.find((e) => e.id === id);
    return row ? json(r, row) : json(r, 'not found', 404);
  });
  await page.route(ROSTER, (r) =>
    typeof roster === 'function' ? (roster as (r: Route) => Promise<void>)(r) : json(r, roster));
}

/// The shell's own non-GET: App.svelte records every route open
/// (shell/surface-opens.ts). It is the chrome's write, not this page's.
const SHELL_WRITES: ReadonlySet<string> = new Set(['/api/surface-opens']);

function watch(page: Page): { reads: string[]; writes: Request[] } {
  const seen = { reads: [] as string[], writes: [] as Request[] };
  page.on('request', (req) => {
    const url = new URL(req.url());
    if (!url.pathname.startsWith('/api/')) return;
    if (req.method() !== 'GET') {
      if (!SHELL_WRITES.has(url.pathname)) seen.writes.push(req);
      return;
    }
    if (url.pathname === '/api/people') seen.reads.push(url.pathname);
  });
  return seen;
}

const list = (page: Page) => page.locator('section.list-section');
const empty = (page: Page) => list(page).locator('p.empty');
const filters = (page: Page) => page.locator('aside.catalog-filters');
const group = (page: Page, label: string) =>
  page.locator('.filter-group').filter({
    has: page.locator('.filter-label', { hasText: new RegExp(`^${label}$`) }),
  });
const button = (page: Page, groupLabel: string, name: string) =>
  group(page, groupLabel).getByRole('button', { name, exact: true });
/// The page's own search box — the shell's bar carries a second one.
const searchbox = (page: Page) => filters(page).locator('input[type="search"]');
const bodyRows = (page: Page) => page.locator('table.data-table tbody tr');
const headers = (page: Page) => page.locator('table.data-table thead th');

/// Every visible row's cells, as the record spells them (textContent:
/// the status plate is set in caps by CSS, which innerText would read).
async function table(page: Page): Promise<string[][]> {
  return bodyRows(page).evaluateAll((rows) =>
    rows.map((row) => Array.from(row.children).map((td) => (td.textContent ?? '').trim())),
  );
}

/// The BOSS ID column, top to bottom.
const ids = (page: Page) => page.locator('table.data-table tbody td.mono');

async function expectHeader(page: Page, title: string, subtitle: string): Promise<void> {
  await expect(page.locator('.exec-eyebrow')).toHaveText('People');
  await expect(page.locator('h1.exec-title')).toHaveText(title);
  await expect(page.locator('header.exec-header p')).toHaveText(subtitle);
}

const READ_TITLE = '3 active employees';
const READ_SUBTITLE = '3 certifications expiring in 90 days';

async function open(page: Page): Promise<void> {
  await install(page, PEOPLE);
  await mountPage(page, PATH, { titleMatch: /3 active employees/ });
}

test.describe('/ux/people — the roster, read', () => {
  test('the page renders every column, count and word, from one read and no writes', async ({ page }) => {
    const seen = watch(page);
    await open(page);

    // Gap 12 (6a123f1f): the headcount is the read's active rows —
    // whoever the roster holds, a system account included.
    // Gap 13 (9a0e18d5): three certifications fall inside the window,
    // and one of them is the TERMINATED employee's.
    await expectHeader(page, READ_TITLE, READ_SUBTITLE);
    await expect(filters(page).locator('.filter-label')).toHaveText(['View', 'Search', 'Status', 'Department']);
    await expect(searchbox(page)).toHaveAttribute('placeholder', 'Name, email, role…');
    await expect(filters(page).getByRole('button')).toHaveText([
      'List', 'Hierarchy',
      'Active (3)', 'On Leave (1)', 'Terminated (1)', 'All (5)',
      'All (3)', 'Executive (1)', 'IT (1)', 'Sales (1)',
    ]);
    // Pressed: List, Active and the Department's All — the landing view.
    await expect(filters(page).locator('button[aria-pressed="true"]')).toHaveText(['List', 'Active (3)', 'All (3)']);

    await expect(headers(page)).toHaveText([
      'BOSS ID', 'Name', 'Role', 'Department ↑', 'Tenure', 'Skills', 'Location', 'Status',
    ]);
    await expect(headers(page).nth(3)).toHaveAttribute('aria-sort', 'ascending');
    // Default order: department code, then name within it. Tenure is
    // (NOW - hire_date) / 365 days, to one place. Gap 13 (9a0e18d5):
    // Location is the raw location id.
    expect(await table(page)).toEqual([
      ['emp-001', 'Ada Brewer', 'CEO', 'Executive', '6.6y', '2', 'loc-hq', 'active'],
      ['emp-003', 'Cy Tapp', 'Brewer', 'IT', '1.7y', '0', 'loc-hq', 'active'],
      ['emp-002', 'Bo Cellar', 'Head of sales', 'Sales', '3.3y', '1', 'loc-taproom', 'active'],
    ]);

    // Two reads of the one path: the page's, and the shell's own
    // (libs/web-kit/src/session/session.svelte.ts resolves the signed-in
    // employee from the same roster). A third would be the page reading
    // twice.
    expect(await settledReads(page, () => seen.reads.length, 2)).toBe(2);
    expect(seen.writes).toHaveLength(0);
  });

  test('the Status buttons narrow the rows to their status, and All shows every row', async ({ page }) => {
    await open(page);

    await button(page, 'Status', 'On Leave (1)').click();
    await expect(ids(page)).toHaveText(['emp-004']);
    await expect(group(page, 'Status').locator('button[aria-pressed="true"]')).toHaveText(['On Leave (1)']);
    // The Department buttons count what the Status selection admits.
    await expect(group(page, 'Department').getByRole('button')).toHaveText(['All (1)', 'Sales (1)']);

    await button(page, 'Status', 'Terminated (1)').click();
    await expect(ids(page)).toHaveText(['emp-005']);
    expect((await table(page))[0]).toEqual(['emp-005', 'Ed Gone', 'Brewer', 'IT', '7.7y', '0', 'loc-remote-default', 'terminated']);

    await button(page, 'Status', 'All (5)').click();
    await expect(ids(page)).toHaveText(['emp-001', 'emp-003', 'emp-005', 'emp-002', 'emp-004']);
    await expect(group(page, 'Department').getByRole('button')).toHaveText([
      'All (5)', 'Executive (1)', 'IT (2)', 'Sales (2)',
    ]);
    // A status filter narrows the rows only; the header counts the roster.
    await expectHeader(page, READ_TITLE, READ_SUBTITLE);

    await button(page, 'Status', 'Active (3)').click();
    await expect(ids(page)).toHaveText(['emp-001', 'emp-003', 'emp-002']);
  });

  test('the Department buttons narrow the rows, compose with Status, and All restores them', async ({ page }) => {
    await open(page);

    await button(page, 'Department', 'Sales (1)').click();
    await expect(ids(page)).toHaveText(['emp-002']);
    await expect(group(page, 'Department').locator('button[aria-pressed="true"]')).toHaveText(['Sales (1)']);

    // Status All under Sales: the on-leave Sales row joins it.
    await button(page, 'Status', 'All (5)').click();
    await expect(ids(page)).toHaveText(['emp-002', 'emp-004']);
    await expect(button(page, 'Department', 'Sales (2)')).toHaveAttribute('aria-pressed', 'true');

    // Terminated under Sales: no row, and the Sales button stays (at
    // zero) so the filter that empties the table can still be seen.
    await button(page, 'Status', 'Terminated (1)').click();
    await expect(bodyRows(page)).toHaveCount(0);
    await expect(empty(page)).toHaveText('No employees match those filters.');
    await expect(group(page, 'Department').getByRole('button')).toHaveText(['All (1)', 'IT (1)', 'Sales (0)']);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    await button(page, 'Department', 'All (1)').click();
    await expect(ids(page)).toHaveText(['emp-005']);
  });

  test('the search narrows on id, name, email and role label, case-insensitively, and sends nothing', async ({ page }) => {
    const seen = watch(page);
    await open(page);
    const before = await settledReads(page, () => seen.reads.length, 1);

    const search = searchbox(page);
    await search.fill('cellar');
    await expect(ids(page)).toHaveText(['emp-002']);
    await search.fill('EMP-003');
    await expect(ids(page)).toHaveText(['emp-003']);
    await search.fill('ada@algedonic');
    await expect(ids(page)).toHaveText(['emp-001']);
    // The role's registry label, not its code: 'head of' matches
    // "Head of sales"; the code head-of-sales has hyphens.
    await search.fill('head of');
    await expect(ids(page)).toHaveText(['emp-002']);
    // The Status selection still applies: Di Leave is on leave.
    await search.fill('di leave');
    await expect(bodyRows(page)).toHaveCount(0);
    await expect(empty(page)).toHaveText('No employees match those filters.');
    await button(page, 'Status', 'All (5)').click();
    await expect(ids(page)).toHaveText(['emp-004']);
    await search.fill('');
    await expect(bodyRows(page)).toHaveCount(5);

    // Client-side only: no read beyond the mount's, no write.
    expect(seen.reads).toHaveLength(before);
    expect(seen.writes).toHaveLength(0);
  });

  test('every column header sorts, from the mouse and the keyboard, and says which way', async ({ page }) => {
    await open(page);
    await button(page, 'Status', 'All (5)').click();
    const th = (name: string) => headers(page).filter({ hasText: new RegExp(`^\\s*${name}`) });

    // Each column's first click, with its default direction: Tenure and
    // Skills open descending (most first), the rest ascending.
    const orders: ReadonlyArray<readonly [string, string, ReadonlyArray<string>]> = [
      ['BOSS ID', 'ascending', ['emp-001', 'emp-002', 'emp-003', 'emp-004', 'emp-005']],
      ['Name', 'ascending', ['emp-001', 'emp-002', 'emp-003', 'emp-004', 'emp-005']],
      ['Role', 'ascending', ['emp-003', 'emp-004', 'emp-005', 'emp-001', 'emp-002']],
      ['Tenure', 'descending', ['emp-005', 'emp-001', 'emp-004', 'emp-002', 'emp-003']],
      ['Skills', 'descending', ['emp-001', 'emp-002', 'emp-004', 'emp-003', 'emp-005']],
      ['Location', 'ascending', ['emp-001', 'emp-003', 'emp-004', 'emp-005', 'emp-002']],
      ['Status', 'ascending', ['emp-001', 'emp-002', 'emp-003', 'emp-004', 'emp-005']],
      ['Department', 'ascending', ['emp-001', 'emp-003', 'emp-005', 'emp-002', 'emp-004']],
    ];
    for (const [name, dir, order] of orders) {
      await th(name).click();
      await expect(th(name)).toHaveAttribute('aria-sort', dir);
      await expect(th(name)).toHaveText(`${name} ${dir === 'ascending' ? '↑' : '↓'}`);
      await expect(page.locator('table.data-table thead th[aria-sort]')).toHaveCount(1);
      await expect(ids(page)).toHaveText([...order]);
    }

    // A second click on the active column flips it.
    await th('Department').click();
    await expect(th('Department')).toHaveAttribute('aria-sort', 'descending');
    await expect(ids(page)).toHaveText(['emp-004', 'emp-002', 'emp-005', 'emp-003', 'emp-001']);

    // Enter and Space on a focused header do what a click does.
    await th('Name').focus();
    await page.keyboard.press('Enter');
    await expect(th('Name')).toHaveAttribute('aria-sort', 'ascending');
    await page.keyboard.press(' ');
    await expect(th('Name')).toHaveAttribute('aria-sort', 'descending');
    await expect(ids(page)).toHaveText(['emp-005', 'emp-004', 'emp-003', 'emp-002', 'emp-001']);
  });

  test('Hierarchy draws the active employees as a tree under their managers, and List returns', async ({ page }) => {
    await open(page);

    await button(page, 'View', 'Hierarchy').click();
    await expect(button(page, 'View', 'Hierarchy')).toHaveAttribute('aria-pressed', 'true');
    await expect(page.locator('table.data-table')).toHaveCount(0);

    // One root, two reports sorted by name. Di Leave reports to Bo but
    // is on leave, so Bo's card shows no reports.
    const cards = list(page).locator('.org-card');
    await expect(cards).toHaveCount(3);
    await expect(cards.nth(0)).toHaveText(/^\s*Ada Brewer\s*CEO\s*2 reports\s*$/);
    await expect(cards.nth(1)).toHaveText(/^\s*Bo Cellar\s*Head of sales\s*$/);
    await expect(cards.nth(2)).toHaveText(/^\s*Cy Tapp\s*Brewer\s*$/);
    await expect(list(page).locator('ul.org-tree > li > .org-node > ul.org-children > li')).toHaveCount(2);
    await expect(list(page).getByRole('link')).toHaveText(['Ada Brewer', 'Bo Cellar', 'Cy Tapp']);

    // Gap 5 (c71498fe): the filters stay rendered and pressable in this
    // view and change nothing in it.
    await button(page, 'Status', 'Terminated (1)').click();
    await button(page, 'Department', 'IT (1)').click();
    await searchbox(page).fill('nobody');
    await expect(cards).toHaveCount(3);

    await button(page, 'View', 'List').click();
    await expect(button(page, 'View', 'List')).toHaveAttribute('aria-pressed', 'true');
    await expect(ids(page)).toHaveCount(0);
    await expect(empty(page)).toHaveText('No employees match those filters.');
    await searchbox(page).fill('');
    await expect(ids(page)).toHaveText(['emp-005']);
  });
});

test.describe('/ux/people — links and back', () => {
  test('the ID link opens the employee page, and back returns to the roster', async ({ page }) => {
    await open(page);

    // The list route is the People tab's catalog entry; the link's
    // target is the router's employee route, whose pattern
    // /ux/people/:empId surface-opens.test.ts pins. The catalog lists
    // no detail entry for it (the same shape as /ux/vendors, e9c8e28d).
    expect(ROUTE_CATALOG.people.path).toBe(PATH);
    expect(ROUTE_CATALOG.people.label).toBe('Employees');
    expect(ROUTE_CATALOG.people.app).toBe('people');
    expect(parseRoute('/ux/people/emp-001')).toMatchObject({ kind: 'employee', empId: 'emp-001' });
    const catalogued = Object.values(ROUTE_CATALOG).map((entry) => entry.path);
    expect(catalogued).toContain(PATH);
    expect(catalogued.filter((p) => p.startsWith(`${PATH}/`))).toEqual([]);

    const links = list(page).getByRole('link', { name: /^emp-\d+$/ });
    await expect(links).toHaveText(['emp-001', 'emp-003', 'emp-002']);
    for (const id of ['emp-001', 'emp-003', 'emp-002']) {
      await expect(list(page).getByRole('link', { name: id, exact: true })).toHaveAttribute('href', `${PATH}/${id}`);
    }

    await list(page).getByRole('link', { name: 'emp-003', exact: true }).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(`${PATH}/emp-003`);
    await expect(page.locator('h1.detail-title')).toHaveText('Cy Tapp');

    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expectHeader(page, READ_TITLE, READ_SUBTITLE);
    await expect(ids(page)).toHaveText(['emp-001', 'emp-003', 'emp-002']);
  });

  test('a Hierarchy name opens the employee page, and back returns to the roster in List view', async ({ page }) => {
    await open(page);
    await button(page, 'View', 'Hierarchy').click();

    const bo = list(page).getByRole('link', { name: 'Bo Cellar', exact: true });
    await expect(bo).toHaveAttribute('href', `${PATH}/emp-002`);
    await bo.click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(`${PATH}/emp-002`);
    await expect(page.locator('h1.detail-title')).toHaveText('Bo Cellar');

    // The view is page state, not the URL: back remounts the roster in
    // its landing view, List.
    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(button(page, 'View', 'List')).toHaveAttribute('aria-pressed', 'true');
    await expect(ids(page)).toHaveText(['emp-001', 'emp-003', 'emp-002']);
  });
});

test.describe('/ux/people — empty and failed reads never paint alike', () => {
  test('an empty roster is counted as empty and says no employee matches', async ({ page }) => {
    await install(page, []);
    await mountPage(page, PATH, { titleMatch: /0 active employees/ });

    // A roster that was read and is empty IS empty: its zeros are facts.
    await expectHeader(page, '0 active employees', '0 certifications expiring in 90 days');
    await expect(filters(page).getByRole('button')).toHaveText([
      'List', 'Hierarchy', 'Active (0)', 'On Leave (0)', 'Terminated (0)', 'All (0)', 'All (0)',
    ]);
    await expect(empty(page)).toHaveText('No employees match those filters.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(page.locator('table.data-table')).toHaveCount(0);

    await button(page, 'View', 'Hierarchy').click();
    await expect(empty(page)).toHaveText('No leadership rooted org chart yet.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('a roster with no active employee draws no tree, and the list still shows the others', async ({ page }) => {
    await install(page, [DI, ED]);
    await mountPage(page, PATH, { titleMatch: /0 active employees/ });

    await expectHeader(page, '0 active employees', '2 certifications expiring in 90 days');
    await button(page, 'View', 'Hierarchy').click();
    await expect(empty(page)).toHaveText('No leadership rooted org chart yet.');
    await button(page, 'View', 'List').click();
    await button(page, 'Status', 'All (2)').click();
    await expect(ids(page)).toHaveText(['emp-005', 'emp-004']);
  });

  // The line is the page's words verbatim: a status names the HTTP code;
  // an unreachable server is the browser's own message (Chromium's);
  // a body that is not JSON is the parser's, whose wording is V8's and
  // so is matched only as far as the page's own prefix.
  const FAILURES: ReadonlyArray<readonly [string, (r: Route) => Promise<void>, string | RegExp]> = [
    ['refused (500)', (r) => json(r, 'people store down', 500), "Couldn't load the roster — people HTTP 500"],
    ['forbidden (403)', (r) => json(r, 'forbidden', 403), "Couldn't load the roster — people HTTP 403"],
    ['unreachable', (r) => r.abort('connectionrefused'), "Couldn't load the roster — Failed to fetch"],
    ['not JSON', (r) => r.fulfill({ status: 200, contentType: 'application/json', body: 'not json' }), /^\s*Couldn't load the roster — \S.*\S\s*$/],
  ];
  for (const [how, answer, line] of FAILURES) {
    test(`a roster read that is ${how} paints the failure line, never the empty one`, async ({ page }) => {
      await install(page, answer);
      await mountPage(page, PATH);

      const failed = list(page).locator(`p${FAILURE_MARKER}[role=alert]`);
      await expect(failed).toHaveText(line);
      await expect(page.getByText('No employees match those filters.')).toHaveCount(0);
      await expect(page.locator('table.data-table')).toHaveCount(0);
      await expectHeader(page, 'Active employees', 'Counts unknown: the roster did not load');
      await expect(filters(page).getByRole('button', { name: /\(\d+\)/ })).toHaveCount(0);

      // The failure outranks the view: Hierarchy says the same thing,
      // not "No leadership rooted org chart yet."
      await button(page, 'View', 'Hierarchy').click();
      await expect(failed).toHaveText(line);
      await expect(page.getByText('No leadership rooted org chart yet.')).toHaveCount(0);
    });
  }
});
