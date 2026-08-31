// Feedback is a Job, and these pin the two things that makes true:
// the POST carries the identity-first Subject the Workflow declares,
// and the control is present on every surface because it lives in the
// chrome rather than on a page.
//
// The Subject matters more than it looks. The route path IS the
// subject id, so "what have people said about this page" is a
// Subject-history question — the same machinery that answers it for
// an account. Post a Job without that shape and feedback becomes an
// unaddressable pile.

import { test, expect } from '@playwright/test';
import { mountPage } from '../smoke/_helpers';

const MANIFEST = { display_name: 'Algedonic Ales', modules: {}, labels: {} };

/// Scoped to the chrome bar deliberately. An unscoped
/// `getByRole('button', { name: /feedback/i })` matched TWO elements on
/// /system — the System Model hub uses the word in its own copy
/// ("algedonic feedback" is the vocabulary this system is named for) —
/// and an ambiguous locator failed only in the full suite, where the
/// other element had rendered. The control under test is the chrome's,
/// so say so.
const chromeFeedback = (page: import('@playwright/test').Page) =>
  page.locator('.perspective-tabs').getByRole('button', { name: /^feedback$/i });

test.describe('feedback control', () => {
  test.beforeEach(async ({ page }) => {
    await page.route(/\/api\/tenant\/manifest$/, (r) => r.fulfill({ json: MANIFEST }));
  });

  // One test per surface rather than a loop inside one test: a loop
  // reports the first failure without saying which navigation was
  // slow, and re-navigating three times in a single test made this
  // the flakiest thing in the suite.
  for (const path of ['/ux/jobs', '/it', '/ux/views']) {
    test(`is in the chrome on ${path}`, async ({ page }) => {
      await mountPage(page, path);
      await expect(chromeFeedback(page)).toBeVisible();
    });
  }

  test('opens a user-feedback Job carrying the route as its Subject', async ({ page }) => {
    let posted: Record<string, unknown> | null = null;
    await page.route('**/api/jobs', async (route) => {
      if (route.request().method() !== 'POST') return route.fallback();
      posted = route.request().postDataJSON() as Record<string, unknown>;
      return route.fulfill({ json: { id: 'job-fb-1', kind: 'user-feedback' } });
    });

    await mountPage(page, '/ux/views');
    await chromeFeedback(page).click();
    await page.getByRole('radio', { name: /^feature$/i }).click();
    await page.getByPlaceholder(/idea, a gap/i).fill('Column picker forgets my choice');
    await page.getByRole('button', { name: /^send$/i }).click();

    await expect(page.getByText(/opened a feedback Job/i)).toBeVisible();

    expect(posted).not.toBeNull();
    const body = posted as unknown as {
      kind: string;
      subject: { subject_kind: string; id: string };
      metadata: { message: string; route: string };
      tags: string[];
    };
    expect(body.kind).toBe('user-feedback');
    // Identity-first shape. The pre-2026-06-13 {custom_kind, ref_id}
    // form 422s with "missing field `id`" — the failure that left the
    // design-review button dead for weeks.
    expect(body.subject).toEqual({ subject_kind: 'custom', id: '/ux/views' });
    expect(body.metadata.route).toBe('/ux/views');
    expect(body.metadata.message).toContain('Column picker');
    expect(body.tags).toContain('feedback');
  });

  test('will not send an empty message', async ({ page }) => {
    await mountPage(page, '/ux/jobs');
    await chromeFeedback(page).click();
    await expect(page.getByRole('button', { name: /^send$/i })).toBeDisabled();
  });

  test('surfaces a failure instead of claiming success', async ({ page }) => {
    await page.route('**/api/jobs', async (route) => {
      if (route.request().method() !== 'POST') return route.fallback();
      return route.fulfill({ status: 422, body: 'invalid job body' });
    });
    await mountPage(page, '/ux/jobs');
    await chromeFeedback(page).click();
    await page.getByRole('radio', { name: /^feature$/i }).click();
    await page.getByPlaceholder(/idea, a gap/i).fill('something');
    await page.getByRole('button', { name: /^send$/i }).click();
    await expect(page.getByText(/invalid job body/i)).toBeVisible();
    await expect(page.getByText(/opened a feedback Job/i)).toHaveCount(0);
  });

  test('a bug asks for both halves and files them as one message', async ({ page }) => {
    // A bug is a claim that the software is wrong, and the claim needs
    // what happened AND what was expected. Filed as one blob the pair
    // was usually half-missing, because the reporter writes down only
    // the surprising half.
    let posted: Record<string, unknown> | null = null;
    await page.route('**/api/jobs', async (route) => {
      if (route.request().method() !== 'POST') return route.fallback();
      posted = route.request().postDataJSON() as Record<string, unknown>;
      return route.fulfill({ json: { id: 'job-fb-2', kind: 'user-feedback' } });
    });

    await mountPage(page, '/ux/views');
    await chromeFeedback(page).click();

    // Bug is the default, and Send stays disabled until BOTH halves are
    // given — one half is the state this change exists to prevent.
    const send = page.getByRole('button', { name: /^send$/i });
    await page.getByPlaceholder(/actually did/i).fill('Saved and showed the old value');
    await expect(send).toBeDisabled();
    await page.getByPlaceholder(/should have done/i).fill('Showed the value I typed');
    await expect(send).toBeEnabled();
    await send.click();

    await expect(page.getByText(/opened a feedback Job/i)).toBeVisible();
    const body = posted as unknown as {
      metadata: { message: string; feedback_kind: string; reality: string; expectation: string };
      tags: string[];
    };
    // One `message` field, so every existing reader — the triage board,
    // the detail modal, the queue script — keeps working unchanged.
    expect(body.metadata.message).toContain('Saved and showed the old value');
    expect(body.metadata.message).toContain('Showed the value I typed');
    // The halves survive separately too, for a surface that wants them.
    expect(body.metadata.feedback_kind).toBe('bug');
    expect(body.metadata.reality).toBe('Saved and showed the old value');
    expect(body.metadata.expectation).toBe('Showed the value I typed');
    expect(body.tags).toContain('bug');
  });
});