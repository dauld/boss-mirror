// The guest button on /login.
//
// It replaces demo mode, which minted an `audit-readonly` session for
// anyone arriving without a valid cookie. That was invisible, and the
// invisibility is what broke it: when a real admin's session expired,
// the next request minted a guest session over it and reissued the
// cookie under the same name, so the SPA still looked signed in while
// every write returned 403.
//
// So the two properties worth pinning are that the guest session only
// ever appears because somebody asked for it, and that a deployment
// which does not offer it never shows the control.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';
import { installApiFloor } from './_smokeMocks';

const MANIFEST = { display_name: 'Algedonic Ales', modules: {}, labels: {} };

/// /login renders outside the AppShell — it takes the whole viewport,
/// so waiting for `.app-shell` would fail on a page that is working.
const LOGIN_ROOT = '.login-card';

async function stubAuth(
  page: import('@playwright/test').Page,
  opts: { guestEnabled: boolean },
): Promise<void> {
  await installApiFloor(page);
  await page.route(/\/api\/tenant\/manifest$/, (r) => r.fulfill({ json: MANIFEST }));
  // Not signed in — otherwise the page redirects to home instead of
  // rendering the form we are testing.
  await page.route('**/api/auth/me', (r) => r.fulfill({ status: 401, body: '' }));
  await page.route('**/api/auth/guest', async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    return route.fulfill({
      json: {
        enabled: opts.guestEnabled,
        email: 'guest@algedonic.dev',
        role: 'audit-readonly',
      },
    });
  });
}

const guestButton = (page: import('@playwright/test').Page) =>
  page.getByRole('button', { name: /browse as a guest/i });

test.describe('guest sign-in', () => {
  test('is offered when the deployment enables it', async ({ page }) => {
    await stubAuth(page, { guestEnabled: true });
    await mountPage(page, '/login', { root: LOGIN_ROOT });

    await expect(guestButton(page)).toBeVisible();
    // The identity is named on the button's own note rather than left
    // for the visitor to discover in a menu after the fact.
    await expect(page.getByText('guest@algedonic.dev')).toBeVisible();
    await expect(page.getByText(/read-only/i)).toBeVisible();
  });

  // A tenant running BOSS on their own company's data does not hand
  // out a session that reads every projection, and their people should
  // never be shown a button that answers 404.
  test('is absent when the deployment does not offer it', async ({ page }) => {
    await stubAuth(page, { guestEnabled: false });
    await mountPage(page, '/login', { root: LOGIN_ROOT });

    // The form is up, so the page rendered — the button is missing
    // because it was withheld, not because nothing loaded.
    await expect(page.getByRole('button', { name: /^sign in$/i })).toBeVisible();
    await expect(guestButton(page)).toHaveCount(0);
  });

  test('mints the session only on the click, and returns to ?next', async ({ page }) => {
    await stubAuth(page, { guestEnabled: true });

    let mintCalls = 0;
    await page.route('**/api/auth/guest', async (route) => {
      if (route.request().method() !== 'POST') return route.fallback();
      mintCalls += 1;
      return route.fulfill({
        json: {
          email: 'guest@algedonic.dev',
          employee_id: null,
          role: 'audit-readonly',
          access_tier: null,
        },
      });
    });

    await mountPage(page, '/login?next=%2Fux%2Fjobs', { root: LOGIN_ROOT });
    // Rendering the page must not have signed anyone in.
    expect(mintCalls).toBe(0);

    await guestButton(page).click();
    await page.waitForURL(/\/ux\/jobs/, { timeout: 10_000 });
    expect(mintCalls).toBe(1);
  });
});
