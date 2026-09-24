// An account's Notes & interactions panel — naming who wrote each note.
//
// Backlog 1e73bd93 (the last car). NotesPanel read the WHOLE employee
// roster to name the authors of the ten notes it shows, and a refusal
// or a network error was dropped (`if (!r.ok) return`, `catch { //
// Ignore. }`), so the authors silently became raw ids. It now reads one
// row per PERSON among the notes shown through the shared reader
// (src/data/ownerNames.ts) — never a machine author, which has no
// people row — and says so when a name cannot load.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const ID = 'acct-notes';
const PATH = `/ux/accounts/${ID}`;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const paged = (data: ReadonlyArray<unknown>) => ({ data, total: data.length, limit: 500, offset: 0 });

const note = (id: string, actor_id: string, body: string) => ({
  id, account_id: ID, actor_id, body, kind: 'note', created_at: '2026-09-20T12:00:00Z', deleted_at: null,
});

/// One note by a person, one by an agent. The agent must never be
/// looked up — `agent-claude` has no people row.
const NOTES = [
  note('n-1', 'emp-rep-1', 'Called about the keg order'),
  note('n-2', 'agent-claude', 'Drafted the follow-up email'),
];

/// Every read the account page makes, answered; `person` decides the
/// one under test. The ROSTER names the rep differently, so a panel
/// that still reads the whole roster shows the wrong name and fails the
/// first test rather than passing it by accident.
async function install(page: Page, person: (r: Route) => Promise<void>): Promise<string[]> {
  await installSmokeMocks(page);
  await page.route(new RegExp(`/api/people/accounts/${ID}$`), (r) =>
    json(r, { id: ID, name: 'Notes Taproom', director: null, city: null, state: null, tier: null, customer_since: null, territory_rep_id: null }),
  );
  for (const re of [
    new RegExp(`/api/assets\\?account_id=${ID}&`),
    new RegExp(`/api/commerce/invoices\\?account_id=${ID}&`),
    new RegExp(`/api/jobs\\?subject_id=${ID}&`),
    new RegExp(`/api/shipping/shipments\\?account_id=${ID}&`),
  ]) {
    await page.route(re, (r) => json(r, paged([])));
  }
  await page.route(new RegExp(`/api/people/accounts/${ID}/notes\\?`), (r) => json(r, NOTES));
  await page.route(/\/api\/people$/, (r) => json(r, [{ id: 'emp-rep-1', name: 'Roster Copy' }]));
  await page.route(/\/api\/people\/emp-rep-1$/, person);
  const asked: string[] = [];
  page.on('request', (req) => {
    const path = new URL(req.url()).pathname;
    if (/^\/api\/people\/(emp|agent)-[^/]+$/.test(path)) asked.push(path);
  });
  return asked;
}

const panel = (page: Page) => page.locator('section', { has: page.getByRole('heading', { name: /^Notes & interactions/ }) });
const authorOf = (page: Page, body: string) =>
  page.locator('.pp-note-item', { hasText: body }).locator('.pp-note-item-author');

test.describe('an account\'s notes — naming who wrote them', () => {
  test('answered, a person is named from their own row, an agent is never looked up, and nothing says a read failed', async ({ page }) => {
    const asked = await install(page, (r) => json(r, { id: 'emp-rep-1', name: 'Rhea Okafor' }));
    await mountPage(page, PATH);

    await expect(authorOf(page, 'Called about the keg order')).toHaveText('Rhea Okafor');
    await expect(authorOf(page, 'Drafted the follow-up email')).toContainText('claude');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    expect([...new Set(asked)]).toEqual(['/api/people/emp-rep-1']);
  });

  test('refused, the author keeps the id and the panel says the names did not load', async ({ page }) => {
    await install(page, (r) => json(r, { error: 'people down' }, 503));
    await mountPage(page, PATH);

    await expect(authorOf(page, 'Called about the keg order')).toHaveText('emp-rep-1');
    await expect(panel(page).locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      "Couldn't load who wrote these notes — /api/people/emp-rep-1: HTTP 503. Authors show as ids.",
    );
  });
});
