// /ux/inbox — "Inbox" (home), every control and render state pinned as
// the page behaves TODAY (page audit 5477d9eb, step `test`).
//
// Four narrower specs already pin one piece each: false-empty's inbox
// pair (the failure line and Retry, at the legacy /inbox path),
// readonly-session (Compose disabled for a guest), inbox-write-refusals
// (gaps 4 and 5, 2a6fab80 and 129da587) and inbox-archive-and-bulk
// (gaps 2 and 8, 8578b91e and 5963a322). None of them presses a filter
// button, types a search, reads a sender, age or glyph, opens the step
// or job a message points at and comes back, closes the composer three
// ways, or watches Send say "Sending...". This spec is the page's
// whole meaning in one place; the four stay, as the regression pins of
// their fixes.
//
// What the page is (controls_md on the packet, re-read against
// origin/main 507d2308 on 2026-09-25 — the page grew Archive, the bulk
// bar and two refusal lines since `measure` counted it): 1 link kind
// per message that carries an entity_path, plus "Open without marking
// read" beside a refused one; 5 filter buttons, Compose, Retry, and in
// the composer ✕, Send and Cancel; per row Mark read (unread rows only)
// and Archive; the bulk bar's Mark all read and Archive selected; 1
// search input; 1 "Select all shown" checkbox and 1 checkbox per row;
// 1 form (To, Subject, Message). TWO reads, GET
// /api/messages/inbox/{viewer} and GET /api/people; THREE writes, POST
// /api/messages/{id}/read, POST /api/messages/{id}/archive and POST
// /api/messages/send.
//
// Ages are measured from the app clock, so the clock is fixed at NOW.
//
// Lines that pin a FILED gap's current behaviour name the gap and its
// item. They are meant to be edited by the car that fixes it, so the
// fix shows up here as a changed expectation instead of a silently
// passing one:
//   gap 3  74da899d  the inbox read is unbounded: no limit, no paging,
//                    every row rendered in one list
//   gap 6  7d1c11a3  a failed roster read empties the To list and
//                    renders senders as raw ids, and says nothing
// Answered, and pinned as answered: gaps 2, 4, 5, 8 (above) and gap 7
// (fd7f4ab8) — the failure line is pinned here on the catalogued
// path, every way a read can fail. Gap 1 (0b2bac00) landed in the
// messages domain; gaps 9 (efd5a07d) and 10 (9c453257) are the notify
// rule's and the machine door's, not controls this page renders.
//
// Six more this spec found and the audit did not list, all answered by
// backlog e2679b23 and pinned below as answered: (a) the search reads
// the sender as the row SHOWS it, not only the id; (b) a read that has
// not landed prints no counts on the filter buttons; (c) a 200 whose
// body is not a list is a failed read, never the empty inbox; (d) a
// guest's per-row Mark read, Archive and checkbox stand behind the
// WriteGate, and a guest's link is a navigation with no write; (e) the
// To list names each role by its registry name; (f) the search box
// has an accessible name.

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { mountPage, settledReads } from './_helpers';
import { installApiFloor } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { parseRoute } from '../../src/router';

const PATH = '/ux/inbox';
const NOW = new Date('2026-09-25T12:00:00Z');

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const emp = (id: string, name: string, role: string) => ({
  id, name, email: `${id}@algedonic.test`, role, department: 'it',
  hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [],
});
const DAVID = emp('emp-001', 'David', 'platform-admin');
const BO = emp('emp-002', 'Bo Cellar', 'head-of-sales');
const ROSTER = [DAVID, BO];

/// The role Classes, named the way no code could derive them from the
/// code — so the To list proves it read the registry (e2679b23 (e)).
const role = (code: string, display_name: string, sort_order: number) => ({
  subject_kind: 'employee', code, display_name, parent_code: null, member_attribute: 'role',
  metadata: {}, sort_order, retired_at: null,
});
const ROLE_CLASSES = [role('platform-admin', 'Platform administrator', 1), role('head-of-sales', 'Sales lead', 2)];

const ago = (minutes: number): string => new Date(NOW.getTime() - minutes * 60_000).toISOString();

type Msg = Readonly<{
  id: string; sender_id: string; recipient_id: string; kind: string;
  subject: string; body: string; sent_at: string; read_at: string | null;
  entity_ref: { entity_type: string; entity_id: string; entity_path?: string | null } | null;
}>;

const msg = (m: Omit<Msg, 'recipient_id'>): Msg => ({ ...m, recipient_id: DAVID.id });

/// Five messages, in the read's order (the server's sent_at DESC):
/// two unread directs (waiting on you), one unread signal, one read
/// direct, one read signal. The two producer link shapes — a step and
/// a job — and one ref with no path.
const STEP = msg({
  id: 'msg-step', sender_id: BO.id, kind: 'direct', subject: 'Review the design',
  body: 'The review step is ready.', sent_at: ago(30), read_at: null,
  entity_ref: { entity_type: 'step', entity_id: 'step-9', entity_path: '/jobs/job-1/steps/step-9' },
});
const JOB = msg({
  id: 'msg-job', sender_id: 'system', kind: 'direct', subject: 'Approve the payout',
  body: 'A payout waits on your sign-off.', sent_at: ago(5 * 60), read_at: null,
  entity_ref: { entity_type: 'job', entity_id: 'job-2', entity_path: '/jobs/job-2' },
});
const READ_SIGNAL = msg({
  id: 'msg-sig-read', sender_id: 'system', kind: 'signal', subject: 'Train departed',
  body: 'Three cars rode.', sent_at: ago(26 * 60), read_at: ago(20 * 60), entity_ref: null,
});
const READ_DIRECT = msg({
  id: 'msg-read', sender_id: BO.id, kind: 'direct', subject: 'Lunch on Friday',
  body: 'Tacos?', sent_at: ago(2 * 24 * 60), read_at: ago(24 * 60), entity_ref: null,
});
const SIGNAL = msg({
  id: 'msg-sig', sender_id: 'automation:dispatcher', kind: 'signal', subject: 'Feedback closed',
  body: 'Your feedback reached its terminal.', sent_at: ago(3 * 24 * 60), read_at: null,
  entity_ref: { entity_type: 'job', entity_id: 'job-3', entity_path: null },
});
const INBOX: ReadonlyArray<Msg> = [STEP, JOB, READ_SIGNAL, READ_DIRECT, SIGNAL];

const INBOX_READ = /\/api\/messages\/inbox\/[^/?]+(\?.*)?$/;
const ROSTER_READ = /\/api\/people$/;
const SEND = /\/api\/messages\/send$/;
const ROW_WRITE = /\/api\/messages\/([^/]+)\/(read|archive)$/;

type Backend = {
  /// Every inbox read's URL, in order.
  inboxReads: string[];
  /// Every write the page sent, as "verb id" or "send <json body>".
  writes: string[];
};

type Options = Readonly<{
  /// The inbox read's answer: the rows, or a handler of its own.
  inbox?: ReadonlyArray<Msg> | ((r: Route) => Promise<void>);
  /// Ids whose Mark read / Archive answer 403.
  refuse?: ReadonlyArray<string>;
  /// The roster: the list, or a handler. The SESSION reads it first.
  roster?: unknown;
  /// The gateway's probe body (default: David, an operator).
  session?: Record<string, unknown>;
}>;

/// A small backend whose rows answer the page's writes: Mark read sets
/// `read_at`, Archive drops the row from the next read, a refused id
/// answers 403 with the server's words.
async function install(page: Page, opts: Options = {}): Promise<Backend> {
  let rows: Msg[] = Array.isArray(opts.inbox) ? [...(opts.inbox as Msg[])] : [...INBOX];
  const backend: Backend = { inboxReads: [], writes: [] };
  await page.clock.setFixedTime(NOW);
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });
  await installApiFloor(page);
  await page.route(ROSTER_READ, (r) =>
    typeof opts.roster === 'function'
      ? (opts.roster as (r: Route) => Promise<void>)(r)
      : json(r, opts.roster ?? ROSTER));
  await page.route(/\/api\/session$/, (r) =>
    json(r, opts.session ?? { username: 'david', employee_id: DAVID.id, role: 'platform-admin' }));
  await page.route(/\/api\/classes(\?|$)/, (r) => json(r, ROLE_CLASSES));
  // Where the links land: the job surfaces read the job, which this
  // backend does not hold — its own not-found, not the floor's `[]`,
  // which the job page cannot parse.
  await page.route(/\/api\/jobs\/job-\d+(\/.*)?$/, (r) => json(r, 'not found', 404));
  await page.route(INBOX_READ, (r) => {
    backend.inboxReads.push(r.request().url());
    return typeof opts.inbox === 'function' ? opts.inbox(r) : json(r, rows);
  });
  await page.route(ROW_WRITE, (r) => {
    const [, id, verb] = ROW_WRITE.exec(new URL(r.request().url()).pathname) ?? [];
    backend.writes.push(`${verb} ${id}`);
    if (opts.refuse?.includes(id!)) return r.fulfill({ status: 403, body: 'not your message' });
    rows = verb === 'archive'
      ? rows.filter((m) => m.id !== id)
      : rows.map((m) => (m.id === id ? { ...m, read_at: NOW.toISOString() } : m));
    return r.fulfill({ status: 204 });
  });
  return backend;
}

/// The shell's own non-GET: App.svelte records every route open
/// (shell/surface-opens.ts). It is the chrome's write, not this page's.
const SHELL_WRITES: ReadonlySet<string> = new Set(['/api/surface-opens']);

function watch(page: Page): { roster: number; writes: Request[] } {
  const seen = { roster: 0, writes: [] as Request[] };
  page.on('request', (req) => {
    const url = new URL(req.url());
    if (!url.pathname.startsWith('/api/')) return;
    if (req.method() !== 'GET') {
      if (!SHELL_WRITES.has(url.pathname)) seen.writes.push(req);
      return;
    }
    if (url.pathname === '/api/people') seen.roster += 1;
  });
  return seen;
}

const TITLE = '2 waiting on you';
const SUBTITLE = '3 unread · 3 direct · 2 signals · 5 total';

async function open(page: Page, opts: Options = {}): Promise<Backend> {
  const backend = await install(page, opts);
  await mountPage(page, PATH, { titleMatch: /waiting on you/ });
  return backend;
}

async function expectHeader(page: Page, title: string, subtitle: string): Promise<void> {
  await expect(page.locator('.exec-eyebrow')).toHaveText('Inbox');
  await expect(page.locator('h1.exec-title')).toHaveText(title);
  await expect(page.locator('header.exec-header p')).toHaveText(subtitle);
}

const filters = (page: Page) => page.locator('aside.catalog-filters');
const filter = (page: Page, name: string) => filters(page).getByRole('button', { name, exact: true });
/// The page's own search box — the shell's bar carries a second one.
const searchbox = (page: Page) => filters(page).locator('input[type="search"]');
const list = (page: Page) => page.locator('section.list-section');
const empty = (page: Page) => list(page).locator('p.empty');
const rows = (page: Page) => page.locator('.inbox-row');
const subjects = (page: Page) => page.locator('.inbox-row .inbox-subject');
const row = (page: Page, subject: string) => rows(page).filter({ hasText: subject });
const bulk = (page: Page) => page.locator('.inbox-bulk');
const modal = (page: Page) => page.locator('.compose-modal');

test.describe('/ux/inbox — the inbox, read', () => {
  test('the page renders every heading, count, row and word, from two reads and no writes', async ({ page }) => {
    const seen = watch(page);
    const backend = await open(page);

    await expectHeader(page, TITLE, SUBTITLE);
    await expect(page.getByRole('button', { name: 'Compose', exact: true })).toBeEnabled();
    await expect(filters(page).locator('.filter-label')).toHaveText(['Search', 'Filter']);
    await expect(searchbox(page)).toHaveAttribute('placeholder', 'Subject, sender…');
    // (f) A name of its own: the placeholder is example text, and it is
    // gone once anything is typed (class 2361ac45).
    await expect(filters(page).getByRole('searchbox', { name: 'Search messages', exact: true })).toHaveCount(1);
    await expect(filters(page).getByRole('button')).toHaveText([
      'Waiting on you (2)', 'All (5)', 'Unread (3)', 'Direct (3)', 'Signals (2)',
    ]);
    // The landing view is Waiting on you: unread directs only.
    await expect(filters(page).locator('button[aria-pressed="true"]')).toHaveText(['Waiting on you (2)']);
    await expect(subjects(page)).toHaveText(['Review the design', 'Approve the payout']);

    // The bulk bar counts what is shown.
    await expect(bulk(page)).toContainText('Select all shown');
    await expect(bulk(page).getByRole('button')).toHaveText(['Mark all read (2)', 'Archive selected (0)']);
    await expect(bulk(page).getByRole('button', { name: /^Archive selected/ })).toBeDisabled();

    // Every row's parts, under All: glyph, sender (a rostered name, the
    // literal 'system' as System, anyone else as the raw id), age off
    // the app clock, subject, body, and the controls an unread row adds.
    await filter(page, 'All (5)').click();
    await expect(subjects(page)).toHaveText([
      'Review the design', 'Approve the payout', 'Train departed', 'Lunch on Friday', 'Feedback closed',
    ]);
    await expect(page.locator('.inbox-row .inbox-kind')).toHaveText(['✉', '✉', '⚡', '✉', '⚡']);
    await expect(page.locator('.inbox-row .inbox-sender')).toHaveText([
      'Bo Cellar', 'System', 'System', 'Bo Cellar', 'automation:dispatcher',
    ]);
    await expect(page.locator('.inbox-row .inbox-age')).toHaveText(['just now', '5h', '1d', '2d', '3d']);
    await expect(page.locator('.inbox-row .inbox-body')).toHaveText([
      'The review step is ready.', 'A payout waits on your sign-off.', 'Three cars rode.', 'Tacos?',
      'Your feedback reached its terminal.',
    ]);
    await expect(page.locator('.inbox-row-unread .inbox-subject')).toHaveText([
      'Review the design', 'Approve the payout', 'Feedback closed',
    ]);
    for (const m of INBOX) {
      const controls = m.read_at === null ? ['Mark read', 'Archive'] : ['Archive'];
      await expect(row(page, m.subject).getByRole('button')).toHaveText(controls);
      await expect(row(page, m.subject).getByRole('checkbox', { name: `Select ${m.subject}` })).not.toBeChecked();
    }
    // A ref with a path is a link; one without is its words, no link.
    await expect(page.locator('.inbox-entity')).toHaveText(['step: step-9', 'job: job-2', 'job: job-3']);
    await expect(list(page).getByRole('link')).toHaveText(['step: step-9', 'job: job-2']);

    // Two reads of the roster — the shell's session (which resolves the
    // viewer from it) and the page's — and one of the inbox. Gap 3
    // (74da899d): the inbox read asks for everything, no limit, no page.
    expect(await settledReads(page, () => seen.roster, 2)).toBe(2);
    expect(backend.inboxReads.map((u) => new URL(u).pathname + new URL(u).search))
      .toEqual(['/api/messages/inbox/emp-001']);
    expect(seen.writes).toHaveLength(0);
  });

  test('each filter button shows its rows and says it is pressed', async ({ page }) => {
    await open(page);

    const views: ReadonlyArray<readonly [string, ReadonlyArray<string>]> = [
      ['All (5)', ['Review the design', 'Approve the payout', 'Train departed', 'Lunch on Friday', 'Feedback closed']],
      ['Unread (3)', ['Review the design', 'Approve the payout', 'Feedback closed']],
      ['Direct (3)', ['Review the design', 'Approve the payout', 'Lunch on Friday']],
      ['Signals (2)', ['Train departed', 'Feedback closed']],
      ['Waiting on you (2)', ['Review the design', 'Approve the payout']],
    ];
    for (const [name, shown] of views) {
      await filter(page, name).click();
      await expect(filters(page).locator('button[aria-pressed="true"]')).toHaveText([name]);
      await expect(subjects(page)).toHaveText([...shown]);
      // The bulk bar follows the view: Mark all read counts its unread.
      const unread = INBOX.filter((m) => shown.includes(m.subject) && m.read_at === null).length;
      await expect(bulk(page).getByRole('button', { name: /^Mark all read/ })).toHaveText(`Mark all read (${unread})`);
    }
    // A filter narrows the rows only; the header counts the inbox.
    await expectHeader(page, TITLE, SUBTITLE);
  });

  test('the search narrows on subject, body and sender — as shown, or by id — case-insensitively, and sends nothing', async ({ page }) => {
    const seen = watch(page);
    const backend = await open(page);
    await filter(page, 'All (5)').click();
    const before = backend.inboxReads.length;

    const search = searchbox(page);
    await search.fill('REVIEW');
    await expect(subjects(page)).toHaveText(['Review the design']);
    await search.fill('tacos');
    await expect(subjects(page)).toHaveText(['Lunch on Friday']);
    await search.fill('emp-002');
    await expect(subjects(page)).toHaveText(['Review the design', 'Lunch on Friday']);
    await search.fill('automation:');
    await expect(subjects(page)).toHaveText(['Feedback closed']);
    // (a) The row shows the sender's NAME, and the name a viewer can see
    // finds the rows it is shown on — 'System' included.
    await search.fill('Cellar');
    await expect(subjects(page)).toHaveText(['Review the design', 'Lunch on Friday']);
    await search.fill('system');
    await expect(subjects(page)).toHaveText(['Approve the payout', 'Train departed']);
    await search.fill('no such words');
    await expect(rows(page)).toHaveCount(0);
    await expect(empty(page)).toHaveText('No messages match those filters.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    // The search and the filter compose.
    await search.fill('emp-002');
    await filter(page, 'Waiting on you (2)').click();
    await expect(subjects(page)).toHaveText(['Review the design']);
    await search.fill('');
    await expect(subjects(page)).toHaveText(['Review the design', 'Approve the payout']);

    // Client-side only.
    expect(backend.inboxReads).toHaveLength(before);
    expect(seen.writes).toHaveLength(0);
  });

  test('gap 3 (74da899d): every row the read returns is rendered, in one list', async ({ page }) => {
    const many: Msg[] = Array.from({ length: 120 }, (_, i) => msg({
      id: `msg-${i}`, sender_id: 'system', kind: 'signal', subject: `Signal ${i}`, body: '',
      sent_at: ago(i), read_at: ago(0), entity_ref: null,
    }));
    await install(page, { inbox: many });
    await mountPage(page, PATH, { titleMatch: /Nothing is waiting on you/ });

    await expectHeader(page, 'Nothing is waiting on you', '0 unread · 0 direct · 120 signals · 120 total');
    await filter(page, 'All (120)').click();
    await expect(rows(page)).toHaveCount(120);
  });
});

test.describe('/ux/inbox — rows: Mark read, Archive, and the bulk bar', () => {
  test('Mark read marks the row read, and a refused one says why on the row', async ({ page }) => {
    const backend = await open(page, { refuse: [JOB.id] });

    await row(page, 'Review the design').getByRole('button', { name: 'Mark read' }).click();
    await expectHeader(page, '1 waiting on you', '2 unread · 3 direct · 2 signals · 5 total');
    await expect(subjects(page)).toHaveText(['Approve the payout']);

    await row(page, 'Approve the payout').getByRole('button', { name: 'Mark read' }).click();
    const refused = row(page, 'Approve the payout').locator('p.inbox-write-refused[role=alert]');
    await expect(refused).toHaveText('Not marked read — HTTP 403: not your message');
    await expect(page.locator('h1.exec-title')).toHaveText('1 waiting on you');
    expect(backend.writes).toEqual([`read ${STEP.id}`, `read ${JOB.id}`]);
  });

  test('Archive takes the row out, and a refused one keeps it and says why', async ({ page }) => {
    const backend = await open(page, { refuse: [JOB.id] });

    await row(page, 'Review the design').getByRole('button', { name: 'Archive', exact: true }).click();
    await expect(row(page, 'Review the design')).toHaveCount(0);
    await expectHeader(page, '1 waiting on you', '2 unread · 2 direct · 2 signals · 4 total');

    await row(page, 'Approve the payout').getByRole('button', { name: 'Archive', exact: true }).click();
    await expect(row(page, 'Approve the payout').locator('p.inbox-write-refused[role=alert]'))
      .toHaveText('Not archived — HTTP 403: not your message');
    await expect(page.locator('h1.exec-title')).toHaveText('1 waiting on you');
    expect(backend.writes).toEqual([`archive ${STEP.id}`, `archive ${JOB.id}`]);
  });

  test('Mark all read marks what is shown, and says how many landed', async ({ page }) => {
    const backend = await open(page, { refuse: [JOB.id] });

    await bulk(page).getByRole('button', { name: 'Mark all read (2)' }).click();
    const note = list(page).locator('p.inbox-bulk-note');
    await expect(note).toHaveText('Marked 1 of 2 read — 1 refused; each row says why.');
    await expect(note).toHaveAttribute('role', 'alert');
    await expect(row(page, 'Approve the payout').locator('.inbox-write-refused'))
      .toHaveText('Not marked read — HTTP 403: not your message');
    // Only what the view showed: the unread SIGNAL was not shown.
    expect(backend.writes).toEqual([`read ${STEP.id}`, `read ${JOB.id}`]);
  });

  test('an admitted bulk write is a status, not an alert', async ({ page }) => {
    await open(page);

    await bulk(page).getByRole('button', { name: 'Mark all read (2)' }).click();
    const note = list(page).locator('p.inbox-bulk-note');
    await expect(note).toHaveText('Marked 2 of 2 read.');
    await expect(note).toHaveAttribute('role', 'status');
    await expect(page.locator('h1.exec-title')).toHaveText('Nothing is waiting on you');
    // The note stays above the list the write emptied.
    await expect(empty(page)).toHaveText('No messages match those filters.');
  });

  test('the checkboxes choose what Archive selected archives, and Select all shown takes the view', async ({ page }) => {
    const backend = await open(page);
    await filter(page, 'Signals (2)').click();

    const archive = bulk(page).getByRole('button', { name: /^Archive selected/ });
    const all = bulk(page).getByRole('checkbox', { name: 'Select all shown' });
    await all.check();
    await expect(archive).toHaveText('Archive selected (2)');
    await all.uncheck();
    await expect(archive).toHaveText('Archive selected (0)');
    await expect(archive).toBeDisabled();

    await row(page, 'Train departed').getByRole('checkbox').check();
    await expect(all).not.toBeChecked();
    await expect(archive).toHaveText('Archive selected (1)');
    await archive.click();
    await expect(list(page).locator('p.inbox-bulk-note')).toHaveText('Archived 1 of 1.');
    await expect(subjects(page)).toHaveText(['Feedback closed']);
    await expect(archive).toHaveText('Archive selected (0)');
    expect(backend.writes).toEqual([`archive ${READ_SIGNAL.id}`]);
  });
});

test.describe('/ux/inbox — links and back', () => {
  test('the step link marks the message read, opens the step, and back returns to the inbox', async ({ page }) => {
    const backend = await open(page);

    // The producers write /jobs/{id} and /jobs/{id}/steps/{step}: the
    // router's job and step routes, reached through its unprefixed-path
    // fallback. The catalog lists the jobs surface they sit under and
    // no detail entry for either (the same shape as /ux/people/:id).
    expect(ROUTE_CATALOG.inbox.path).toBe(PATH);
    expect(ROUTE_CATALOG.inbox.label).toBe('Inbox');
    expect(ROUTE_CATALOG.inbox.app).toBe('home');
    expect(parseRoute('/jobs/job-1/steps/step-9')).toMatchObject({ kind: 'stepFocus', jobId: 'job-1', stepId: 'step-9' });
    expect(parseRoute('/jobs/job-2')).toMatchObject({ kind: 'jobDetail', jobId: 'job-2' });
    expect(parseRoute(ROUTE_CATALOG.jobs.path)).toMatchObject({ kind: 'jobs' });

    const link = list(page).getByRole('link', { name: 'step: step-9' });
    await expect(link).toHaveAttribute('href', '/jobs/job-1/steps/step-9');
    await link.click();
    await expect.poll(() => new URL(page.url()).pathname).toBe('/jobs/job-1/steps/step-9');
    expect(backend.writes).toEqual([`read ${STEP.id}`]);

    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expectHeader(page, '1 waiting on you', '2 unread · 3 direct · 2 signals · 5 total');
  });

  test('the job link opens the job, and back returns to the inbox', async ({ page }) => {
    await open(page);

    const link = list(page).getByRole('link', { name: 'job: job-2' });
    await expect(link).toHaveAttribute('href', '/jobs/job-2');
    await link.click();
    await expect.poll(() => new URL(page.url()).pathname).toBe('/jobs/job-2');

    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(page.locator('h1.exec-title')).toHaveText('1 waiting on you');
  });

  test('a refused link write stays here, says why, and offers the link without the write', async ({ page }) => {
    const backend = await open(page, { refuse: [JOB.id] });

    await list(page).getByRole('link', { name: 'job: job-2' }).click();
    await expect(row(page, 'Approve the payout').locator('.inbox-write-refused'))
      .toHaveText('Not marked read — HTTP 403: not your message');
    expect(new URL(page.url()).pathname).toBe(PATH);

    const anyway = row(page, 'Approve the payout').getByRole('link', { name: 'Open without marking read' });
    await expect(anyway).toHaveAttribute('href', '/jobs/job-2');
    await anyway.click();
    await expect.poll(() => new URL(page.url()).pathname).toBe('/jobs/job-2');
    expect(backend.writes).toEqual([`read ${JOB.id}`]);

    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
  });
});

test.describe('/ux/inbox — the composer', () => {
  test('Compose opens the form; ✕, Cancel and the backdrop each close it, and what was typed stays', async ({ page }) => {
    await open(page);
    const compose = page.getByRole('button', { name: 'Compose', exact: true });

    await compose.click();
    await expect(modal(page).locator('.compose-title')).toHaveText('New Message');
    await expect(modal(page).locator('label')).toHaveText(['To', 'Subject', 'Message']);
    // (e) Each role by its registry name, not its code.
    await expect(page.locator('#inbox-to option')).toHaveText([
      'Select recipient...', 'David (Platform administrator)', 'Bo Cellar (Sales lead)',
    ]);
    await expect(page.locator('#inbox-subject')).toHaveAttribute('placeholder', 'Subject...');
    await expect(page.locator('#inbox-body')).toHaveAttribute('placeholder', 'Write your message...');
    await expect(modal(page).getByRole('button')).toHaveText(['✕', 'Send', 'Cancel']);

    await page.locator('#inbox-subject').fill('Kept');
    await modal(page).getByRole('button', { name: '✕' }).click();
    await expect(modal(page)).toHaveCount(0);

    await compose.click();
    await expect(page.locator('#inbox-subject')).toHaveValue('Kept');
    await modal(page).getByRole('button', { name: 'Cancel' }).click();
    await expect(modal(page)).toHaveCount(0);

    await compose.click();
    await page.locator('.compose-overlay').click({ position: { x: 5, y: 5 } });
    await expect(modal(page)).toHaveCount(0);
    // A click inside the dialog is not a click on the backdrop.
    await compose.click();
    await modal(page).locator('.compose-title').click();
    await expect(modal(page)).toHaveCount(1);
  });

  test('Send is disabled until To, Subject and Message are filled, says Sending while out, and sends the form', async ({ page }) => {
    const backend = await open(page);
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => { release = resolve; });
    let sent: unknown = null;
    await page.route(SEND, async (r) => {
      sent = r.request().postDataJSON();
      await held;
      await json(r, { id: 'msg-new' });
    });

    await page.getByRole('button', { name: 'Compose', exact: true }).click();
    const send = modal(page).getByRole('button', { name: 'Send' });
    await expect(send).toBeDisabled();
    await page.locator('#inbox-to').selectOption(BO.id);
    await expect(send).toBeDisabled();
    await page.locator('#inbox-subject').fill('Hello');
    await expect(send).toBeDisabled();
    await page.locator('#inbox-body').fill('A message.');
    await expect(send).toBeEnabled();

    const reads = backend.inboxReads.length;
    await send.click();
    await expect(modal(page).getByRole('button', { name: 'Sending...' })).toBeDisabled();
    // No kind: the server defaults a composed message to direct.
    expect(sent).toEqual({ sender_id: DAVID.id, recipient_id: BO.id, subject: 'Hello', body: 'A message.' });
    release();
    await expect(modal(page)).toHaveCount(0);
    await expect.poll(() => backend.inboxReads.length).toBe(reads + 1);

    // An admitted Send clears the form.
    await page.getByRole('button', { name: 'Compose', exact: true }).click();
    await expect(page.locator('#inbox-to')).toHaveValue('');
    await expect(page.locator('#inbox-subject')).toHaveValue('');
    await expect(page.locator('#inbox-body')).toHaveValue('');
  });

  test('a refused Send says why in the modal, keeps the text, and a reopened composer starts without the line', async ({ page }) => {
    await open(page);
    await page.route(SEND, (r) => r.fulfill({ status: 400, body: 'recipient is not a person' }));

    await page.getByRole('button', { name: 'Compose', exact: true }).click();
    await page.locator('#inbox-to').selectOption(BO.id);
    await page.locator('#inbox-subject').fill('Hello');
    await page.locator('#inbox-body').fill('A message.');
    await modal(page).getByRole('button', { name: 'Send' }).click();

    await expect(modal(page).locator('p.compose-refused[role=alert]'))
      .toHaveText('Not sent — HTTP 400: recipient is not a person');
    await expect(page.locator('#inbox-body')).toHaveValue('A message.');
    await expect(modal(page).getByRole('button', { name: 'Send' })).toBeEnabled();

    await modal(page).getByRole('button', { name: 'Cancel' }).click();
    await page.getByRole('button', { name: 'Compose', exact: true }).click();
    await expect(modal(page).locator('.compose-refused')).toHaveCount(0);
    await expect(page.locator('#inbox-subject')).toHaveValue('Hello');
  });

  test('a Send that never reaches the server is said the same way', async ({ page }) => {
    await open(page);
    await page.route(SEND, (r) => r.abort('connectionrefused'));

    await page.getByRole('button', { name: 'Compose', exact: true }).click();
    await page.locator('#inbox-to').selectOption(BO.id);
    await page.locator('#inbox-subject').fill('Hello');
    await page.locator('#inbox-body').fill('A message.');
    await modal(page).getByRole('button', { name: 'Send' }).click();
    await expect(modal(page).locator('p.compose-refused')).toHaveText('Not sent — Failed to fetch');
  });

  test('gap 6 (7d1c11a3): a failed roster read empties the To list and names senders by id, and says nothing', async ({ page }) => {
    // The session reads the roster first and resolves the viewer; the
    // page's own read of it is the second, and it is the one refused.
    let reads = 0;
    await open(page, {
      roster: (r: Route) => (++reads === 1 ? json(r, ROSTER) : json(r, 'people store down', 500)),
    });
    await expect.poll(() => reads).toBe(2);

    await filter(page, 'All (5)').click();
    await expect(page.locator('.inbox-row .inbox-sender')).toHaveText([
      'emp-002', 'System', 'System', 'emp-002', 'automation:dispatcher',
    ]);
    await page.getByRole('button', { name: 'Compose', exact: true }).click();
    await expect(page.locator('#inbox-to option')).toHaveText(['Select recipient...']);
    await page.locator('#inbox-subject').fill('Hello');
    await page.locator('#inbox-body').fill('A message.');
    await expect(modal(page).getByRole('button', { name: 'Send' })).toBeDisabled();
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(page.getByRole('alert')).toHaveCount(0);
  });
});

test.describe('/ux/inbox — empty, loading and failed reads never paint alike', () => {
  test('an empty inbox says nothing is waiting, and counts zeros it read', async ({ page }) => {
    await install(page, { inbox: [] });
    await mountPage(page, PATH, { titleMatch: /Nothing is waiting on you/ });

    await expectHeader(page, 'Nothing is waiting on you', '0 unread · 0 direct · 0 signals · 0 total');
    await expect(filters(page).getByRole('button')).toHaveText([
      'Waiting on you (0)', 'All (0)', 'Unread (0)', 'Direct (0)', 'Signals (0)',
    ]);
    await expect(empty(page)).toHaveText('No messages match those filters.');
    await expect(bulk(page)).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('while the inbox loads the page claims neither a count nor an empty inbox', async ({ page }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => { release = resolve; });
    await install(page, { inbox: async (r) => { await held; await json(r, INBOX); } });
    await mountPage(page, PATH, { titleMatch: /^Inbox$/ });

    await expectHeader(page, 'Inbox', 'Loading…');
    await expect(empty(page)).toHaveText('Loading…');
    await expect(page.getByText('Nothing is waiting on you')).toHaveCount(0);
    // (b) Nor on the buttons: a count is a claim about a read that landed.
    await expect(filters(page).getByRole('button')).toHaveText([
      'Waiting on you', 'All', 'Unread', 'Direct', 'Signals',
    ]);
    release();
    await expectHeader(page, TITLE, SUBTITLE);
  });

  // The line is the page's words verbatim: a status names the read's
  // path and HTTP code; an unreachable server is the browser's own
  // message (Chromium's); a body that is not JSON is the parser's,
  // whose wording is V8's and so is matched only past the page's own
  // prefix.
  const FAILURES: ReadonlyArray<readonly [string, (r: Route) => Promise<void>, string | RegExp]> = [
    ['refused (500)', (r) => json(r, 'message store down', 500),
      "Couldn't load your inbox — /api/messages/inbox/emp-001: HTTP 500"],
    ['forbidden (403)', (r) => json(r, 'forbidden', 403),
      "Couldn't load your inbox — /api/messages/inbox/emp-001: HTTP 403"],
    ['unreachable', (r) => r.abort('connectionrefused'), "Couldn't load your inbox — Failed to fetch"],
    ['not JSON', (r) => r.fulfill({ status: 200, contentType: 'application/json', body: 'not json' }),
      /^\s*Couldn't load your inbox — \S.*\S\s*$/],
    // (c) A changed response shape — an envelope where the list was due
    // — used to be coerced to [] and read "Nothing is waiting on you".
    ['a 200 that is not a list', (r) => json(r, { data: [STEP] }),
      "Couldn't load your inbox — /api/messages/inbox/emp-001: HTTP 200, but the body is an object, not a list"],
    ['a 200 that is null', (r) => json(r, null),
      "Couldn't load your inbox — /api/messages/inbox/emp-001: HTTP 200, but the body is null, not a list"],
  ];
  for (const [how, answer, line] of FAILURES) {
    test(`an inbox read that is ${how} paints the failure line and Retry, never the empty one`, async ({ page }) => {
      await install(page, { inbox: answer });
      await mountPage(page, PATH);

      const failed = list(page).locator(`p${FAILURE_MARKER}[role=alert]`);
      await expect(failed).toHaveText(line);
      // Read, not reached: a 403 and a 200 of the wrong shape each came
      // from a store that answered (e2679b23 (c)).
      await expectHeader(page, 'Inbox', 'Your inbox could not be read.');
      await expect(page.getByText('Nothing is waiting on you')).toHaveCount(0);
      await expect(page.getByText('No messages match those filters.')).toHaveCount(0);
      await expect(list(page).getByRole('button', { name: 'Retry' })).toBeVisible();
      await expect(rows(page)).toHaveCount(0);
      // (b) The filter buttons print no counts: zeros here would be the
      // empty inbox's words on a read that failed.
      await expect(filters(page).getByRole('button')).toHaveText([
        'Waiting on you', 'All', 'Unread', 'Direct', 'Signals',
      ]);
    });
  }

  test('Retry reads again, and a recovered store paints the inbox', async ({ page }) => {
    let up = false;
    const backend = await install(page, {
      inbox: (r) => (up ? json(r, INBOX) : json(r, 'message store down', 500)),
    });
    await mountPage(page, PATH);
    await expect(list(page).locator(FAILURE_MARKER)).toBeVisible();

    up = true;
    await list(page).getByRole('button', { name: 'Retry' }).click();
    await expectHeader(page, TITLE, SUBTITLE);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    expect(backend.inboxReads).toHaveLength(2);
  });
});

test.describe('/ux/inbox — a read-only guest', () => {
  test('Compose, the bulk bar and every row control are behind the gate, and a link is only a navigation', async ({ page }) => {
    const backend = await install(page, { session: { username: 'guest-7', role: 'audit-readonly' } });
    await mountPage(page, PATH, { titleMatch: /waiting on you/ });

    await expect(page.getByRole('button', { name: 'Compose', exact: true })).toBeDisabled();
    await expect(bulk(page).getByRole('button', { name: /^Mark all read/ })).toBeDisabled();
    await expect(bulk(page).getByRole('checkbox', { name: 'Select all shown' })).toBeDisabled();
    // One note for the composer, one for the list: the bulk bar and the
    // rows share a gate, so the list does not repeat it per row.
    await expect(page.locator('p.write-gate-note')).toHaveText([
      'Read-only session — sign in to act.', 'Read-only session — sign in to act.',
    ]);
    // (d) The rows' own writes were outside the gate: a guest could
    // press each, and each was a 403 waiting for the click.
    for (const subject of ['Review the design', 'Approve the payout']) {
      await expect(row(page, subject).getByRole('button', { name: 'Mark read' })).toBeDisabled();
      await expect(row(page, subject).getByRole('button', { name: 'Archive', exact: true })).toBeDisabled();
      await expect(row(page, subject).getByRole('checkbox')).toBeDisabled();
    }
    // A link stays a link — a guest may read what a message points at —
    // but it no longer tries to mark the message read on the way.
    await list(page).getByRole('link', { name: 'job: job-2' }).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe('/jobs/job-2');
    expect(backend.writes).toEqual([]);
  });
});
