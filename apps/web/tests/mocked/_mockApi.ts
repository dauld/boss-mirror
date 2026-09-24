// Backend mock for the authoring smoke specs. These run against the
// dev-server (which serves the SPA shell) with EVERY `/api/**` call
// intercepted in-browser — so no backend is needed and the suite is
// deterministic and CI-gateable. The fidelity trade-off (fixtures, not
// the live API) is covered by the Phase-2 full-stack suite + the Rust
// integration tests; these specs guard SPA *behavior*.
//
// The design-Job endpoints are STATEFUL: completing a step advances the
// next one to `ready`, mirroring the server's readiness re-eval, so the
// workflow rail can be driven author → validate → approve → publish.

import type { Page, Route } from '@playwright/test';
import { installApiFloor } from './_smokeMocks';

export const JOB_ID = 'design-mock-1';
export const KIND_SLUG = 'seasonal-release';

const EMP = {
  id: 'emp-001', name: 'Demo CEO', email: 'ceo@demo', role: 'ceo',
  department: 'exec', hire_date: '2020-01-01', status: 'active',
  location: 'HQ', employment_type: 'full-time', skills: [], certifications: [],
};

// A viable two-step spec so the graph renders a trigger + a terminal
// (and one edge between them).
function seedSpec() {
  return {
    kind: KIND_SLUG, version: 1, status: 'draft', label: 'Seasonal Release',
    description: null, category: 'production', subject_kinds: ['asset'],
    steps: [
      // `terminal` is intentionally OMITTED here (not `null`): the real API
      // serializes non-terminal steps with skip_serializing_if, so the field
      // is absent → undefined on the wire. Keeping the fixture faithful makes
      // the authoring specs exercise the shape that crashed StepDagEditor.
      { title: 'start', kind: 'generic', ready_when: 'true', title_template: '', sign_offs_required: [], authority_role: null, metadata_defaults: {} },
      { title: 'finish', kind: 'generic', ready_when: 'steps.start.done', terminal: { outcome: 'completed' }, title_template: '', sign_offs_required: [], authority_role: null, metadata_defaults: {} },
    ],
    metadata_schema: {}, metadata: {}, entitlements: {},
    owning_team: 'authoring', authoring_job_id: null,
    created_at: '1970-01-01T00:00:00.000Z',
  };
}

const json = (route: Route, body: unknown, status = 200): Promise<void> =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

/// Install every route the New + authoring-workspace flows touch. Call
/// per-test (fresh stateful step graph each time). Routes are matched
/// last-registered-first, so the broad catch-all is registered first
/// and the specifics override it.
export async function installAuthoringMocks(page: Page): Promise<void> {
  // The bun dev-server injects a `<bun-hmr>` overlay element that
  // intercepts pointer events; keep removing it so clicks land on the UI.
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });

  const steps = [
    { id: 's-author', job_id: JOB_ID, kind: 'task', title: 'author', assignee_id: null, status: 'ready', sort_order: 0, blocked_by: [], completed_on: null, metadata: {} },
    { id: 's-validate', job_id: JOB_ID, kind: 'task', title: 'validate', assignee_id: null, status: 'pending', sort_order: 1, blocked_by: [], completed_on: null, metadata: {} },
    { id: 's-approve', job_id: JOB_ID, kind: 'sign-off', title: 'approve', assignee_id: null, status: 'pending', sort_order: 2, blocked_by: [], sign_offs_required: ['workflow-approver'], sign_offs: [], completed_on: null, metadata: { authority_role: 'workflow-approver' } },
    { id: 's-publish', job_id: JOB_ID, kind: 'workflow-publish', title: 'publish', assignee_id: null, status: 'pending', sort_order: 3, blocked_by: [], completed_on: null, metadata: { workflow_spec: seedSpec() } },
  ];
  const job = {
    id: JOB_ID, kind: 'workflow-design',
    subject: { subject_kind: 'custom', id: KIND_SLUG },
    title: `Design ${KIND_SLUG}`, owner_id: EMP.id, status: 'open',
    priority: 'standard', opened_on: '2026-06-21', due_on: null,
    closed_on: null, metadata: {}, tags: [],
  };
  const order = ['s-author', 's-validate', 's-approve', 's-publish'];

  // 1) The floor (lowest priority): empty-but-valid answers so nothing
  //    reaches the dev-server and the shell's incidental fetches resolve.
  await installApiFloor(page);

  // 2) Session/roster → deterministic demo persona (emp-001 is the
  //    SPA's default stored persona, so session.user resolves to it).
  await page.route('**/api/people', (r) => json(r, [EMP]));
  await page.route('**/api/session', (r) => json(r, {}));

  // 3) Authoring vocabulary.
  await page.route('**/api/subject-kinds', (r) => json(r, [{ kind: 'asset' }, { kind: 'account' }, { kind: 'employee' }]));
  await page.route('**/api/workflows', (r) => json(r, []));
  await page.route('**/api/jobs/step-types', (r) => json(r, [
    { kind: 'task', label: 'Task', category: 'generic', ux: 'inline', description: 'A unit of work' },
    { kind: 'sign-off', label: 'Sign-off', category: 'approval', ux: 'inline', description: 'Requires an approval' },
    { kind: 'checklist', label: 'Checklist', category: 'generic', ux: 'inline', description: 'A checklist' },
  ]));

  // 4) Any kind-slug GET → 404 (existence probe: nothing exists yet, so
  //    New proceeds). Registered before `_validate` so the POST wins.
  await page.route('**/api/workflows/*', (r) => json(r, 'not found', 404));
  await page.route('**/api/workflows/_validate', (r) => json(r, { ok: true, problems: [] }));

  // 5) Create the design Job (POST) — return its id.
  await page.route('**/api/jobs', (r) =>
    r.request().method() === 'POST' ? json(r, { id: JOB_ID }) : json(r, []));

  // 6) Load the design Job (flattened + steps), reflecting current state.
  await page.route(`**/api/jobs/${JOB_ID}`, (r) => json(r, { ...job, steps }));

  // 7) Step PUT — persist metadata and/or complete + advance the next.
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/[^/]+$`), (r) => {
    const m = r.request().url().match(/\/steps\/([^/?]+)/);
    const step = steps.find((s) => s.id === m?.[1]);
    if (step) {
      const body = JSON.parse(r.request().postData() ?? '{}') as { status?: string; metadata?: Record<string, unknown> };
      if (body.metadata) step.metadata = body.metadata;
      if (body.status === 'completed') {
        step.status = 'completed';
        const next = steps.find((s) => s.id === order[order.indexOf(step.id) + 1]);
        if (next) next.status = 'ready';
      }
    }
    return json(r, step ?? {});
  });

  // 7b) Step merge door — top-level keys merged in, a null deletes
  //     (backlog e39a9d2a: step surfaces write metadata here, then PUT
  //     the status alone).
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/[^/]+/metadata$`), (r) => {
    const m = r.request().url().match(/\/steps\/([^/?]+)\/metadata/);
    const step = steps.find((s) => s.id === m?.[1]);
    if (step) {
      const patch = JSON.parse(r.request().postData() ?? '{}') as Record<string, unknown>;
      step.metadata = Object.fromEntries(
        Object.entries({ ...step.metadata, ...patch }).filter(([, v]) => v !== null),
      );
    }
    return json(r, step ?? {});
  });

  // 8) Sign-off stamp.
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/[^/]+/sign-offs$`), (r) => json(r, { ok: true }));
}
