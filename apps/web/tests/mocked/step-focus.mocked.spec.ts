// The full-page step route, for steps that have no plugin.
//
// This page was written for plugin-backed steps and mounted the
// plugin unconditionally, so a kind without a bundle rendered "No
// plugin registered for <kind>" and stopped. That was fine while the
// only way here was a link on a plugin-backed step. It stopped being
// fine when inbox notifications started deep-linking every
// authority-gated ready step to this route: the message took you to a
// dead end, which is worse than not linking at all.
//
// The second half is subtler. Landing on a working generic surface is
// still useless if the step declares a required field, because
// validators run at `completed` — the API refuses the write and the
// operator has no way to supply the value. Inline field authoring
// exists so a Workflow can state its own completion contract without a
// bespoke surface; if the generic surface ignores that contract, the
// mechanism only works where someone already wrote a plugin.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';
import { installApiFloor } from './_smokeMocks';

const MANIFEST = { display_name: 'Algedonic Ales', modules: {}, labels: {} };

const JOB_ID = '99cfb52b-fca1-4e69-8798-f02575faf592';
const STEP_ID = 'ab8036d5-0326-46be-9585-90c4636d9116';
const DISPOSITIONS = 'reproduce|design|build|duplicate|needs-info|decline';

const STEP = {
  id: STEP_ID,
  job_id: JOB_ID,
  kind: 'task',
  title: 'Triage feedback',
  assignee_id: 'emp-bootstrap-admin',
  // `active` is the state the Complete control appears in.
  status: 'active',
  sort_order: 1,
  blocked_by: [],
  sign_offs_required: [],
  sign_offs: [],
  completed_on: null,
  notes: null,
  fields: [{ name: 'disposition', field_type: DISPOSITIONS, required: true }],
  metadata: { authority_role: 'platform-admin' },
};

const JOB = {
  id: JOB_ID,
  kind: 'user-feedback',
  title: 'Feedback on /inbox',
  status: 'open',
  subject: { subject_kind: 'custom', id: '/inbox' },
  owner_id: 'emp-bootstrap-admin',
  metadata: { message: 'Deep-link verification.', route: '/inbox' },
  steps: [STEP],
};

// The spec drives the surface's write controls, so it must BE someone
// who may write. Unmocked, /api/session falls through to the
// dev-server's default — the audit-readonly guest — and the readonly
// gate (correctly) renders every control disabled.
const EMP = {
  id: 'emp-bootstrap-admin', name: 'Bootstrap Admin', email: 'admin@boss',
  role: 'platform-admin', department: 'it', hire_date: '2023-01-01',
  status: 'active', location: 'hq', employment_type: 'full-time',
  skills: [], certifications: [],
};

test.describe('full-page step route without a plugin', () => {
  test.beforeEach(async ({ page }) => {
    await installApiFloor(page);
    await page.route(/\/api\/tenant\/manifest$/, (r) => r.fulfill({ json: MANIFEST }));
    await page.route(/\/api\/people$/, (r) => r.fulfill({ json: [EMP] }));
    await page.route(/\/api\/session$/, (r) =>
      r.fulfill({ json: { username: EMP.email, employee_id: EMP.id, role: EMP.role } }));
    await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => r.fulfill({ json: JOB }));
    await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/${STEP_ID}$`), async (route) => {
      if (route.request().method() === 'GET') return route.fulfill({ json: STEP });
      return route.fallback();
    });
  });

  test('renders a usable surface instead of "no plugin registered"', async ({ page }) => {
    await mountPage(page, `/jobs/${JOB_ID}/steps/${STEP_ID}`, { root: '.step-focus' });

    await expect(page.getByText(/no plugin registered/i)).toHaveCount(0);
    // The step's own contract, rendered from data.
    await expect(page.getByLabel(/disposition/i)).toBeVisible();
  });

  test('completing sends the declared field, so the API can accept it', async ({ page }) => {
    let body: Record<string, unknown> | null = null;
    await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/${STEP_ID}$`), async (route) => {
      if (route.request().method() === 'GET') return route.fulfill({ json: STEP });
      if (route.request().method() !== 'PUT') return route.fallback();
      body = route.request().postDataJSON() as Record<string, unknown>;
      return route.fulfill({ json: {} });
    });

    await mountPage(page, `/jobs/${JOB_ID}/steps/${STEP_ID}`, { root: '.step-focus' });

    const complete = page.getByRole('button', { name: /^complete$/i });
    // Nothing chosen yet: the contract is unsatisfied, and offering a
    // button that would 400 is how the original bug felt to a user.
    await expect(complete).toBeDisabled();

    await page.getByLabel(/disposition/i).selectOption('design');
    await expect(complete).toBeEnabled();
    await complete.click();

    await expect.poll(() => body !== null).toBe(true);
    const sent = body as unknown as { status: string; metadata: Record<string, unknown> };
    expect(sent.status).toBe('completed');
    expect(sent.metadata['disposition']).toBe('design');
    // The gate that keeps the step waiting on a person must survive.
    expect(sent.metadata['authority_role']).toBe('platform-admin');
  });
});
