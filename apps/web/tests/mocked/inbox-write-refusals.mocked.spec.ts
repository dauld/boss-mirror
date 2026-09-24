// /ux/inbox — a refused write is said on the page, never swallowed
// (page audit 5477d9eb, 2026-09-23; backlog 129da587 and 2a6fab80).
//
// Before this car the inbox's two writes each dropped their answer:
//
//   Mark read (GAP 5, 129da587) — `markRead` never read `r.ok` and
//   caught every throw into nothing, so the re-read showed the row
//   still unread with no reason. The entity link fired the same write
//   unawaited and navigated away before any answer could show.
//
//   Send (GAP 4, 2a6fab80) — `send` read `r.ok` only to close the
//   modal: a 4xx/5xx left it open with no line saying why, and a
//   network throw (try/finally, no catch) was an unhandled rejection.
//
// The generic crawls reach this route and passed the whole time: a
// crawl asserts that a page renders, and a swallowed refusal renders
// fine. These tests are the assertion a crawl cannot make — the same
// click against a refusing backend and against an admitting one, and
// the two must not look alike.

import { expect, test, type Page, type Route } from '@playwright/test';
import { installApiFloor } from './_smokeMocks';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const PATH = ROUTE_CATALOG.inbox.path;

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const EMP = { id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

/// Where the message's entity link points. Any SPA route the floor
/// can answer; the claim is only about whether the page goes there.
const ENTITY_PATH = ROUTE_CATALOG.people.path;

const MSG = {
  id: 'msg-1', sender_id: 'system', recipient_id: EMP.id, kind: 'direct',
  subject: 'A thing needs you', body: 'Please look at the thing.',
  sent_at: '2026-08-20T10:00:00Z', read_at: null,
  entity_ref: { entity_type: 'employee', entity_id: EMP.id, entity_path: ENTITY_PATH },
};

const READ = /\/api\/messages\/msg-1\/read$/;
const SEND = /\/api\/messages\/send$/;

/// The persona and a one-message inbox. `read` flips once a Mark read
/// is ADMITTED, so an admitted write re-reads as read — the difference
/// between the two legs is the backend's answer, nothing else.
async function inboxMocks(page: Page): Promise<{ read: () => boolean; markRead: () => void }> {
  let read = false;
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });
  await installApiFloor(page);
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: 'platform-admin' }));
  await page.route(/\/api\/messages\/inbox\//, (r) =>
    json(r, [{ ...MSG, read_at: read ? '2026-08-20T11:00:00Z' : null }]));
  return { read: () => read, markRead: () => { read = true; } };
}

async function mount(page: Page): Promise<void> {
  await page.goto(PATH);
  await expect(page.getByText('1 waiting on you')).toBeVisible();
}

// ---- Mark read --------------------------------------------------------

test('a refused Mark read names the refusal on the row, which stays unread', async ({ page }) => {
  await inboxMocks(page);
  await page.route(READ, (r) => r.fulfill({ status: 403, body: 'not your message' }));
  await mount(page);

  await page.getByRole('button', { name: 'Mark read' }).click();
  const refused = page.locator('.inbox-write-refused');
  await expect(refused).toBeVisible();
  await expect(refused).toContainText('HTTP 403');
  await expect(refused).toContainText('not your message');
  // Still unread, and now the page says why.
  await expect(page.getByText('1 waiting on you')).toBeVisible();
});

test('a Mark read that never reaches the server is said the same way', async ({ page }) => {
  await inboxMocks(page);
  await page.route(READ, (r) => r.abort('internetdisconnected'));
  await mount(page);

  await page.getByRole('button', { name: 'Mark read' }).click();
  await expect(page.locator('.inbox-write-refused')).toBeVisible();
  await expect(page.getByText('1 waiting on you')).toBeVisible();
});

test('an admitted Mark read clears the row and says nothing', async ({ page }) => {
  const backend = await inboxMocks(page);
  await page.route(READ, (r) => { backend.markRead(); return r.fulfill({ status: 204 }); });
  await mount(page);

  await page.getByRole('button', { name: 'Mark read' }).click();
  await expect(page.getByText('Nothing is waiting on you')).toBeVisible();
  await expect(page.locator('.inbox-write-refused')).toHaveCount(0);
});

// ---- the entity link ---------------------------------------------------

test('the entity link waits for its Mark read before it leaves', async ({ page }) => {
  const backend = await inboxMocks(page);
  // Hold the write until the test has read where the page is.
  let release: () => void = () => {};
  const held = new Promise<void>((resolve) => { release = resolve; });
  await page.route(READ, async (r) => {
    await held;
    backend.markRead();
    await r.fulfill({ status: 204 });
  });
  await mount(page);

  const write = page.waitForRequest(READ);
  await page.locator('.inbox-entity-link').click();
  await write;
  // The write is out and unanswered: an unawaited link has already
  // navigated by now; an awaited one is still here.
  expect(new URL(page.url()).pathname).toBe(PATH);
  release();
  await expect(page).toHaveURL(new RegExp(`${ENTITY_PATH}$`));
  expect(backend.read()).toBe(true);
});

test('a refused Mark read on the entity link stays on the inbox, says why, and offers the link without the write', async ({ page }) => {
  await inboxMocks(page);
  let writes = 0;
  await page.route(READ, (r) => { writes += 1; return r.fulfill({ status: 403, body: 'not your message' }); });
  await mount(page);

  await page.locator('.inbox-entity-link').click();
  const refused = page.locator('.inbox-write-refused');
  await expect(refused).toContainText('HTTP 403');
  expect(new URL(page.url()).pathname).toBe(PATH);

  // The viewer still gets where they were going — by a link that says
  // it does not mark the message read, and does not try to.
  await page.getByRole('link', { name: 'Open without marking read' }).click();
  await expect(page).toHaveURL(new RegExp(`${ENTITY_PATH}$`));
  expect(writes).toBe(1);
});

// ---- Send ---------------------------------------------------------------

async function compose(page: Page): Promise<void> {
  await page.getByRole('button', { name: 'Compose' }).click();
  await page.locator('#inbox-to').selectOption(EMP.id);
  await page.locator('#inbox-subject').fill('Hello');
  await page.locator('#inbox-body').fill('A message.');
}

test('a refused Send keeps the modal open and names the refusal in it', async ({ page }) => {
  await inboxMocks(page);
  await page.route(SEND, (r) => r.fulfill({ status: 400, body: 'recipient is not a person' }));
  await mount(page);
  await compose(page);

  await page.getByRole('button', { name: 'Send' }).click();
  const refused = page.locator('.compose-refused');
  await expect(refused).toContainText('HTTP 400');
  await expect(refused).toContainText('recipient is not a person');
  // Nothing typed is lost: the modal and its fields are still there.
  await expect(page.locator('#inbox-subject')).toHaveValue('Hello');
});

test('a Send that never reaches the server is said in the modal — not an unhandled rejection', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', (e) => errors.push(e.message));
  await inboxMocks(page);
  await page.route(SEND, (r) => r.abort('internetdisconnected'));
  await mount(page);
  await compose(page);

  await page.getByRole('button', { name: 'Send' }).click();
  await expect(page.locator('.compose-refused')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Send' })).toBeEnabled();
  expect(errors).toEqual([]);
});

test('an admitted Send closes the modal with no refusal line', async ({ page }) => {
  await inboxMocks(page);
  await page.route(SEND, (r) => json(r, { id: 'msg-2' }));
  await mount(page);
  await compose(page);

  await page.getByRole('button', { name: 'Send' }).click();
  await expect(page.locator('.compose-modal')).toHaveCount(0);
  await expect(page.locator('.compose-refused')).toHaveCount(0);
});
