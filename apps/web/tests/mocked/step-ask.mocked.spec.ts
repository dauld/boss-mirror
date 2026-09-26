// A decision step shows its one ask (user-feedback 26ae4d44).
//
// David, 2026-09-15, on a backlog-item triage step assigned to him
// with a brief that said exactly what to enter: "Wasn't quite sure how
// to fill in the job properly to move to the next step." Measured
// from the code that day, the generic surface put the brief ABOVE the
// card, then the title, the assignee, the due date, and only then the
// form — six bare words in a select, a one-line box for evidence, the
// whole markdown brief crammed into a second one-line box — and the
// only button on a `ready` step was Start. Complete appeared after,
// said nothing about where the answer goes, and when it could not be
// pressed the reason was a hover title.
//
// Rendered proof of the shape that replaces it: title, brief, form,
// button — in that order; every option naming the step it opens; the
// button naming the route; the missing field named in the row.

import { expect, test, type Page, type Route } from '@playwright/test';
import { servePeopleRows } from './_smokeMocks';

const JOB_ID = '6f58e9a1-0000-4000-8000-000000000001';
const STEP_ID = '19b29853-0000-4000-8000-000000000002';

const EMP = { id: 'emp-david', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

const DISPOSITIONS = 'verify|design|build|duplicate|stale|decline';

// The backlog-item graph as the registry serves it (v5), trimmed to
// what routes on triage.
const SPEC = {
  kind: 'backlog-item', version: 5,
  steps: [
    { title: 'filed', kind: 'trigger', ready_when: 'true', title_template: 'Filed to the backlog', fields: [] },
    { title: 'triage', kind: 'task', ready_when: 'steps.filed.done',
      title_template: 'Measure the claim, choose a route',
      fields: [
        { name: 'disposition', field_type: DISPOSITIONS, required: true },
        { name: 'evidence', field_type: 'string', required: true },
        { name: 'context_md', field_type: 'string', required: false },
        { name: 'proposed', field_type: 'string', required: false },
      ] },
    { title: 'measure', kind: 'task', title_template: 'Re-measure the claim',
      ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "verify"', fields: [] },
    { title: 'design-review', kind: 'answer-question', title_template: 'Decide the design',
      ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "design"', fields: [] },
    { title: 'build', kind: 'task', title_template: 'Build the change',
      ready_when: '(steps.triage.done AND steps.triage.metadata.disposition = "build") OR (steps.design-review.done AND steps.design-review.metadata.verdict = "approved")', fields: [] },
    { title: 'duplicate', kind: 'outcome', title_template: 'Closed as a duplicate',
      ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "duplicate"', fields: [] },
    { title: 'stale', kind: 'outcome', title_template: 'Closed — the claim no longer holds',
      ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "stale"', fields: [] },
    { title: 'declined', kind: 'outcome', title_template: 'Closed without action',
      ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "decline"', fields: [] },
  ],
};

const BRIEF = 'Measured 2026-09-15: the claim holds. Enter disposition build, evidence option b.';

const STEP = {
  id: STEP_ID, job_id: JOB_ID, kind: 'task', spec_slug: 'triage',
  title: 'Measure the claim, choose a route',
  assignee_id: EMP.id,
  // `ready` is the state an assigned triage step sits in — the state
  // David opened it in. The ask must be answerable from here.
  status: 'ready', sort_order: 1, blocked_by: [], sign_offs_required: [], sign_offs: [],
  completed_on: null, notes: null,
  fields: SPEC.steps[1]!.fields,
  metadata: { authority_role: 'platform-admin', context_md: BRIEF },
};

const JOB = {
  id: JOB_ID, kind: 'backlog-item', workflow_version: 5,
  title: 'A step that cannot be acted on unaided', status: 'open',
  opened_on: '2026-09-15', due_on: null, closed_on: null, owner_id: EMP.id,
  priority: 'standard', simulated: false, tags: [],
  subject: { subject_kind: 'custom', id: 'apps/web' }, metadata: { body: 'the filed case' },
  steps: [STEP],
};

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

async function mocks(
  page: Page,
): Promise<{ puts: Record<string, unknown>[]; merges: Record<string, unknown>[] }> {
  const puts: Record<string, unknown>[] = [];
  const merges: Record<string, unknown>[] = [];
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });
  // Catch-all FIRST: Playwright matches routes in reverse registration
  // order, so later, more specific routes win over this one.
  await page.route('**/api/**', (r) => json(r, { data: [], total: 0 }));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await servePeopleRows(page, [EMP]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: EMP.role }));
  await page.route(/\/api\/jobs\/step-types$/, (r) =>
    json(r, [{ kind: 'task', label: 'Task', category: 'generic', ux: 'inline', description: '' }]));
  await page.route(/\/api\/jobs\/step-plugins.*/, (r) => json(r, []));
  await page.route(/\/api\/workflows\/backlog-item\/versions\/5$/, (r) => json(r, SPEC));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, JOB));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/${STEP_ID}$`), (r) => {
    if (r.request().method() === 'PUT') {
      puts.push(JSON.parse(r.request().postData() ?? '{}') as Record<string, unknown>);
      return json(r, { ...STEP, status: 'completed' });
    }
    return json(r, STEP);
  });
  // The step merge door — the answered fields land here, before the
  // status-only PUT (backlog e39a9d2a).
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/${STEP_ID}/metadata$`), (r) => {
    merges.push(JSON.parse(r.request().postData() ?? '{}') as Record<string, unknown>);
    return json(r, STEP);
  });
  return { puts, merges };
}

test('title, brief, form, button — in that order, with nothing between', async ({ page }) => {
  await mocks(page);
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const card = page.locator('.step-generic');
  await expect(card.locator('.step-ask')).toBeVisible();
  await expect(card.locator('.step-decision-context .sdc-body')).toContainText('option b');

  // Reading order inside the card: h3 → the case → the ask → the
  // assignee row. The DOM order is the screen order.
  const order = await card.evaluate((el) => {
    const marks = ['h3', '.step-decision-context', '.step-ask', '.step-assign-row'];
    const all = [...el.querySelectorAll('*')];
    return marks.map((m) => all.findIndex((n) => n.matches(m)));
  });
  expect(order.every((i) => i >= 0)).toBe(true);
  expect([...order].sort((a, b) => a - b)).toEqual(order);
});

test('every option names the step it opens, read off the Workflow graph', async ({ page }) => {
  await mocks(page);
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const select = page.locator('.step-ask').getByLabel(/disposition/i);
  await expect(select.locator('option[value="build"]')).toHaveText('build — Build the change');
  await expect(select.locator('option[value="verify"]')).toHaveText('verify — Re-measure the claim');
  await expect(select.locator('option[value="stale"]')).toHaveText(
    'stale — Closed — the claim no longer holds',
  );
  // Free text is a textarea, and the brief the router wrote is not
  // crammed into a one-line box.
  await expect(page.locator('.step-ask textarea')).toHaveCount(3);
});

test('the button names the route and the missing field, and completes from ready', async ({ page }) => {
  const { puts, merges } = await mocks(page);
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const ask = page.locator('.step-ask');
  const complete = ask.getByRole('button', { name: /^Complete/ });

  // Nothing answered: the button is inert and the row says which
  // fields it waits on — not a hover title.
  await expect(complete).toBeDisabled();
  await expect(complete).toHaveText('Complete');
  await expect(ask.locator('.step-ask-needs')).toHaveText('Needs: disposition, evidence');

  await ask.getByLabel(/disposition/i).selectOption('build');
  await expect(complete).toHaveText('Complete — routes to: Build the change');
  await expect(ask.locator('.step-ask-needs')).toHaveText('Needs: evidence');
  await expect(complete).toBeDisabled();

  await ask.getByLabel(/evidence/i).fill('option b');
  await expect(complete).toBeEnabled();
  await complete.click();

  await expect.poll(() => puts.length).toBe(1);
  const sent = puts[0] as { status: string; metadata?: unknown };
  expect(sent.status).toBe('completed');
  expect(sent.metadata).toBeUndefined();
  expect(merges.length).toBe(1);
  const merged = merges[0];
  expect(merged['disposition']).toBe('build');
  expect(merged['evidence']).toBe('option b');
  // A key the surface does not own is not re-sent: the merge leaves it
  // on the row as stored, where a wholesale PUT had to carry it back.
  // (`context_md` IS a declared field, so the form owns it and sends
  // its value, unchanged.)
  expect('authority_role' in merged).toBe(false);
  expect(merged['context_md']).toBe(BRIEF);
});
