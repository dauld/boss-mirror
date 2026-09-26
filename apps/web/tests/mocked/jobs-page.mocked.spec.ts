// /ux/jobs — "All jobs" (department it, catalogued under app home),
// every control and render state pinned as the page behaves TODAY
// (page audit 473f4f92, step `test`).
//
// Six narrower specs already pin one claim each, and this one does not
// repeat them: jobs-list-failed-read (the failure line's words on all
// three routes), jobs-kinds-failed-read (the registry read is held, not
// looped), jobs-filter-survives-reload (the Status buttons write the
// URL), jobs-deep-link-all-statuses (status= means all),
// jobs-status-filters-are-the-vocabulary (Open / Closed / All) and
// jobs-subject-link (a custom Subject's href). Nothing before this spec
// rendered a row with its columns, chose a Kind, typed a Subject id,
// opened the new-job form, sent or refused its one write, followed the
// ID link or the row and came back, or read the page as a guest.
//
// The page, counted (JobsListPage.svelte, on the tree this car was cut
// from). It is one component mounted on three routes — /ux/service and
// /ux/sales mount it with a department and a title — and every line
// below is theirs too, except the URL writes (only /ux/jobs has them):
//   links    — 2 per row (the short ID -> /ux/jobs/{id}; the Subject ->
//              its page, or its own packets for any kind with no page)
//              and the row itself, which goes where the ID link goes;
//   buttons  — 8: Clear ✕, Start a new Job, Create Ad Hoc Job (only when
//              the registry carries `ad-hoc`), the Status aside's Open /
//              Closed / All, Create Job, Cancel;
//   inputs   — 3 filters (Kind, Status, Subject id) and 5 form fields
//              (Kind, Subject kind, Subject id with a datalist, Owner,
//              Title), plus the Step preview disclosure;
//   reads    — 4 call sites: the list (`/api/jobs?…&limit=200`), the
//              Workflow registry, the people roster (the Owner picker),
//              and the subject autocomplete (one path per subject kind);
//   writes   — 1: POST /api/jobs.
//
// Lines that pin a FILED gap's current behaviour name the gap and its
// item, and are meant to be edited by the car that answers it, so the
// answer shows up here as a changed expectation rather than a silently
// passing one:
//   gap 3  3b1ec06e  a failed registry read leaves Kind looking valid
//   gap 5  ce8f634a  a failed people read leaves Owner looking empty
//   gap 6  6c9672c2  owner_id / kind_prefix narrow with no visible sign
//   gap 10 8708447c  no column for the step a packet waits at, its
//                    holder, or its closed date
//   gap 11 75d1b902  no department filter, column or link
// Answered on main before this spec, each pinned by its own spec named
// above: gap 2 4af37dd8, gap 4 e98cabd0, gap 7 03e198e5, gap 8 45ca0f89,
// gap 9 3c3dc8f3. Answered here: gap 1 d1310776 (200 rows, no offset,
// no notice when total was larger), pinned under "the pager". This spec also found seven defects the audit did not
// list, pinned here as UNFILED when it landed; backlog d0b93b80 fixed
// all seven, and each line that pinned one now pins the fix and names
// that item: Cancel on a deep-linked form reopening it, a Subject link
// that did not remount the list, a deep link's subject_id filtering the
// list behind the form, the account list read twice, Subject kind held
// to its first value, unprefixed Subject paths, and "0 open" above
// "Loading…". The deep link's subject_id fix left its `kind` a list
// filter; backlog 3f5cce16 made it the form's Kind, pinned beside it.

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { mountPage, settledReads } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { parseRoute } from '../../src/router';

const PATH = ROUTE_CATALOG.jobs.path;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const EMP = {
  id: 'emp-001', name: 'Demo CEO', email: 'ceo@demo', role: 'ceo', department: 'exec',
  hire_date: '2020-01-01', status: 'active', location: 'HQ', employment_type: 'full-time',
  skills: [], certifications: [],
};
const BREWER = { ...EMP, id: 'emp-002', name: 'Rhea Okafor', email: 'rhea@demo', role: 'brewer' };

/// Three Workflow rows: one with a description, a category and a step
/// that needs a sign-off (the preview's every part); `ad-hoc`, which
/// is what makes the Create Ad Hoc Job button render, and the only
/// kind here that accepts an account; and one whose label IS its kind,
/// which the form lists bare.
const REGISTRY = [
  {
    kind: 'page-audit', label: 'Page audit', description: 'Read a page against its department.',
    category: 'quality', subject_kinds: ['custom'],
    steps: [
      { kind: 'task', title_template: 'Inventory the page', sign_offs_required: [], authority_role: null },
      { kind: 'sign-off', title_template: 'Review the words', sign_offs_required: ['platform-admin'], authority_role: 'platform-admin' },
    ],
  },
  { kind: 'ad-hoc', label: 'Ad hoc', description: null, category: null, subject_kinds: ['account', 'custom'], steps: [] },
  { kind: 'backlog-item', label: 'backlog-item', description: null, category: null, subject_kinds: ['custom'], steps: null },
];

const J1 = '11111111-1111-4111-8111-00000000aa01';
const J2 = '22222222-2222-4222-8222-00000000bb02';
const J3 = '33333333-3333-4333-8333-00000000cc03';

const job = (
  id: string, kind: string, subject: Record<string, unknown>, title: string,
  priority: string, opened_on: string, extra: Record<string, unknown> = {},
) => ({
  id, kind, subject, title, owner_id: 'emp-001', status: 'open', priority, opened_on,
  due_on: null, closed_on: null, metadata: {}, tags: [], ...extra,
});

/// Three open packets, one per Subject shape the page links differently:
/// a custom Subject (every live packet's), an account, and an asset. The
/// first carries an ACTIVE step with a holder, which the list fetches
/// and never shows (gap 10).
const ROWS = [
  job(J1, 'page-audit', { subject_kind: 'custom', id: '/ux/jobs' }, 'Page audit: /ux/jobs', 'standard', '2026-09-19', {
    steps: [{ id: 's1', job_id: J1, kind: 'task', title: 'test', status: 'active', assignee_id: 'agent-claude', sort_order: 3, blocked_by: [], completed_on: null, metadata: {} }],
  }),
  job(J2, 'ad-hoc', { subject_kind: 'account', id: 'acc-1' }, 'Call the taproom', 'urgent', '2026-09-23'),
  job(J3, 'backlog-item', { subject_kind: 'asset', id: 'ast-9' }, 'Replace the chiller', 'standard', '2026-09-24'),
];

/// The live instance's open count when the audit measured it (286, of
/// which the page could show 200). Three rows stand in for the 200.
const LIVE_TOTAL = 286;

const ACCOUNTS = { data: [{ id: 'acc-1', name: 'Cascade Taproom' }, { id: 'acc-2', name: 'Prairie Pub' }], total: 40 };

const CREATED = '44444444-4444-4444-8444-00000000dd04';

// ---------------------------------------------------------------------------
// The backend
// ---------------------------------------------------------------------------

type Handler = (r: Route) => Promise<void>;
type Backend = Readonly<{
  list?: Handler;
  registry?: Handler;
  post?: Handler;
  accounts?: Handler;
  session?: Record<string, unknown>;
}>;

type Seen = { list: URLSearchParams[]; registry: number; people: number; accounts: number; writes: Request[] };

/// The shell's own non-GET: App.svelte records every route open. It is
/// the chrome's write, not this page's.
const SHELL_WRITES: ReadonlySet<string> = new Set(['/api/surface-opens']);

const SIGNED_IN = { username: 'ceo@demo', employee_id: 'emp-001', role: 'ceo' };

/// The shell (installSmokeMocks), then the page's reads and its write.
/// The list answers ROWS under LIVE_TOTAL unless told otherwise; a
/// detail read answers the matching row (or the created one), so a
/// followed link renders its packet.
async function install(page: Page, b: Backend = {}): Promise<Seen> {
  const seen: Seen = { list: [], registry: 0, people: 0, accounts: 0, writes: [] };
  page.on('request', (req) => {
    const url = new URL(req.url());
    if (!url.pathname.startsWith('/api/')) return;
    if (req.method() !== 'GET' && !SHELL_WRITES.has(url.pathname)) seen.writes.push(req);
  });
  await installSmokeMocks(page);
  await page.route(/\/api\/people$/, (r) => {
    seen.people += 1;
    return json(r, [EMP, BREWER]);
  });
  await page.route(/\/api\/session$/, (r) => json(r, b.session ?? SIGNED_IN));
  await page.route(/\/api\/workflows$/, (r) => {
    seen.registry += 1;
    return b.registry ? b.registry(r) : json(r, REGISTRY);
  });
  await page.route(/\/api\/people\/accounts\?/, (r) => {
    seen.accounts += 1;
    return b.accounts ? b.accounts(r) : json(r, ACCOUNTS);
  });
  await page.route(/\/api\/jobs\/[^/?]+$/, (r) => {
    const id = decodeURIComponent(new URL(r.request().url()).pathname.split('/').pop() ?? '');
    const row = ROWS.find((j) => j.id === id)
      ?? (id === CREATED ? job(CREATED, 'page-audit', { subject_kind: 'custom', id: '/ux/vendors' }, 'Page audit — /ux/vendors', 'standard', '2026-09-25') : null);
    return row ? json(r, { steps: [], ...row }) : json(r, 'not found', 404);
  });
  await page.route(/\/api\/jobs(\?|$)/, (r) => {
    if (r.request().method() === 'POST') {
      return b.post ? b.post(r) : json(r, { id: CREATED }, 201);
    }
    seen.list.push(new URL(r.request().url()).searchParams);
    return b.list ? b.list(r) : json(r, { data: ROWS, total: LIVE_TOTAL });
  });
  return seen;
}

// ---------------------------------------------------------------------------
// Locators and readers
// ---------------------------------------------------------------------------

const filterBar = (page: Page) => page.locator('.job-filters');
const filter = (page: Page, label: string) =>
  filterBar(page).locator(`label.job-filter:has(> span:text-is("${label}"))`);
const kindFilter = (page: Page) => filter(page, 'Kind').locator('select');
const statusFilter = (page: Page) => filter(page, 'Status').locator('select');
const subjectFilter = (page: Page) => filter(page, 'Subject id').locator('input');
const form = (page: Page) => page.locator('form.new-job-form');
const field = (page: Page, label: string) => form(page).locator(`label:has(> span:text-is("${label}"))`);
const formError = (page: Page) => form(page).locator('p.form-error');
const list = (page: Page) => page.locator('section.list-section');
const listLine = (page: Page) => list(page).locator('p.empty');
const bodyRows = (page: Page) => page.locator('table.data-table tbody tr');
const pager = (page: Page) => list(page).locator('nav.job-pager');
const pagerLine = (page: Page) => pager(page).locator('p');
const button = (page: Page, name: string) => page.getByRole('button', { name, exact: true });
const statusButton = (page: Page, name: string) =>
  page.locator('.catalog-filters').getByRole('button', { name, exact: true });

/// Every visible row's cells, as text.
async function table(page: Page): Promise<string[][]> {
  return bodyRows(page).evaluateAll((rows) =>
    rows.map((row) => Array.from(row.children).map((td) => (td as HTMLElement).innerText.trim())),
  );
}

async function expectHeader(page: Page, title: string, subtitle: string): Promise<void> {
  await expect(page.locator('.exec-eyebrow')).toHaveText('Work');
  await expect(page.locator('h1.exec-title')).toHaveText(title);
  await expect(page.locator('header.exec-header p')).toHaveText(subtitle);
}

/// The last list read's query, as a plain object for exact comparison.
const lastRead = (seen: Seen): Record<string, string> => Object.fromEntries(seen.list.at(-1) ?? []);

async function openList(page: Page, b: Backend = {}, path = PATH): Promise<Seen> {
  const seen = await install(page, b);
  await mountPage(page, path);
  return seen;
}

// ---------------------------------------------------------------------------
// The list
// ---------------------------------------------------------------------------

test.describe('/ux/jobs — the list', () => {
  test('renders every column and word of three open packets, from one list read and no writes', async ({ page }) => {
    const seen = await openList(page);
    await expect(bodyRows(page)).toHaveCount(3);

    // Gap 1 (d1310776), answered: the header counts 286 above three rows,
    // and the list now says which of the 286 it shows and offers the
    // rest. It said neither, so the 86 oldest open packets — the ones
    // the standing order works first — could not be reached. The first
    // page's read sends no offset.
    await expectHeader(page, 'All jobs', `${LIVE_TOTAL} open`);
    expect(lastRead(seen)).toEqual({ status: 'open', limit: '200' });
    await expect(pagerLine(page)).toHaveText(`Showing 1–3 of ${LIVE_TOTAL}, newest first`);
    // The page's every button, in order: the two entry buttons, the
    // three Status buttons and the pager's two. (Clear ✕, Create Job and
    // Cancel render only under a filter or an open form.)
    await expect(page.locator('.catalog').getByRole('button')).toHaveText([
      'Start a new Job', 'Create Ad Hoc Job', 'Open', 'Closed', 'All', 'Previous', 'Next',
    ]);
    await expect(button(page, 'Previous')).toBeDisabled();
    await expect(button(page, 'Next')).toBeEnabled();

    await expect(filterBar(page).locator('label.job-filter > span')).toHaveText(['Kind', 'Status', 'Subject id']);
    // d0b93b80: it named a brewery account id.
    await expect(subjectFilter(page)).toHaveAttribute('placeholder', 'An exact subject id');
    await expect(page.locator('.catalog-filters .filter-label')).toHaveText('Status');

    // Gap 10 (8708447c): seven columns. J1's active step `test` and its
    // holder `agent-claude` arrive on the row and are shown nowhere.
    // Gap 11 (75d1b902): no department in the columns or the filters.
    await expect(page.locator('table.data-table thead th')).toHaveText([
      'ID', 'Kind', 'Title', 'Subject', 'Status', 'Priority', 'Opened',
    ]);
    expect(await table(page)).toEqual([
      ['0000aa01', 'page-audit', 'Page audit: /ux/jobs', '/ux/jobs', 'open', 'standard', '2026-09-19'],
      ['0000bb02', 'ad-hoc', 'Call the taproom', 'acc-1', 'open', 'urgent', '2026-09-23'],
      ['0000cc03', 'backlog-item', 'Replace the chiller', 'ast-9', 'open', 'standard', '2026-09-24'],
    ]);
    await expect(list(page)).not.toContainText('agent-claude');

    // Mounting and reading write nothing, and read the registry once
    // (for the Kind filter and the Ad Hoc button). The roster is the
    // shell's read at mount; the page reads it only when the form opens.
    expect(await settledReads(page, () => seen.list.length, 1)).toBe(1);
    expect(seen.registry).toBe(1);
    expect(seen.accounts).toBe(0);
    expect(seen.writes).toHaveLength(0);
  });

  test('a list read still pending paints "Loading…" under a header that states no count yet', async ({ page }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => { release = resolve; });
    await openList(page, { list: async (r) => { await held; await json(r, { data: ROWS, total: 3 }); } });

    await expect(listLine(page)).toHaveText('Loading…');
    // d0b93b80: while the read was out the header stated the zero
    // `total` starts at — "0 open" above "Loading…".
    await expectHeader(page, 'All jobs', 'Counting…');
    release();
    await expect(bodyRows(page)).toHaveCount(3);
    await expectHeader(page, 'All jobs', '3 open');
    // Every match is on the page, so there is nothing to page to.
    await expect(pager(page)).toHaveCount(0);
  });

  // The false-empty pair: an empty answer and a refused one are never
  // the same paint (jobs-list-failed-read pins the refusal on all three
  // routes; this holds the two side by side on this one).
  test('an empty backend says "No jobs match." and paints no failure marker', async ({ page }) => {
    await openList(page, { list: (r) => json(r, { data: [], total: 0 }) });
    await expect(listLine(page)).toHaveText('No jobs match.');
    await expectHeader(page, 'All jobs', '0 open');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(page.locator('table.data-table')).toHaveCount(0);
  });

  test('a refused list read says it failed, on the marker, under a header with no number', async ({ page }) => {
    await openList(page, { list: (r) => json(r, 'jobs down', 503) });
    await expect(page.locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText("Couldn't load jobs: HTTP 503");
    await expect(page.getByText('No jobs match.')).toHaveCount(0);
    await expectHeader(page, 'All jobs', 'Job count unknown — the read failed');
    await expect(page.locator('table.data-table')).toHaveCount(0);
  });
});

// ---------------------------------------------------------------------------
// The pager (d1310776)
// ---------------------------------------------------------------------------

/// Five open packets, three on the first page and two past it: a
/// server that pages by `offset` the way GET /api/jobs does, so Next
/// reaches rows the first read never returned.
const J4 = '55555555-5555-4555-8555-00000000ee05';
const J5 = '66666666-6666-4666-8666-00000000ff06';
const OLDER = [
  job(J4, 'page-audit', { subject_kind: 'custom', id: '/ux/sales' }, 'Page audit: /ux/sales', 'standard', '2026-09-17'),
  job(J5, 'backlog-item', { subject_kind: 'custom', id: 'b-1' }, 'An old backlog item', 'urgent', '2026-09-17'),
];
const paged: Handler = (r) => {
  const offset = Number(new URL(r.request().url()).searchParams.get('offset') ?? '0');
  return json(r, { data: offset === 0 ? ROWS : offset === 3 ? OLDER : [], total: 5, limit: 200, offset });
};

test.describe('/ux/jobs — the pager', () => {
  test('Next reads the rows past the first page by offset, says which it shows, and Previous comes back', async ({ page }) => {
    const seen = await openList(page, { list: paged });
    await expect(bodyRows(page)).toHaveCount(3);
    await expect(pagerLine(page)).toHaveText('Showing 1–3 of 5, newest first');

    await button(page, 'Next').click();
    await expect.poll(() => lastRead(seen)).toEqual({ status: 'open', limit: '200', offset: '3' });
    await expect(bodyRows(page)).toHaveCount(2);
    await expect(list(page)).toContainText('An old backlog item');
    await expect(pagerLine(page)).toHaveText('Showing 4–5 of 5, newest first');
    await expect(button(page, 'Next')).toBeDisabled();
    await expect(button(page, 'Previous')).toBeEnabled();
    // The header's count is the filter's, not the page's.
    await expectHeader(page, 'All jobs', '5 open');

    await button(page, 'Previous').click();
    await expect.poll(() => lastRead(seen)).toEqual({ status: 'open', limit: '200' });
    await expect(pagerLine(page)).toHaveText('Showing 1–3 of 5, newest first');
    await expect(button(page, 'Previous')).toBeDisabled();
    expect(seen.writes).toHaveLength(0);
  });

  test('changing a filter goes back to the first page', async ({ page }) => {
    const seen = await openList(page, { list: paged });
    await button(page, 'Next').click();
    await expect.poll(() => lastRead(seen)).toEqual({ status: 'open', limit: '200', offset: '3' });

    await statusButton(page, 'Closed').click();
    await expect.poll(() => lastRead(seen)).toEqual({ status: 'closed', limit: '200' });
    await expect(pagerLine(page)).toHaveText('Showing 1–3 of 5, newest first');
  });
});

// ---------------------------------------------------------------------------
// The filters
// ---------------------------------------------------------------------------

test.describe('/ux/jobs — the filters', () => {
  test('Kind lists the registry\'s raw codes, sorted; choosing one re-reads, retitles and writes the URL; Clear undoes all three', async ({ page }) => {
    const seen = await openList(page);
    await expect(bodyRows(page)).toHaveCount(3);

    await expect(kindFilter(page).locator('option')).toHaveText(['All kinds', 'ad-hoc', 'backlog-item', 'page-audit']);
    await expect(button(page, 'Clear ✕')).toHaveCount(0);

    await kindFilter(page).selectOption('page-audit');
    await expect.poll(() => lastRead(seen)).toEqual({ kind: 'page-audit', status: 'open', limit: '200' });
    await expect(page.locator('h1.exec-title')).toHaveText('page-audit jobs');
    await expect.poll(() => new URL(page.url()).search).toBe('?kind=page-audit');
    await expect(button(page, 'Clear ✕')).toHaveAttribute('title', 'Clear all filters');

    await button(page, 'Clear ✕').click();
    await expect.poll(() => lastRead(seen)).toEqual({ status: 'open', limit: '200' });
    await expect(page.locator('h1.exec-title')).toHaveText('All jobs');
    await expect.poll(() => new URL(page.url()).search).toBe('');
    await expect(button(page, 'Clear ✕')).toHaveCount(0);
    expect(seen.writes).toHaveLength(0);
  });

  test('the Status select and the Status buttons are one filter: each moves the other, the read and the count', async ({ page }) => {
    const seen = await openList(page);
    await expect(bodyRows(page)).toHaveCount(3);
    await expect(statusFilter(page)).toHaveValue('open');

    await statusFilter(page).selectOption('closed');
    await expect.poll(() => lastRead(seen)).toEqual({ status: 'closed', limit: '200' });
    await expect(page.locator('.filter-button-active')).toHaveText('Closed');
    await expect(statusButton(page, 'Closed')).toHaveAttribute('aria-pressed', 'true');
    // Gap 10 (8708447c): Closed shows no closed date; the columns do not change.
    await expect(page.locator('table.data-table thead th')).toHaveCount(7);
    await expectHeader(page, 'All jobs', `${LIVE_TOTAL} closed`);
    await expect(button(page, 'Clear ✕')).toBeVisible();

    await statusButton(page, 'All').click();
    await expect.poll(() => lastRead(seen)).toEqual({ limit: '200' });
    await expect(statusFilter(page)).toHaveValue('');
    // d0b93b80: it read "286 any-status".
    await expectHeader(page, 'All jobs', `${LIVE_TOTAL} in all statuses`);

    await statusButton(page, 'Open').click();
    await expect.poll(() => lastRead(seen)).toEqual({ status: 'open', limit: '200' });
    await expect(button(page, 'Clear ✕')).toHaveCount(0);
  });

  test('Subject id is an exact-match filter that reads once per keystroke, and Clear empties it', async ({ page }) => {
    const seen = await openList(page);
    await expect(bodyRows(page)).toHaveCount(3);
    const before = seen.list.length;

    await subjectFilter(page).pressSequentially('ast-9');
    await expect.poll(() => lastRead(seen)).toEqual({ status: 'open', subject_id: 'ast-9', limit: '200' });
    expect(seen.list.slice(before).map((p) => p.get('subject_id'))).toEqual(['a', 'as', 'ast', 'ast-', 'ast-9']);
    await expect.poll(() => new URL(page.url()).search).toBe('?subject_id=ast-9');

    await button(page, 'Clear ✕').click();
    await expect(subjectFilter(page)).toHaveValue('');
    await expect.poll(() => lastRead(seen)).toEqual({ status: 'open', limit: '200' });
  });

  test('an owner_id deep link narrows the read with no control, chip or title that says so, and Clear keeps it', async ({ page }) => {
    const seen = await openList(page, {}, `${PATH}?owner_id=emp-002&kind_prefix=backlog`);
    await expect(bodyRows(page)).toHaveCount(3);

    // Gap 6 (6c9672c2): owner_id and kind_prefix reach the read; the
    // page names the prefix in its title and the owner nowhere, and
    // no filter control shows either.
    expect(lastRead(seen)).toEqual({ kind_prefix: 'backlog', status: 'open', owner_id: 'emp-002', limit: '200' });
    await expect(page.locator('h1.exec-title')).toHaveText('backlog jobs');
    await expect(filterBar(page)).not.toContainText('emp-002');
    await expect(filterBar(page)).not.toContainText('Rhea');
    await expect(button(page, 'Clear ✕')).toHaveCount(0);

    await statusFilter(page).selectOption('closed');
    await button(page, 'Clear ✕').click();
    await expect.poll(() => lastRead(seen)).toEqual({ kind_prefix: 'backlog', status: 'open', owner_id: 'emp-002', limit: '200' });
  });

  test('the page offers no department control (gap 11)', async ({ page }) => {
    // Gap 11 (75d1b902): the server's `department=` join exists and the
    // page mounts with one on /ux/service and /ux/sales — but /ux/jobs
    // has no way to set it, and a `department` query is not read.
    const seen = await openList(page, {}, `${PATH}?department=finance`);
    await expect(bodyRows(page)).toHaveCount(3);
    expect(lastRead(seen)).toEqual({ status: 'open', limit: '200' });
    await expect(page.getByRole('link', { name: /department/i })).toHaveCount(0);
    await expect(page.locator('.catalog').getByText(/department/i)).toHaveCount(0);
  });

  test('a failed registry read leaves Kind offering "All kinds" alone and says nothing until the form opens', async ({ page }) => {
    const seen = await openList(page, { registry: (r) => json(r, 'registry down', 503) });
    await expect(bodyRows(page)).toHaveCount(3);
    expect(await settledReads(page, () => seen.registry, 1)).toBe(1);

    // Gap 3 (3b1ec06e): no failure line anywhere with the form closed,
    // and the Ad Hoc button is simply absent.
    await expect(kindFilter(page).locator('option')).toHaveText(['All kinds']);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(page.getByText('HTTP 503')).toHaveCount(0);
    await expect(button(page, 'Create Ad Hoc Job')).toHaveCount(0);

    // Focusing the select is the gesture that asks again — and a second
    // failure is just as silent.
    await kindFilter(page).focus();
    await expect.poll(() => seen.registry).toBe(2);
    await expect(page.getByText('HTTP 503')).toHaveCount(0);

    // Opening the form asks a third time, and the form is the one place
    // the failure is said: its bare status, with no noun.
    await button(page, 'Start a new Job').click();
    await expect.poll(() => seen.registry).toBe(3);
    await expect(formError(page)).toHaveText('HTTP 503');
    await expect(field(page, 'Kind').locator('option')).toHaveText(['— select —']);
  });
});

// ---------------------------------------------------------------------------
// The links
// ---------------------------------------------------------------------------

test.describe('/ux/jobs — the links', () => {
  test('the ID link opens the packet, and back returns to the list', async ({ page }) => {
    await openList(page);
    const link = bodyRows(page).nth(2).getByRole('link', { name: '0000cc03', exact: true });
    await expect(link).toHaveAttribute('href', `/ux/jobs/${J3}`);
    // The router serves the detail as `jobDetail`; like every detail
    // page it has no catalog row, and the list it came from does.
    expect(parseRoute(`/ux/jobs/${J3}`)).toMatchObject({ kind: 'jobDetail', jobId: J3 });
    expect(parseRoute(PATH).kind).toBe('jobs');

    await link.click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(`/ux/jobs/${J3}`);
    await expect(page.locator('h1')).toContainText('Replace the chiller');

    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(page.locator('h1.exec-title')).toHaveText('All jobs');
    await expect(bodyRows(page)).toHaveCount(3);
  });

  test('a click anywhere else on the row goes where the ID link goes, in one history entry', async ({ page }) => {
    await openList(page);
    const row = page.getByRole('link', { name: 'Call the taproom', exact: true });
    await expect(row).toHaveCount(1);
    await row.locator('td', { hasText: 'urgent' }).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(`/ux/jobs/${J2}`);
    await expect(page.locator('h1')).toContainText('Call the taproom');

    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(bodyRows(page)).toHaveCount(3);
  });

  test('each Subject links by its kind, to a path the router serves', async ({ page }) => {
    await openList(page);
    const subject = (name: string) => page.locator('table.data-table').getByRole('link', { name, exact: true });

    // d0b93b80: every Subject href was the router's unprefixed legacy
    // spelling ("defensive"); each is now the catalog's /ux/…, and the
    // custom one lands on ROUTE_CATALOG.jobs.path.
    const expected: ReadonlyArray<[string, string, string]> = [
      ['/ux/jobs', `${PATH}?subject_id=%2Fux%2Fjobs`, 'jobs'],
      ['acc-1', '/ux/accounts/acc-1', 'account'],
      ['ast-9', '/ux/assets/ast-9', 'asset'],
    ];
    for (const [name, href, kind] of expected) {
      await expect(subject(name)).toHaveAttribute('href', href);
      const [path, query = ''] = href.split('?');
      expect(parseRoute(path!, query ? `?${query}` : '').kind, href).toBe(kind);
    }
  });

  test('a custom Subject\'s link opens the list of its own packets, and back returns to them all', async ({ page }) => {
    // A backend that honours subject_id, so a narrowed read shows.
    const seen = await openList(page, {
      list: (r) => {
        const sid = new URL(r.request().url()).searchParams.get('subject_id');
        const data = sid ? ROWS.filter((j) => j.subject.id === sid) : ROWS;
        return json(r, { data, total: data.length });
      },
    });
    await expect(bodyRows(page)).toHaveCount(3);

    await page.locator('table.data-table').getByRole('link', { name: '/ux/jobs', exact: true }).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    expect(new URL(page.url()).searchParams.get('subject_id')).toBe('/ux/jobs');

    // d0b93b80: the /jobs mount was not keyed on the route, so the page
    // kept the filters it took at mount — no read, an empty Subject id
    // box, all three rows under a URL naming one subject. App now keys
    // it, as it keys Finance, and the click mounts what the URL names.
    await expect(subjectFilter(page)).toHaveValue('/ux/jobs');
    await expect.poll(() => lastRead(seen)).toEqual({ status: 'open', subject_id: '/ux/jobs', limit: '200' });
    await expect(bodyRows(page)).toHaveCount(1);

    await page.goBack();
    await expect(subjectFilter(page)).toHaveValue('');
    await expect(bodyRows(page)).toHaveCount(3);
  });
});

// ---------------------------------------------------------------------------
// The new-job form and its one write
// ---------------------------------------------------------------------------

test.describe('/ux/jobs — the new-job form', () => {
  test('Start a new Job opens the form, reads the roster, and defaults every field it can', async ({ page }) => {
    const seen = await openList(page);
    await expect(bodyRows(page)).toHaveCount(3);
    const peopleAtMount = seen.people;
    await expect(form(page)).toHaveCount(0);

    await button(page, 'Start a new Job').click();
    await expect(form(page)).toBeVisible();
    await expect.poll(() => seen.people).toBe(peopleAtMount + 1);

    await expect(form(page).locator('label > span')).toHaveText([
      'Kind', 'Subject kind', 'Subject id', 'Owner', 'Title (optional)',
    ]);
    // Registry order, not sorted, each "Label (kind)" unless the two match.
    await expect(field(page, 'Kind').locator('option')).toHaveText([
      '— select —', 'Page audit (page-audit)', 'Ad hoc (ad-hoc)', 'backlog-item',
    ]);
    await expect(field(page, 'Kind').locator('select')).toHaveValue('');
    // With no kind picked, Subject kind offers every kind the registry
    // names, and the form takes the first on opening. d0b93b80: a taken
    // subject kind used to be the ONLY option until a Kind was picked,
    // so `account` could not be chosen first, though Ad hoc accepts it.
    await expect(field(page, 'Subject kind').locator('option')).toHaveText(['— select —', 'custom', 'account']);
    await expect(field(page, 'Subject kind').locator('select')).toHaveValue('custom');
    await expect(field(page, 'Subject id').locator('input')).toHaveAttribute(
      'placeholder', 'Type the id by hand (no autocomplete for this kind)',
    );
    await expect(field(page, 'Owner').locator('option')).toHaveText(['— unassigned —', 'Demo CEO (ceo)', 'Rhea Okafor (brewer)']);
    await expect(field(page, 'Owner').locator('select')).toHaveValue('emp-001');
    await expect(field(page, 'Title (optional)').locator('input')).toHaveAttribute('placeholder', 'Defaulted from kind + subject');
    await expect(button(page, 'Create Job')).toBeDisabled();
    await expect(button(page, 'Cancel')).toBeEnabled();
    await expect(formError(page)).toHaveCount(0);

    // Choosing account narrows Kind to what takes one, and stays chosen.
    await field(page, 'Subject kind').locator('select').selectOption('account');
    await expect(field(page, 'Kind').locator('option')).toHaveText(['— select —', 'Ad hoc (ad-hoc)']);
    await expect(field(page, 'Subject kind').locator('select')).toHaveValue('account');

    await button(page, 'Cancel').click();
    await expect(form(page)).toHaveCount(0);
    expect(seen.writes).toHaveLength(0);
  });

  test('picking a kind shows its description and step preview, and Create Job sends the one write and opens the packet', async ({ page }) => {
    const seen = await openList(page);
    await button(page, 'Start a new Job').click();

    await field(page, 'Kind').locator('select').selectOption('page-audit');
    await expect(form(page).locator('p.kind-description')).toHaveText('quality Read a page against its department.');
    await expect(form(page).locator('.kind-category')).toHaveText('quality');
    const preview = form(page).locator('details.step-preview');
    await expect(preview).toHaveAttribute('open', '');
    await expect(preview.locator('summary')).toHaveText('Step preview · 2 steps');
    await expect(preview.locator('li')).toHaveText([
      'task Inventory the page',
      'sign-off Review the words ✓ platform-admin',
    ]);
    await expect(field(page, 'Subject kind').locator('option')).toHaveText(['— select —', 'custom']);
    await expect(button(page, 'Create Job')).toBeDisabled();

    await field(page, 'Subject id').locator('input').fill('/ux/vendors');
    await expect(button(page, 'Create Job')).toBeEnabled();
    await button(page, 'Create Job').click();

    await expect.poll(() => new URL(page.url()).pathname).toBe(`/ux/jobs/${CREATED}`);
    await expect(page.locator('h1')).toContainText('Page audit — /ux/vendors');
    expect(seen.writes).toHaveLength(1);
    const body = seen.writes[0]!.postDataJSON() as Record<string, unknown>;
    expect(seen.writes[0]!.method()).toBe('POST');
    expect(new URL(seen.writes[0]!.url()).pathname).toBe('/api/jobs');
    expect(body['opened_on']).toMatch(/^\d{4}-\d{2}-\d{2}$/);
    expect({ ...body, opened_on: 'today' }).toEqual({
      kind: 'page-audit',
      subject: { subject_kind: 'custom', custom_kind: 'custom', ref_id: '/ux/vendors' },
      title: 'Page audit — /ux/vendors',
      owner_id: 'emp-001',
      status: 'open',
      priority: 'standard',
      opened_on: 'today',
      metadata: {},
      tags: [],
    });

    // Back returns to the list it came from, with the form closed.
    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(bodyRows(page)).toHaveCount(3);
    await expect(form(page)).toHaveCount(0);
  });

  test('a refused create shows the refusal in the form, where the operator was looking, and stays put', async ({ page }) => {
    const seen = await openList(page, {
      post: (r) => r.fulfill({ status: 403, contentType: 'text/plain', body: 'refused by policy: create on job' }),
    });
    await button(page, 'Start a new Job').click();
    await field(page, 'Kind').locator('select').selectOption('backlog-item');
    await field(page, 'Subject id').locator('input').fill('bosspipeline');
    await field(page, 'Title (optional)').locator('input').fill('Pin the jobs page');
    await button(page, 'Create Job').click();

    await expect(formError(page)).toHaveText('HTTP 403: refused by policy: create on job');
    await expect(form(page)).toBeVisible();
    await expect(button(page, 'Create Job')).toBeEnabled();
    expect(new URL(page.url()).pathname).toBe(PATH);
    expect(seen.writes).toHaveLength(1);
    const body = seen.writes[0]!.postDataJSON() as Record<string, unknown>;
    expect(body['title']).toBe('Pin the jobs page');
    expect(body['kind']).toBe('backlog-item');
  });

  test('Create Ad Hoc Job preselects ad-hoc; an account subject autocompletes, says when the list is capped, and names the account in the default title', async ({ page }) => {
    const seen = await openList(page);
    await button(page, 'Create Ad Hoc Job').click();

    await expect(field(page, 'Kind').locator('select')).toHaveValue('ad-hoc');
    // Ad hoc has no description and no steps: neither block renders.
    await expect(form(page).locator('p.kind-description')).toHaveCount(0);
    await expect(form(page).locator('details.step-preview')).toHaveCount(0);
    await expect(field(page, 'Subject kind').locator('option')).toHaveText(['— select —', 'account', 'custom']);
    await expect(field(page, 'Subject kind').locator('select')).toHaveValue('account');

    // Read ONCE. d0b93b80: it was read twice — the effect that defaults
    // the subject kind writes it and then loads its options, the write
    // re-ran the effect before the first read had answered, and the
    // loader's guard was set only by an answer. It now holds the read
    // that is out.
    expect(await settledReads(page, () => seen.accounts, 1)).toBe(1);
    await expect(form(page).locator('datalist#new-job-subject-options option')).toHaveCount(2);
    await expect(field(page, 'Subject id').locator('input')).toHaveAttribute('placeholder', 'Pick from the list or type an id');
    await expect(field(page, 'Subject id').locator('small.hint')).toHaveText(
      "Showing 2 of 40 account options — type the id by hand if you don't see yours.",
    );

    await field(page, 'Subject id').locator('input').fill('acc-2');
    await field(page, 'Owner').locator('select').selectOption('emp-002');
    await button(page, 'Create Job').click();
    await expect.poll(() => seen.writes.length).toBe(1);
    const body = seen.writes[0]!.postDataJSON() as Record<string, unknown>;
    expect(body['subject']).toEqual({ subject_kind: 'account', id: 'acc-2' });
    expect(body['title']).toBe('Ad hoc — Prairie Pub');
    expect(body['owner_id']).toBe('emp-002');
  });

  test('without an ad-hoc row the Ad Hoc button is not offered', async ({ page }) => {
    const seen = await openList(page, { registry: (r) => json(r, REGISTRY.filter((w) => w.kind !== 'ad-hoc')) });
    await expect(bodyRows(page)).toHaveCount(3);
    expect(await settledReads(page, () => seen.registry, 1)).toBe(1);
    await expect(button(page, 'Start a new Job')).toBeVisible();
    await expect(button(page, 'Create Ad Hoc Job')).toHaveCount(0);
  });

  test('a failed subject autocomplete says so in the form and falls back to typing', async ({ page }) => {
    await openList(page, { accounts: (r) => json(r, 'people down', 502) });
    await button(page, 'Create Ad Hoc Job').click();
    await expect(field(page, 'Subject kind').locator('select')).toHaveValue('account');

    await expect(formError(page)).toHaveText('HTTP 502');
    // The placeholder says the kind HAS no autocomplete — the reason is
    // the read failed, which only the error line above says.
    await expect(field(page, 'Subject id').locator('input')).toHaveAttribute(
      'placeholder', 'Type the id by hand (no autocomplete for this kind)',
    );
    await expect(form(page).locator('datalist#new-job-subject-options option')).toHaveCount(0);
  });

  test('a failed people read leaves the Owner picker with "— unassigned —" alone, and says nothing', async ({ page }) => {
    await openList(page);
    await expect(bodyRows(page)).toHaveCount(3);
    // Failed only now: the shell has already read the roster at mount,
    // so the session is signed in and this is the page's own read.
    let peopleReads = 0;
    await page.route(/\/api\/people$/, (r) => {
      peopleReads += 1;
      return json(r, 'people down', 503);
    });
    await button(page, 'Start a new Job').click();
    await expect.poll(() => peopleReads).toBe(1);

    // Gap 5 (ce8f634a): the non-2xx is swallowed; the picker shows only
    // the unassigned row, and the session user it defaulted to is no
    // longer an option the select can show.
    await expect(field(page, 'Owner').locator('option')).toHaveText(['— unassigned —']);
    await expect(formError(page)).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('a new-job deep link opens the form narrowed to kinds that take its subject; Cancel closes it and keeps the filters in the URL', async ({ page }) => {
    const DEEP = `${PATH}?new=1&subject_kind=account&subject_id=acc-1&status=closed`;
    const seen = await openList(page, {}, DEEP);
    await expect(form(page)).toBeVisible();

    await expect(field(page, 'Kind').locator('option')).toHaveText(['— select —', 'Ad hoc (ad-hoc)']);
    // d0b93b80: it read "accept a account subject".
    await expect(field(page, 'Kind').locator('small.hint')).toHaveText(
      'Filtered to kinds that accept account subjects (1 of 3)',
    );
    await expect(field(page, 'Subject kind').locator('select')).toHaveValue('account');
    await expect(field(page, 'Subject id').locator('input')).toHaveValue('acc-1');
    await expect(button(page, 'Create Job')).toBeDisabled();

    // d0b93b80: `subject_id` was both the new job's subject and the
    // list's filter, so the deep link narrowed the list behind the form.
    // Under `new=1` it is the new job's only; the list keeps the status
    // the link asked for, and the mount rewrites none of the deep link.
    await expect(subjectFilter(page)).toHaveValue('');
    await expect.poll(() => lastRead(seen)).toEqual({ status: 'closed', limit: '200' });
    expect(seen.list.every((p) => !p.has('subject_id'))).toBe(true);
    expect(new URL(page.url()).search).toBe('?new=1&subject_kind=account&subject_id=acc-1&status=closed');

    // d0b93b80: Cancel did not close a deep-linked form. It closed it and
    // stripped the query, then the effect that had opened it from the
    // deep link saw `newJobOpen` false while `initialNewJobOpen` was
    // still true and opened it again at once, reset — what was typed
    // was lost. The deep link now opens the form once, at mount; and
    // Cancel takes out only the deep link's new-job half, so the URL
    // keeps the filters the page still shows.
    await field(page, 'Title (optional)').locator('input').fill('typed, then cancelled');
    await button(page, 'Cancel').click();
    await expect(form(page)).toHaveCount(0);
    await expect.poll(() => new URL(page.url()).search).toBe('?status=closed');
    await expect(subjectFilter(page)).toHaveValue('');
    await expect(statusFilter(page)).toHaveValue('closed');
    // short on purpose: a form that does not come back has no event to wait on — the window elapsing IS the answer
    await page.waitForTimeout(300);
    await expect(form(page)).toHaveCount(0);
    expect(seen.writes).toHaveLength(0);

    // A reload of what the URL now says is the page the operator sees.
    await page.reload();
    await expect(bodyRows(page)).toHaveCount(3);
    await expect(form(page)).toHaveCount(0);
    await expect(statusFilter(page)).toHaveValue('closed');
  });

  test('a subject filter typed while a deep-linked form is open waits for Cancel to reach the URL', async ({ page }) => {
    await openList(page, {}, `${PATH}?new=1&subject_kind=account&subject_id=acc-1`);
    await expect(form(page)).toBeVisible();
    await subjectFilter(page).fill('ast-9');
    // The deep link's subject is not overwritten while its form is up.
    await expect.poll(() => new URL(page.url()).searchParams.get('subject_id')).toBe('acc-1');

    await button(page, 'Cancel').click();
    await expect(form(page)).toHaveCount(0);
    await expect.poll(() => new URL(page.url()).search).toBe('?subject_id=ast-9');
  });

  // HrPage's "start a workflow" link is this shape: it names the new
  // job's workflow in `kind`. d0b93b80 made the deep link's subject_id
  // the new job's and left `kind` a list filter, so the list behind the
  // form narrowed to that workflow and the form's own Kind came up
  // unpicked (backlog 3f5cce16). Under `new=1` it is the form's Kind.
  test('a deep link naming a kind picks it in the form and filters nothing; Cancel takes it out', async ({ page }) => {
    const seen = await openList(page, {}, `${PATH}?new=1&kind=ad-hoc&subject_kind=account&subject_id=acc-1&status=closed`);
    await expect(form(page)).toBeVisible();
    await expect(field(page, 'Kind').locator('select')).toHaveValue('ad-hoc');
    await expect(field(page, 'Subject kind').locator('select')).toHaveValue('account');
    await expect(field(page, 'Subject id').locator('input')).toHaveValue('acc-1');
    await expect(button(page, 'Create Job')).toBeEnabled();

    await expect(kindFilter(page)).toHaveValue('');
    await expect.poll(() => lastRead(seen)).toEqual({ status: 'closed', limit: '200' });
    expect(seen.list.every((p) => !p.has('kind'))).toBe(true);
    await expect(bodyRows(page)).toHaveCount(3);
    expect(new URL(page.url()).search).toBe('?new=1&kind=ad-hoc&subject_kind=account&subject_id=acc-1&status=closed');

    // A Kind filter chosen while the form is up waits for Cancel, as a
    // subject filter does; Cancel keeps the status and drops only the
    // new-job half, the deep link's kind with it.
    await kindFilter(page).selectOption('page-audit');
    await expect.poll(() => lastRead(seen)).toEqual({ kind: 'page-audit', status: 'closed', limit: '200' });
    expect(new URL(page.url()).searchParams.get('kind')).toBe('ad-hoc');
    await button(page, 'Cancel').click();
    await expect(form(page)).toHaveCount(0);
    await expect.poll(() => new URL(page.url()).search).toBe('?status=closed&kind=page-audit');
    await expect(kindFilter(page)).toHaveValue('page-audit');
    expect(seen.writes).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// A guest
// ---------------------------------------------------------------------------

test.describe('/ux/jobs — a read-only guest', () => {
  test('the list and its filters work; the two entry buttons are disabled, with a way to sign in', async ({ page }) => {
    const seen = await openList(page, { session: { username: 'guest@algedonic.dev', role: 'audit-readonly' } });
    await expect(bodyRows(page)).toHaveCount(3);

    await expect(button(page, 'Start a new Job')).toBeDisabled();
    await expect(button(page, 'Create Ad Hoc Job')).toBeDisabled();
    const note = page.locator('.job-actions p.write-gate-note');
    await expect(note).toHaveText('Read-only session — sign in to act.');
    await expect(note.getByRole('link', { name: 'sign in' })).toHaveAttribute('href', /^\/login\?next=%2Fux%2Fjobs/);

    await statusFilter(page).selectOption('closed');
    await expect.poll(() => lastRead(seen)).toEqual({ status: 'closed', limit: '200' });
    expect(seen.writes).toHaveLength(0);
  });
});
