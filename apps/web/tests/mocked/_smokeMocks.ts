// Generic adversarial backend mock for the route-crawl smoke spec
// (route-smoke.mocked.spec.ts). Every `/api/**` call is intercepted in
// the browser, so no backend is needed and the crawl is deterministic +
// CI-gateable.
//
// The fixtures are deliberately ADVERSARIAL: each data-bearing endpoint
// returns exactly one item with every OPTIONAL field OMITTED — mirroring
// serde `skip_serializing_if`, which sends `undefined`, not `null`. That
// is the shape that slipped past the happy-path fixtures and crashed
// StepDagEditor (omitted `terminal`) and would have caught the carrier
// type-lie. Unknown endpoints fall through to `[]` so every page still
// mounts.

import type { Page, Route } from '@playwright/test';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

// Persona: one employee, role ceo ⇒ every route is visible.
const EMP = {
  id: 'emp-001', name: 'Demo CEO', email: 'ceo@demo', role: 'ceo',
  department: 'exec', hire_date: '2020-01-01', status: 'active',
  location: 'HQ', employment_type: 'full-time', skills: [], certifications: [],
};

// A Workflow whose first step OMITS `terminal` (the adversarial serde
// shape) and whose second carries one. Renders on the hub, atlas,
// workflows list, and the detail page.
const WORKFLOW = {
  kind: 'seasonal-release', version: 1, status: 'active', label: 'Seasonal Release',
  description: null, category: 'production', subject_kinds: ['asset'],
  steps: [
    { title: 'start', kind: 'generic', ready_when: 'true', title_template: '', sign_offs_required: [], authority_role: null, metadata_defaults: {} },
    { title: 'finish', kind: 'generic', ready_when: 'steps.start.done', terminal: { outcome: 'completed' }, title_template: '', sign_offs_required: [], authority_role: null, metadata_defaults: {} },
  ],
  metadata_schema: {}, metadata: {}, entitlements: {},
  owning_team: 'platform', authoring_job_id: null, created_at: '2026-01-01T00:00:00.000Z',
};

// A marketing asset with every OPTIONAL field omitted (kind, description,
// file_url, owner_id, *_at-by, supersedes_id). Required arrays present.
const MARKETING_ASSET = {
  id: 'ma-1', title: 'Brand deck', tags: [], linked_device_skus: [],
  linked_account_ids: [], linked_campaign_ids: [],
  created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z',
};

// A shipment with `carrier` OMITTED — identity-first, no label yet.
const SHIPMENT = {
  id: 'sh-1', direction: 'outbound', status: 'in-transit', tracking_number: null,
  origin: 'HQ', destination: 'Depot', asset_ids: [], line_items: [],
  po_id: null, order_id: null, account_id: null,
  created_on: '2026-01-01', shipped_on: null, estimated_delivery: null, delivered_on: null,
};

export async function installSmokeMocks(page: Page): Promise<void> {
  // Strip the bun dev-server HMR overlay. Identity comes from the
  // mocked `/api/session` below, not from anything stored client-side.
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });

  // Catch-all FIRST (lowest priority): unknown endpoints → empty list, 200,
  // so the shell's incidental fetches resolve and the page mounts. Routes
  // registered later (below) take precedence.
  await page.route('**/api/**', (r) => json(r, []));

  // Identity / session.
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await page.route(/\/api\/session$/, (r) => json(r, {}));
  await page.route(/\/api\/auth\/me$/, (r) => json(r, {}));

  // Live job state (objects, not lists — the catch-all `[]` would break these).
  await page.route(/\/api\/jobs\/live$/, (r) => json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(/\/api\/jobs\/summary(\?|$)/, (r) => json(r, { counts: {}, total: 0 }));
  // The yard status read-model (object, not a list). An empty-but-well-
  // formed payload so the page renders its "no trains / no cars" states.
  await page.route(/\/api\/yard\/status$/, (r) =>
    json(r, {
      trains: [], dock: [], recent: [], stranded: [],
      boarding: { dock_threshold: null, cooldown_minutes: null, at_times: [], dock_depth: 0, threshold_met: null, summary: 'No boarding cadence is configured.' },
      gates: { capacity: 3, active: [] },
      garage: [],
      policy: { stall_hours: null, max_red_trains: null },
      now: '2026-09-03T12:00:00Z',
    }),
  );

  // Workflow registry + the adversarial kind (omitted-terminal step).
  await page.route(/\/api\/workflows$/, (r) => json(r, [WORKFLOW]));
  await page.route(/\/api\/workflows\/[^/]+$/, (r) => json(r, WORKFLOW));
  await page.route(/\/api\/workflows\/[^/]+\/versions$/, (r) => json(r, [WORKFLOW]));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'generic', label: 'Generic', category: 'generic', ux: 'inline', description: '' },
    { kind: 'task', label: 'Task', category: 'generic', ux: 'inline', description: '' },
  ]));
  await page.route(/\/api\/jobs\/step-plugins$/, (r) => json(r, [
    { kind: 'demo', label: 'Demo', category: 'generic', version: 1, frontend_url: '/plugins/demo.js', owning_team: 'platform' },
  ]));

  // Dispatcher cascade.
  await page.route(/\/api\/dispatcher\/rules$/, (r) => json(r, {
    rules: [{ name: 'r1', on_event: 'step.done.task', when: null, do: [{ handler: 'h1', args: {} }], version: 1 }],
    handler_emits: { h1: ['x.y'] }, system_edges: [],
  }));

  // Taxonomy registries (Subjects & Classes + System Model hub).
  await page.route(/\/api\/subject-kinds$/, (r) => json(r, [
    { kind: 'person', label: 'Person', parent_kind: null, description: null, owning_team: 'platform', metadata: {}, sort_order: 1, retired_at: null },
    { kind: 'employee', label: 'Employee', parent_kind: 'person', description: null, owning_team: 'platform', metadata: {}, sort_order: 1, retired_at: null },
  ]));
  // Class row with `retired_at` OMITTED (adversarial).
  await page.route(/\/api\/classes(\?|$)/, (r) => json(r, [
    { subject_kind: 'employee', code: 'ceo', display_name: 'CEO', parent_code: null, member_attribute: 'role', metadata: {}, sort_order: 1 },
  ]));

  // Gateway perf histogram (PerfPage iterates `.endpoints`).
  await page.route(/\/api\/gateway\/perf$/, (r) => json(r, { endpoints: [], window_started_at: '2026-01-01T00:00:00Z' }));

  // Audit tail (the event pulse).
  await page.route(/\/api\/events\/tail(\?|$)/, (r) => json(r, [
    { event_id: 'e1', timestamp: '2026-01-01T00:00:00Z', source: 'jobs', kind: 'jobs.step.updated', payload: {} },
  ]));

  // Marketing assets (optionals omitted).
  await page.route(/\/api\/catalog\/marketing-assets(\?|$)/, (r) => json(r, [MARKETING_ASSET]));
  await page.route(/\/api\/catalog\/marketing-assets\/[^/]+\/history$/, (r) => json(r, []));
  await page.route(/\/api\/catalog\/marketing-assets\/[^/]+$/, (r) => json(r, MARKETING_ASSET));

  // Shipments (carrier omitted).
  // Views — the Home composer surface. Two rows so the crawler renders
  // both visibility badges, and a results payload with `truncated` set
  // so the ceiling warning is exercised rather than only the happy path.
  // The tenant manifest now carries the tenant's own name, which the
  // chrome bar renders. Mocked so the brand assertions below have
  // something deterministic to read.
  await page.route(/\/api\/tenant\/manifest$/, (r) =>
    json(r, {
      display_name: 'Algedonic Ales',
      tenant_id: 'brewery',
      modules: {},
      labels: {},
    }),
  );
  await page.route(/\/api\/views(\?|$)/, (r) =>
    json(r, [
      {
        id: 'view-1', owner_id: 'emp-1', title: 'Open jobs', source: 'jobs',
        filter: 'status = "open"', columns: ['id', 'status'], layout: 'table',
        visibility: 'private', created_at: '2026-08-01T00:00:00Z',
        updated_at: '2026-08-01T00:00:00Z',
      },
      {
        id: 'view-2', owner_id: 'emp-other', title: 'Recent events', source: 'events',
        filter: '', columns: [], layout: 'count', visibility: 'shared',
        created_at: '2026-08-01T00:00:00Z', updated_at: '2026-08-01T00:00:00Z',
      },
    ]),
  );
  await page.route(/\/api\/views\/[^/]+\/results/, (r) =>
    json(r, {
      view_id: 'view-1', source: 'jobs', layout: 'table',
      rows: [{ id: 'j-1', status: 'open' }],
      matched: 1, truncated: false,
    }),
  );
  await page.route(/\/api\/shipping\/shipments(\?|$)/, (r) => json(r, [SHIPMENT]));
  await page.route(/\/api\/shipping\/shipments\/[^/]+$/, (r) => json(r, SHIPMENT));
}
