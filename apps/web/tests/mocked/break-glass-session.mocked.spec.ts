// The emergency door opens, and the app does not call it an error.
//
// What David saw on the first live assertion with an enrolled hardware
// key (packet 2ef7726b, 2026-09-08): the ceremony worked end to end —
// `/break-glass` opened, the session carried role `break-glass` with
// the right actor — and then the app answered "Signed in as
// break-glass-operator, but no matching employee in the roster."
//
// The session has no employee ON PURPOSE (Q4 of
// docs/design/break-glass-is-a-key-you-hold.md): resolving one would
// make the emergency path depend on boss-people being up. So the
// no-employee case is not a broken login to report, it is the design —
// and `classifyProbe` filed it under `unrecognized` all the same.
//
// Two properties, and they are the whole fix: the identity renders,
// and the error is gone. What the narrow role can DO — deploy
// rollback, merge approval, auth administration — is a second surface,
// deliberately not built here.

import { expect, test, type Page, type Route } from '@playwright/test';

const json = (r: Route, b: unknown): Promise<void> =>
  r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });

async function breakGlassSession(page: Page): Promise<void> {
  // Catch-all FIRST: Playwright matches in reverse registration order.
  await page.route('**/api/**', (r) => json(r, []));
  // Exactly what `break_glass::mint_session` signs into the cookie and
  // `GET /api/session` answers: the break-glass actor, the narrow
  // role, and no employee id.
  await page.route(/\/api\/people$/, (r) => json(r, []));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'break-glass-operator', role: 'break-glass' }));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(/\/api\/jobs\/summary(\?|$)/, (r) => json(r, { counts: {}, total: 0 }));
}

test('a break-glass session renders as an operator identity', async ({ page }) => {
  await breakGlassSession(page);
  await page.goto('/');

  await expect(page.getByText('Break-glass operator').first()).toBeVisible();
  // The shell's own identity block — the chrome agrees with the server
  // about who is signed in instead of rendering nobody.
  await expect(page.locator('.shell-user-role')).toHaveText('break-glass');
});

test('the door does not error the moment it succeeds', async ({ page }) => {
  await breakGlassSession(page);
  await page.goto('/');

  // The defect, in its own words. Svelte splits the paragraph at the
  // interpolated username, so match the fragment that carries the
  // claim rather than the whole sentence.
  await expect(page.getByText(/matching employee in the roster/)).toHaveCount(0);
  // And it is not handed an employee's board either — the guest's
  // defect (three empty panels under a tenure nobody has), which a
  // non-employee identity would otherwise inherit.
  await expect(page.getByText('Nothing in your personal queue')).toHaveCount(0);
  await expect(page.getByText("Couldn't load your watchlist")).toHaveCount(0);
});
