// The dispatcher cascade and the rule editor under a failed read —
// backlog d7732e88 (2026-09-24).
//
// /it/registry/rules learned to mark its failure line (backlog
// cae1a377, car 3071e235): `.load-failed` + role=alert, the one marker
// outage-crawl asserts. The cascade at /it/registry/dispatcher reads the
// SAME /api/dispatcher/rules and painted its failure as a bare
// `.dx-msg.dx-err`, so it sat in outage-crawl's SILENT map for the same
// reason the rules page had. And no crawl rendered the rule editor at
// all — route-smoke dropped every parameterised catalog path — so its
// failed versions read painted the message as a header subtitle, in the
// same place and the same words as "this rule has no versions".
//
// Both are pinned here in the two shapes the read can fail in — a
// refused HTTP status and, for the cascade, the dispatcher's own 200
// carrying `error` (boss-dispatcher http.rs `rules`) — and the editor's
// empty answer is pinned apart from its failure, because an empty
// versions list is a rule that does not exist, not a read that failed.

import { expect, test, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const CASCADE = '/it/registry/dispatcher';
const CASCADE_TITLE = 'Dispatcher rules — reactive cascade';
/// The cascade's one read — anchored, so an editor's
/// `/api/dispatcher/rules/<name>/versions` is not it.
const RULES = /\/api\/dispatcher\/rules$/;

const RULE = 'auto-park-on-gate-green';
const EDITOR = `/it/registry/rules/${RULE}`;
const VERSIONS = new RegExp(`/api/dispatcher/rules/${RULE}/versions$`);

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

test.describe('/it/registry/dispatcher — a failed read is marked, and nothing is counted', () => {
  test('a refused read paints one marked alert and no rule counts', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(RULES, (r) => r.fulfill({ status: 503, contentType: 'text/plain', body: 'dispatcher down' }));
    await mountPage(page, CASCADE, { titleMatch: new RegExp(CASCADE_TITLE) });

    const failure = page.locator(`${FAILURE_MARKER}[role=alert]`);
    await expect(failure).toHaveCount(1);
    await expect(failure).toHaveText('Couldn’t load rules: HTTP 503 fetching /api/dispatcher/rules');
    await expect(page.getByText('No dispatcher rules are loaded.')).toHaveCount(0);
    // The stats line counts what was read; under a failed read it is
    // absent rather than "0 rules".
    await expect(page.locator('.dx-stats')).toHaveCount(0);
    // The way to the rules list stays reachable.
    await expect(page.getByRole('link', { name: 'Edit rules →' })).toHaveCount(1);
  });

  test('a 200 whose body carries an error is the same marked failure', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(RULES, (r) =>
      json(r, {
        rules: [],
        handler_emits: {},
        system_edges: [],
        error: 'load dispatcher_rules: relation "dispatcher_rules" does not exist',
      }),
    );
    await mountPage(page, CASCADE, { titleMatch: new RegExp(CASCADE_TITLE) });

    await expect(page.locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      'Couldn’t load rules: load dispatcher_rules: relation "dispatcher_rules" does not exist',
    );
    await expect(page.getByText('No dispatcher rules are loaded.')).toHaveCount(0);
    await expect(page.locator('.dx-stats')).toHaveCount(0);
  });

  test('an empty rule set is the empty state, unmarked', async ({ page }) => {
    // installSmokeMocks seeds two rules for the cascade; this one reads none.
    await installSmokeMocks(page);
    await page.route(RULES, (r) => json(r, { rules: [], handler_emits: {}, system_edges: [] }));
    await mountPage(page, CASCADE, { titleMatch: new RegExp(CASCADE_TITLE) });

    await expect(page.getByText('No dispatcher rules are loaded.')).toHaveCount(1);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });
});

test.describe('/it/registry/rules/:ruleName — a failed versions read is not a rule with no versions', () => {
  test('a refused versions read paints one marked alert under the rule name', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(VERSIONS, (r) => r.fulfill({ status: 503, contentType: 'text/plain', body: 'dispatcher down' }));
    await mountPage(page, EDITOR, { titleMatch: new RegExp(RULE) });

    const failure = page.locator(`.catalog ${FAILURE_MARKER}[role=alert]`);
    await expect(failure).toHaveCount(1);
    await expect(failure).toHaveText('Failed to load: HTTP 503: dispatcher down');
    // The header does not speak for the read it could not make.
    await expect(page.locator('.catalog header.exec-header p')).toHaveText(
      'Versions unknown — the registry read failed',
    );
    await expect(page.getByRole('link', { name: '← All dispatcher rules' })).toHaveCount(1);
  });

  test('an empty versions list says the rule has none, unmarked', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(VERSIONS, (r) => json(r, []));
    await mountPage(page, EDITOR, { titleMatch: new RegExp(RULE) });

    await expect(page.locator('.catalog header.exec-header p')).toHaveText(`No versions found for "${RULE}".`);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(page.locator('.catalog [role=alert]')).toHaveCount(0);
  });
});
