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

/// Every module id the SPA gates on (nav-catalog `module`, App.svelte's
/// routeRequiredModule, the Simulator tab and DebugGear), all on — the
/// playground tenant's manifest, in the shape the tenant contract
/// documents.
export const MODULES_ON: Readonly<Record<string, boolean>> = {
  calendar: true, equipment: true, exec: true, finance: true, 'marketing-assets': true,
  parts: true, qa: true, shipping: true, support: true, warehouse: true, shop: true, sim: true,
};

/// The platform's department Classes (01-registries.sql) as `/api/classes`
/// rows: the chrome bar derives its tabs from `(employee, *, department)`
/// since ce68f137, so a mock with no departments is a bar with no
/// department tabs. Shared with the chrome specs that mock no other
/// backend.
export const DEPARTMENT_CLASSES: ReadonlyArray<Record<string, unknown>> = [
  ['it', 'IT'], ['executive', 'Executive'], ['sales', 'Sales'], ['service', 'Service'],
  ['refurb', 'Refurb'], ['qa', 'QA'], ['warehouse', 'Warehouse'], ['finance', 'Finance'],
  ['people', 'People'], ['support', 'Support'], ['marketing', 'Marketing'],
].map(([code, display_name], i) => ({
  subject_kind: 'employee', code, display_name, parent_code: null, member_attribute: 'department',
  metadata: {}, sort_order: (i + 1) * 10, retired_at: null,
}));

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

/// The endpoints the app SHELL needs to paint at all: identity, the
/// tenant manifest and the taxonomy registries the nav resolves
/// visibility from. A crawl that breaks or empties every read still
/// keeps these up, because a crawl where nothing renders measures
/// nothing. Read by outage-crawl (every other read 500s) and
/// interaction-crawl's empty leg (every other read is `[]`); it lives
/// here, beside the fixtures it names, so the two legs cannot disagree
/// about what "the shell" is.
export const SHELL_ENDPOINTS: ReadonlyArray<RegExp> = [
  /\/api\/session$/,
  /\/api\/auth\/me$/,
  /\/api\/people$/,
  /\/api\/tenant\/manifest$/,
  /\/api\/classes(\?|$)/,
  /\/api\/subject-kinds$/,
];

/// The endpoints whose fixture is an OBJECT, not a list. Named once,
/// used below to route them and exported for interaction-crawl's empty
/// leg, which answers `[]` to every collection read and must let these
/// fall through to their (already empty, or seeded-detail) fixture: a
/// list where an object is due is a malformed read, not an empty one.
export const JOBS_LIVE = /\/api\/jobs\/live$/;
export const JOBS_SUMMARY = /\/api\/jobs\/summary(\?|$)/;
export const YARD_STATUS = /\/api\/yard\/status$/;
export const YARD_REGIONS = /\/api\/yard\/regions(\?|$)/;
export const YARD_BORDERS = /\/api\/yard\/borders(\?|$)/;
export const WORKFLOW_DETAIL = /\/api\/workflows\/[^/]+$/;
export const DISPATCHER_RULES = /\/api\/dispatcher\/rules$/;
export const GATEWAY_PERF = /\/api\/gateway\/perf$/;
export const MARKETING_ASSET_DETAIL = /\/api\/catalog\/marketing-assets\/[^/]+$/;
export const VIEW_RESULTS = /\/api\/views\/[^/]+\/results/;
export const SHIPMENT_DETAIL = /\/api\/shipping\/shipments\/[^/]+$/;
export const OBJECT_ENDPOINTS: ReadonlyArray<RegExp> = [
  JOBS_LIVE, JOBS_SUMMARY, YARD_STATUS, YARD_REGIONS, YARD_BORDERS, WORKFLOW_DETAIL, DISPATCHER_RULES, GATEWAY_PERF,
  MARKETING_ASSET_DETAIL, VIEW_RESULTS, SHIPMENT_DETAIL,
];

/// The floor under every mocked spec's backend (backlog f88e7908,
/// 2026-09-19). Installed FIRST, so everything a spec registers after
/// it wins (routes match last-registered-first), it answers whatever
/// the spec did not: `[]` to any /api/** read, and the empty-but-valid
/// shape of its own endpoint where that shape is not a list. Without
/// it a spec that mocked only what it tested let the shell's reads
/// reach the dev-server — 403 misses across 16 paths per full run,
/// from five specs — and eight of those paths never showed in the old
/// connect noise because the proxy had no port for them.
///
/// What it does NOT do: mock an identity. `/api/session` stays with
/// the dev-server's own audit-readonly demo session, and `/api/auth/me`
/// answers 401 — the gateway's answer for that session, and what
/// SignInControl reads as "Sign in". A spec that needs a persona mocks
/// `/api/people` + `/api/session` itself (installSmokeMocks does). A
/// detail read for an id nothing seeded answers 404, the registry's
/// own answer for a kind it does not hold; a spec that relies on a 404
/// for its error-state rendering now gets it from here rather than
/// from the dev-server's miss.
export async function installApiFloor(page: Page): Promise<void> {
  await page.route('**/api/**', (r) => json(r, []));

  // Identity probes: nobody. The SPA's 401 interceptor exempts
  // `/api/auth/*`, so this is the answer, not a redirect.
  await page.route(/\/api\/auth\/me$/, (r) => json(r, 'no session', 401));
  // The two /login availability probes (`.enabled === true` is the
  // read): neither door is offered.
  await page.route(/\/api\/auth\/(guest|oidc\/available)$/, (r) => json(r, { enabled: false }));

  // What the chrome asks on every mount.
  // The unread badge: `{ count }` (boss-messages' UnreadResponse).
  await page.route(/\/api\/messages\/unread\/[^/]+(\?|$)/, (r) => json(r, { count: 0 }));
  // The route-open record: a fire-and-forget POST the API answers 204.
  await page.route(/\/api\/surface-opens$/, (r) => r.fulfill({ status: 204 }));
  // Server-sent streams (the sim clock, a job's, the event pulse's): a
  // 204 fails the EventSource cleanly — no reconnect — so the client
  // falls back to its poll, which the objects below answer.
  await page.route(/\/api\/.+\/stream(\?|$)/, (r) => r.fulfill({ status: 204 }));

  // Live job state (objects, not lists — `[]` would break these).
  await page.route(JOBS_LIVE, (r) => json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(JOBS_SUMMARY, (r) => json(r, { counts: {}, total: 0 }));
  // The yard status read-model (object, not a list). An empty-but-well-
  // formed payload so the page renders its "no trains / no cars" states.
  await page.route(YARD_STATUS, (r) =>
    json(r, {
      trains: [], dock: [], recent: [], stranded: [],
      boarding: { dock_threshold: null, cooldown_minutes: null, at_times: [], dock_depth: 0, threshold_met: null, summary: 'No boarding cadence is configured.' },
      gates: { capacity: 3, active: [] },
      garage: [],
      policy: { stall_hours: null, max_red_trains: null },
      now: '2026-09-03T12:00:00Z',
    }),
  );

  // The IT system map's regions (design 0524fc95, car 2): the /it
  // landing reads this ONE endpoint. An empty-but-well-formed map —
  // eight regions, each clear with a count of 0 and a trend with no
  // samples — so the page draws eight cards and the crawl walks eight
  // doors. Under the `[]` catch-all the page would render a failure
  // line (a list where the map is due is a malformed read), which is
  // right for an outage and wrong for the empty leg.
  await page.route(YARD_REGIONS, (r) =>
    json(r, {
      window_hours: 24,
      now: '2026-09-03T12:00:00Z',
      regions: ['dock', 'gates', 'track', 'shed', 'arrivals', 'garage', 'receiving', 'marshalling'].map((name) => ({
        name, count: 0, state: 'clear', why: 'nothing here',
        trend: { metric: 'nothing measured', unit: 'per day', current: null, previous: null, samples: 0, previous_samples: 0 },
      })),
    }),
  );
  // The map's RAILS (design d2154293, car 2), for the same reason: the
  // empty leg is eight quiet borders, not a failed read. The hops are
  // world.ts's, which the server's table is pinned equal to.
  await page.route(YARD_BORDERS, (r) =>
    json(r, {
      window_hours: 24,
      now: '2026-09-03T12:00:00Z',
      borders: [
        ['receiving', 'marshalling'], ['marshalling', 'dock'], ['dock', 'gates'], ['gates', 'track'],
        ['track', 'arrivals'], ['arrivals', 'shed'], ['gates', 'garage'], ['track', 'garage'],
      ].map(([from, to]) => ({
        from, to, crossing: 'nothing measured', state: 'clear', why: 'nothing waiting',
        rate: { metric: 'crossings', unit: 'per day', current: 0, previous: 0, samples: 0, previous_samples: 0 },
        last_crossed: null, waiting: 0, holds: [],
        machine: { name: 'nothing', kind: 'actors', last_fired: null, silent_for_minutes: null,
          expected_every_minutes: null, silent: null, why: 'no firing recorded' },
      })),
    }),
  );

  // The other object reads, empty; the detail reads, absent.
  await page.route(DISPATCHER_RULES, (r) => json(r, { rules: [], handler_emits: {}, system_edges: [] }));
  await page.route(GATEWAY_PERF, (r) => json(r, { endpoints: [], window_started_at: '2026-01-01T00:00:00Z' }));
  for (const detail of [WORKFLOW_DETAIL, MARKETING_ASSET_DETAIL, SHIPMENT_DETAIL, VIEW_RESULTS]) {
    await page.route(detail, (r) => json(r, 'not found', 404));
  }
}

export async function installSmokeMocks(page: Page): Promise<void> {
  // Strip the bun dev-server HMR overlay. Identity comes from the
  // mocked `/api/session` below, not from anything stored client-side.
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });

  // The floor FIRST (lowest priority): every read the fixtures below
  // do not name still resolves, so the page mounts. Routes registered
  // later take precedence.
  await installApiFloor(page);

  // Identity / session: a persona, so `/api/auth/me` is someone.
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await page.route(/\/api\/session$/, (r) => json(r, {}));
  await page.route(/\/api\/auth\/me$/, (r) => json(r, {}));

  // Workflow registry + the adversarial kind (omitted-terminal step).
  await page.route(/\/api\/workflows$/, (r) => json(r, [WORKFLOW]));
  await page.route(WORKFLOW_DETAIL, (r) => json(r, WORKFLOW));
  await page.route(/\/api\/workflows\/[^/]+\/versions$/, (r) => json(r, [WORKFLOW]));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'generic', label: 'Generic', category: 'generic', ux: 'inline', description: '' },
    { kind: 'task', label: 'Task', category: 'generic', ux: 'inline', description: '' },
  ]));
  await page.route(/\/api\/jobs\/step-plugins$/, (r) => json(r, [
    { kind: 'demo', label: 'Demo', category: 'generic', version: 1, frontend_url: '/plugins/demo.js', owning_team: 'platform' },
  ]));

  // Dispatcher cascade.
  // Two rows: an event-triggered rule, and a SCHEDULED one shaped like
  // the real registry's — `schedule` and NO `on_event` (the registry is
  // on_event XOR schedule), with a tenant `source`. The first nightly
  // playground crawl (car 01180167, 2026-09-18) found the cascade page
  // throwing on that shape while this mock, then one row that always
  // carried on_event, rendered it green (backlog ee86a789).
  await page.route(DISPATCHER_RULES, (r) => json(r, {
    rules: [
      { name: 'r1', on_event: 'step.done.task', when: null, do: [{ handler: 'h1', args: {} }], version: 1, source: 'product' },
      { name: 'sweep-daily', schedule: { cadence: 'daily', anchor_date: '2026-09-09' }, when: null, do: [{ handler: 'h1', args: {} }], version: 1, source: 'tenant:brewery', authored: false, why: null },
    ],
    handler_emits: { h1: ['x.y'] }, system_edges: [],
  }));

  // Taxonomy registries (Subjects & Classes + System Model hub).
  await page.route(/\/api\/subject-kinds$/, (r) => json(r, [
    { kind: 'person', label: 'Person', parent_kind: null, description: null, owning_team: 'platform', metadata: {}, sort_order: 1, retired_at: null },
    { kind: 'employee', label: 'Employee', parent_kind: 'person', description: null, owning_team: 'platform', metadata: {}, sort_order: 1, retired_at: null },
  ]));
  // Class row with `retired_at` OMITTED (adversarial), plus the
  // platform's own department rows (the chrome bar's tabs).
  await page.route(/\/api\/classes(\?|$)/, (r) => json(r, [
    { subject_kind: 'employee', code: 'ceo', display_name: 'CEO', parent_code: null, member_attribute: 'role', metadata: {}, sort_order: 1 },
    ...DEPARTMENT_CLASSES,
  ]));

  // Gateway perf histogram (PerfPage iterates `.endpoints`).
  await page.route(GATEWAY_PERF, (r) => json(r, { endpoints: [], window_started_at: '2026-01-01T00:00:00Z' }));

  // Audit tail (the event pulse).
  await page.route(/\/api\/events\/tail(\?|$)/, (r) => json(r, [
    { event_id: 'e1', timestamp: '2026-01-01T00:00:00Z', source: 'jobs', kind: 'jobs.step.updated', payload: {} },
  ]));

  // Marketing assets (optionals omitted).
  await page.route(/\/api\/catalog\/marketing-assets(\?|$)/, (r) => json(r, [MARKETING_ASSET]));
  await page.route(/\/api\/catalog\/marketing-assets\/[^/]+\/history$/, (r) => json(r, []));
  await page.route(MARKETING_ASSET_DETAIL, (r) => json(r, MARKETING_ASSET));

  // Shipments (carrier omitted).
  // Views — the Home composer surface. Two rows so the crawler renders
  // both visibility badges, and a results payload with `truncated` set
  // so the ceiling warning is exercised rather than only the happy path.
  // The tenant manifest now carries the tenant's own name, which the
  // chrome bar renders. Mocked so the brand assertions below have
  // something deterministic to read. A module is on only when listed
  // true (ce68f137), so the playground persona lists every module the
  // SPA gates — the crawl has to reach the pages behind them.
  await page.route(/\/api\/tenant\/manifest$/, (r) =>
    json(r, {
      display_name: 'Algedonic Ales',
      tenant_id: 'brewery',
      modules: MODULES_ON,
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
  await page.route(VIEW_RESULTS, (r) =>
    json(r, {
      view_id: 'view-1', source: 'jobs', layout: 'table',
      rows: [{ id: 'j-1', status: 'open' }],
      matched: 1, truncated: false,
    }),
  );
  await page.route(/\/api\/shipping\/shipments(\?|$)/, (r) => json(r, [SHIPMENT]));
  await page.route(SHIPMENT_DETAIL, (r) => json(r, SHIPMENT));
}
