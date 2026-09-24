// Session and gating agree about who you are.
//
// Three properties, one per defect:
//
//  (a) A guest (readonly) session renders READONLY surfaces — disabled
//      controls behind the shared WriteGate, with a quiet "sign in to
//      act" affordance — not live composers and pickers that 403 on
//      click. `session.readonly` had exactly one consumer (MePage)
//      while ~43 files issued writes.
//  (b) Becoming an operator ends the guest: the same surface renders
//      live controls once the session resolves to a rostered employee.
//      (The same-tab `setPersona` half of this transition is pinned as
//      a value in src/me/personaReadonly.test.ts.)
//  (c) The `unauthenticated` state is REACHABLE and its copy is TRUE.
//      The 401 interceptor used to redirect on the session probe
//      itself, so the state machine could never land there — and
//      MePage's "Reload the page to log in" advice was false (a reload
//      just re-ran the loop). A probe 401 is an answer, not an expiry;
//      data 401s still bounce to /login.

import { expect, test, type Page, type Route } from '@playwright/test';

const JOB_ID = 'job-ro-1';

const STEP = {
  id: 's1', job_id: JOB_ID, kind: 'task', title: 'triage', status: 'active',
  assignee_id: null, sort_order: 0, blocked_by: [], sign_offs_required: [],
  sign_offs: [], completed_on: null, metadata: {}, notes: null, spec_slug: 'triage',
};

const JOB = {
  id: JOB_ID, kind: 'user-feedback', title: 'Feedback on /shop',
  status: 'open', opened_on: '2026-08-15', due_on: null, closed_on: null,
  owner_id: 'emp-001', priority: 'standard', simulated: false, tags: [],
  subject: { subject_kind: 'custom', id: '/shop' }, metadata: {}, steps: [STEP],
};

const OPERATOR = {
  id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [],
};

const json = (r: Route, b: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

/// Everything except identity: the shell's incidental fetches, the
/// packet under test, the step-type registry the surface dispatcher
/// reads. Register FIRST — Playwright matches last-registered first.
async function baseMocks(page: Page): Promise<void> {
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, JOB));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'task', label: 'Task', category: 'generic', ux: 'inline', description: '' },
  ]));
}

async function guestIdentity(page: Page): Promise<void> {
  // No employee matches and the role is audit-readonly — exactly how
  // classifyProbe recognises the guest persona.
  await page.route(/\/api\/people$/, (r) => json(r, []));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'guest@algedonic.dev', role: 'audit-readonly' }));
}

async function operatorIdentity(page: Page): Promise<void> {
  await page.route(/\/api\/people$/, (r) => json(r, [OPERATOR]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: OPERATOR.id, role: OPERATOR.role }));
}

const NOTE = /read-only session/i;

test.describe('a guest sees readonly renders, not live controls that 403', () => {
  test('the step surface: controls disabled, the way to act named', async ({ page }) => {
    await baseMocks(page);
    await guestIdentity(page);
    await page.goto(`/ux/jobs/${JOB_ID}`);

    const surface = page.locator('.step-generic');
    await expect(surface).toBeVisible();

    // The picker and the transition button are rendered — a guest may
    // READ everything — but inert, not live controls that 403.
    await expect(surface.getByRole('button', { name: 'Complete' })).toBeDisabled();
    await expect(surface.locator(`#assignee-${STEP.id}`)).toBeDisabled();

    // The quiet affordance says why, and names the way back in.
    await expect(page.getByText(NOTE).first()).toBeVisible();
    const signIn = page.getByRole('link', { name: /sign in/i }).first();
    await expect(signIn).toHaveAttribute('href', /^\/login/);
  });

  test('the composer: Compose is disabled, not a modal that 403s on send', async ({ page }) => {
    await baseMocks(page);
    await guestIdentity(page);
    await page.goto('/ux/inbox');

    const compose = page.getByRole('button', { name: 'Compose' });
    await expect(compose).toBeVisible();
    await expect(compose).toBeDisabled();
    await expect(page.getByText(NOTE).first()).toBeVisible();
  });
});

test('becoming an operator ends the guest: the same surface goes live', async ({ page }) => {
  await baseMocks(page);
  await guestIdentity(page);
  await page.goto(`/ux/jobs/${JOB_ID}`);

  const complete = page.locator('.step-generic').getByRole('button', { name: 'Complete' });
  await expect(complete).toBeDisabled();

  // Sign-in is a navigation: the gateway sets the cookie and the SPA
  // reloads. The same probe now answers with a rostered operator.
  await page.unroute(/\/api\/session$/);
  await page.unroute(/\/api\/people$/);
  await operatorIdentity(page);
  await page.reload();

  await expect(complete).toBeEnabled();
  await expect(page.getByText(NOTE)).toHaveCount(0);
});

test.describe('the unauthenticated state is reachable and its copy is true', () => {
  test('a session-probe 401 lands on honest copy, not a redirect loop', async ({ page }) => {
    await baseMocks(page);
    await page.route(/\/api\/people$/, (r) => json(r, []));
    await page.route(/\/api\/session$/, (r) => json(r, 'unauthenticated', 401));

    await page.goto('/me');

    // The state renders — nobody was bounced for asking who they are.
    await expect(page.getByText('Not signed in')).toBeVisible();
    expect(new URL(page.url()).pathname).toBe('/me');

    // True copy: a link to the door, not "reload the page" (a reload
    // only re-ran the probe and landed here again).
    await expect(page.getByText(/reload the page/i)).toHaveCount(0);
    await expect(page.getByRole('link', { name: /sign in/i })).toHaveAttribute(
      'href', /^\/login/,
    );
  });

  test('a DATA 401 still bounces to /login with the path as next', async ({ page }) => {
    await baseMocks(page);
    await operatorIdentity(page);
    // The packet read itself is denied — an expired or insufficient
    // session mid-browse. This is the interceptor's real job.
    await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) =>
      json(r, 'unauthenticated', 401));
    // /login's own probes must not loop the redirect.
    await page.route(/\/api\/auth\/me$/, (r) => json(r, 'unauthenticated', 401));

    await page.goto(`/ux/jobs/${JOB_ID}`);

    await page.waitForURL(/\/login\?next=/);
    const next = new URL(page.url()).searchParams.get('next');
    expect(next).toBe(`/ux/jobs/${JOB_ID}`);
  });
});
