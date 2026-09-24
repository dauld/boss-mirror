// The knowledge view's timeline — naming the people who acted.
//
// Backlog 1e73bd93. KnowledgeBaseView read the WHOLE employee roster to
// name the actors on an entity's timeline, and a refusal or a network
// error was dropped (`if (!r.ok) return`, `catch { /* ignore */ }`), so
// the actors silently became raw ids. It now reads one row per PERSON
// on the timeline through the shared reader (src/data/ownerNames.ts) —
// never a machine actor, which has no people row — and says so when a
// name cannot load. Reached through an account's Knowledge tab, the
// one surface that mounts the view.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const ID = 'acct-kb';
const PATH = `/ux/accounts/${ID}`;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const paged = (data: ReadonlyArray<unknown>) => ({ data, total: data.length, limit: 500, offset: 0 });

/// Two facts: one by a person, one by an agent. The agent must never
/// be looked up — `agent-claude` has no people row.
const FACTS = [
  { id: 'f-1', fact_kind: 'note', occurred_at: '2026-09-20', actor_id: 'emp-rep-1', payload: { summary: 'Called about the order' } },
  { id: 'f-2', fact_kind: 'note', occurred_at: '2026-09-21', actor_id: 'agent-claude', payload: { summary: 'Drafted a follow-up' } },
];

/// Every read the page makes, answered; `person` decides the one under
/// test. The ROSTER names the rep differently, so a view that still
/// reads the whole roster shows the wrong name and fails the first test
/// rather than passing it by accident.
async function install(page: Page, person: (r: Route) => Promise<void>): Promise<string[]> {
  await installSmokeMocks(page);
  await page.route(new RegExp(`/api/people/accounts/${ID}$`), (r) =>
    json(r, { id: ID, name: 'KB Taproom', director: null, city: null, state: null, tier: null, customer_since: null, territory_rep_id: null }),
  );
  for (const re of [
    new RegExp(`/api/assets\\?account_id=${ID}&`),
    new RegExp(`/api/commerce/invoices\\?account_id=${ID}&`),
    new RegExp(`/api/jobs\\?subject_id=${ID}&`),
    new RegExp(`/api/shipping/shipments\\?account_id=${ID}&`),
  ]) {
    await page.route(re, (r) => json(r, paged([])));
  }
  await page.route(new RegExp(`/api/people/accounts/${ID}/facts$`), (r) => json(r, FACTS));
  await page.route(/\/api\/people$/, (r) => json(r, [{ id: 'emp-rep-1', name: 'Roster Copy' }]));
  await page.route(/\/api\/people\/emp-rep-1$/, person);
  const asked: string[] = [];
  page.on('request', (req) => {
    const path = new URL(req.url()).pathname;
    if (/^\/api\/people\/(emp|agent)-[^/]+$/.test(path)) asked.push(path);
  });
  return asked;
}

async function openKnowledge(page: Page): Promise<void> {
  await mountPage(page, PATH);
  await page.getByRole('tab', { name: 'Knowledge' }).click();
}

const actorOf = (page: Page, summary: string) =>
  page.locator('.kb-timeline-item', { hasText: summary }).locator('.kb-timeline-actor');

test.describe('the knowledge timeline — naming the people who acted', () => {
  test('answered, a person is named from their own row, an agent is never looked up, and nothing says a read failed', async ({ page }) => {
    const asked = await install(page, (r) => json(r, { id: 'emp-rep-1', name: 'Rhea Okafor' }));
    await openKnowledge(page);

    await expect(actorOf(page, 'Called about the order')).toHaveText('Rhea Okafor');
    await expect(actorOf(page, 'Drafted a follow-up')).toHaveText('agent-claude');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    expect([...new Set(asked)]).toEqual(['/api/people/emp-rep-1']);
  });

  test('refused, the actor keeps the id and the view says the names did not load', async ({ page }) => {
    await install(page, (r) => json(r, { error: 'people down' }, 503));
    await openKnowledge(page);

    await expect(actorOf(page, 'Called about the order')).toHaveText('emp-rep-1');
    await expect(page.locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      "Couldn't load the names on the timeline — /api/people/emp-rep-1: HTTP 503. People show as ids.",
    );
  });
});
