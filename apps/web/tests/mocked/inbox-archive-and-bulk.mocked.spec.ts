// /ux/inbox — a way out for finished messages (page audit 5477d9eb,
// GAP 8; backlog 5963a322), and the archived rows kept off the page
// (GAP 2; backlog 8578b91e).
//
// Before this car the only exit on the page was one Mark read at a
// time, used once in the audit window against 201 messages to the one
// human recipient. POST /api/messages/{id}/archive already existed and
// the page never called it; nothing on the page could act on more than
// one row. These tests drive a small backend whose rows answer the
// page's writes — Archive drops the row from the next inbox read, Mark
// read sets `read_at` — and a refusal of either must be said on its
// row, with a bulk write's summary saying how many landed.

import { expect, test, type Page, type Route } from '@playwright/test';
import { installApiFloor, servePeopleRows } from './_smokeMocks';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const PATH = ROUTE_CATALOG.inbox.path;

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const EMP = { id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

type Row = { id: string; subject: string; read_at: string | null };

const message = (m: Row) => ({
  id: m.id, sender_id: 'system', recipient_id: EMP.id, kind: 'direct',
  subject: m.subject, body: `Body of ${m.subject}.`,
  sent_at: '2026-08-20T10:00:00Z', read_at: m.read_at, entity_ref: null,
});

type Backend = {
  /// Ids each write reached, in order.
  archived: string[];
  read: string[];
  /// Every inbox read's URL, so a test can say what the page asked.
  inboxReads: string[];
};

/// A three-row inbox. `refuse` names the ids whose writes answer 403;
/// every other write is admitted and changes what the next read sees.
async function inboxBackend(page: Page, refuse: readonly string[] = []): Promise<Backend> {
  let rows: Row[] = [
    { id: 'msg-a', subject: 'Alpha', read_at: null },
    { id: 'msg-b', subject: 'Bravo', read_at: null },
    { id: 'msg-c', subject: 'Charlie', read_at: null },
  ];
  const backend: Backend = { archived: [], read: [], inboxReads: [] };
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });
  await installApiFloor(page);
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await servePeopleRows(page, [EMP]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: 'platform-admin' }));
  await page.route(/\/api\/messages\/inbox\//, (r) => {
    backend.inboxReads.push(r.request().url());
    return json(r, rows.map(message));
  });
  await page.route(/\/api\/messages\/[^/]+\/(archive|read)$/, (r) => {
    const [, id, verb] = /\/api\/messages\/([^/]+)\/(archive|read)$/.exec(r.request().url()) ?? [];
    if (refuse.includes(id)) return r.fulfill({ status: 403, body: 'not your message' });
    if (verb === 'archive') {
      backend.archived.push(id);
      rows = rows.filter((m) => m.id !== id);
    } else {
      backend.read.push(id);
      rows = rows.map((m) => (m.id === id ? { ...m, read_at: '2026-08-20T11:00:00Z' } : m));
    }
    return r.fulfill({ status: 204 });
  });
  return backend;
}

async function mount(page: Page): Promise<void> {
  await page.goto(PATH);
  await expect(page.getByText('3 waiting on you')).toBeVisible();
}

const row = (page: Page, subject: string) =>
  page.locator('.inbox-row').filter({ hasText: subject });

// ---- the inbox read ----------------------------------------------------

test('the page asks for the inbox without its archived rows', async ({ page }) => {
  const backend = await inboxBackend(page);
  await mount(page);
  expect(backend.inboxReads.length).toBeGreaterThan(0);
  expect(backend.inboxReads.every((u) => !u.includes('include_archived'))).toBe(true);
});

// ---- per-row Archive ---------------------------------------------------

test('Archive on a row takes it out of the inbox', async ({ page }) => {
  const backend = await inboxBackend(page);
  await mount(page);

  await row(page, 'Bravo').getByRole('button', { name: 'Archive', exact: true }).click();
  await expect(page.getByText('2 waiting on you')).toBeVisible();
  await expect(row(page, 'Bravo')).toHaveCount(0);
  await expect(page.locator('.inbox-write-refused')).toHaveCount(0);
  expect(backend.archived).toEqual(['msg-b']);
});

test('a refused Archive keeps the row and names the refusal on it', async ({ page }) => {
  await inboxBackend(page, ['msg-b']);
  await mount(page);

  await row(page, 'Bravo').getByRole('button', { name: 'Archive', exact: true }).click();
  const refused = row(page, 'Bravo').locator('.inbox-write-refused');
  await expect(refused).toContainText('Not archived');
  await expect(refused).toContainText('HTTP 403: not your message');
  await expect(page.getByText('3 waiting on you')).toBeVisible();
});

// ---- bulk: Mark all read -------------------------------------------------

test('Mark all read marks every shown unread row read', async ({ page }) => {
  const backend = await inboxBackend(page);
  await mount(page);

  await page.getByRole('button', { name: /^Mark all read/ }).click();
  await expect(page.getByText('Nothing is waiting on you')).toBeVisible();
  expect([...backend.read].sort()).toEqual(['msg-a', 'msg-b', 'msg-c']);
  await expect(page.locator('.inbox-bulk-note')).toContainText('Marked 3 of 3 read');
});

test('a Mark all read that half-lands says which half', async ({ page }) => {
  await inboxBackend(page, ['msg-c']);
  await mount(page);

  await page.getByRole('button', { name: /^Mark all read/ }).click();
  await expect(page.getByText('1 waiting on you')).toBeVisible();
  const note = page.locator('.inbox-bulk-note');
  await expect(note).toContainText('Marked 2 of 3 read');
  await expect(note).toContainText('1 refused');
  await expect(row(page, 'Charlie').locator('.inbox-write-refused'))
    .toContainText('HTTP 403: not your message');
});

// ---- bulk: Archive selected ----------------------------------------------

test('Archive selected archives exactly the checked rows', async ({ page }) => {
  const backend = await inboxBackend(page);
  await mount(page);

  const archive = page.getByRole('button', { name: /^Archive selected/ });
  await expect(archive).toBeDisabled();
  await page.getByRole('checkbox', { name: 'Select Alpha' }).check();
  await page.getByRole('checkbox', { name: 'Select Charlie' }).check();
  await expect(archive).toHaveText('Archive selected (2)');

  await archive.click();
  await expect(page.getByText('1 waiting on you')).toBeVisible();
  await expect(row(page, 'Bravo')).toHaveCount(1);
  expect(backend.archived).toEqual(['msg-a', 'msg-c']);
  await expect(page.locator('.inbox-bulk-note')).toContainText('Archived 2 of 2');
  // Nothing left selected: the archived rows are gone.
  await expect(archive).toHaveText('Archive selected (0)');
});

test('Select all shown checks every row the filter shows', async ({ page }) => {
  await inboxBackend(page);
  await mount(page);

  await page.getByRole('checkbox', { name: 'Select all shown' }).check();
  await expect(page.getByRole('button', { name: /^Archive selected/ }))
    .toHaveText('Archive selected (3)');
});
