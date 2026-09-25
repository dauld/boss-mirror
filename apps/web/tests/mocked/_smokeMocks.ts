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
// The world's own layout, so the empty leg below cannot list a region
// or a hop the map does not draw (backlog 94c6ffd0: both lists were
// typed out here and went stale the day a ninth region landed).
// world.ts is pinned to the server's REGIONS and BORDERS by
// world.test.ts and borders.test.ts, so this is one definition deep.
import { BORDERS, TERRITORIES } from '../../src/it/yard/world';
import LIVE_RECORDING from './live-tenant-manifest.json' with { type: 'json' };

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

/// Every module id the SPA gates on (nav-catalog `module`, which since
/// backlog f9b43965 also answers for the route gate through
/// `moduleForRoute`, plus the Simulator tab and DebugGear), all on —
/// the playground tenant's manifest, in the shape the tenant contract
/// documents.
export const MODULES_ON: Readonly<Record<string, boolean>> = {
  calendar: true, equipment: true, exec: true, finance: true, 'marketing-assets': true,
  parts: true, qa: true, shipping: true, support: true, warehouse: true, shop: true, sim: true,
};

/// The LIVE tenant's modules: the `modules` of the manifest the live
/// gateway served, read out of live-tenant-manifest.json — a RECORDING,
/// with the time it was taken and the read that took it, never a shape
/// typed here. MODULES_ON above is shaped so a module gate cannot be
/// seen at all — every gate answers "on" — which is how /ux/support
/// stayed gated on 'shipping' and labelled "Shipments" with no mocked
/// spec able to notice (backlog 5b2f3240, gap 10 of the /ux/support
/// audit). A spec that pins a gate "as live" installs this shape through
/// installTenantManifest, after installSmokeMocks.
///
/// This was a literal `{}` documented as the live manifest of
/// 2026-09-19, and it went on saying so after the instance began serving
/// eleven keys with exec, finance and support true — so every spec that
/// rendered "the live instance" rendered those three off, and the
/// products spec kept a second, correct copy beside it (41454ce1). A
/// mocked run cannot read the instance, so the recording cannot refresh
/// itself; what it can do is carry its date into every test title that
/// renders it, and be one file to overwrite — the body verbatim from the
/// read it names — after which every "as live" leg re-judges the page
/// against the new shape and fails by name where the page changed.
export const MODULES_LIVE: Readonly<Record<string, boolean>> = LIVE_RECORDING.body.modules;

/// When the recording above was taken (UTC, as `date -u` read it). Specs
/// put it in the titles of their live legs, so a reader of a run sees
/// how old "live" is.
export const LIVE_MANIFEST_RECORDED_AT: string = LIVE_RECORDING.recorded_at;

/// A manifest listing no modules: what the gateway answers when it finds
/// no tenant.toml (api.rs `tenant_manifest_now`), and what the live one
/// served until it did not. Every module is off. This is its own shape,
/// not "live", so a leg that means "nothing listed" keeps meaning it
/// whatever the recording says.
export const MODULES_NONE: Readonly<Record<string, boolean>> = {};

/// `GET /api/tenant/manifest` with the given modules — the one place a
/// spec says which tenant it is rendering for. Registered routes win in
/// reverse order, so calling this after installSmokeMocks replaces the
/// all-on manifest the crawl needs.
export async function installTenantManifest(
  page: Page,
  modules: Readonly<Record<string, boolean>>,
): Promise<void> {
  await page.route(/\/api\/tenant\/manifest$/, (r) =>
    json(r, {
      display_name: 'Algedonic Ales',
      tenant_id: 'brewery',
      modules,
      labels: {},
    }),
  );
}

/// The platform's department Classes (01-registries.sql) as `/api/classes`
/// rows — the EMPLOYEE DRAWER: the values an employee's `department`
/// column may take, which the policy flyout's scope picker reads.
/// The chrome bar stopped deriving its tabs from these in dc5788ba;
/// `DEPARTMENTS` below is what it reads now.
export const DEPARTMENT_CLASSES: ReadonlyArray<Record<string, unknown>> = [
  ['it', 'IT'], ['executive', 'Executive'], ['sales', 'Sales'], ['service', 'Service'],
  ['refurb', 'Refurb'], ['qa', 'QA'], ['warehouse', 'Warehouse'], ['finance', 'Finance'],
  ['people', 'People'], ['support', 'Support'], ['marketing', 'Marketing'],
].map(([code, display_name], i) => ({
  subject_kind: 'employee', code, display_name, parent_code: null, member_attribute: 'department',
  metadata: {}, sort_order: (i + 1) * 10, retired_at: null,
}));

/// The departments registry as `GET /api/departments` serves it — the
/// chrome bar derives its tabs from these since dc5788ba, so a mock
/// with no departments is a bar with no department tabs. The SAME
/// codes as the drawer above, derived rather than listed a second
/// time: which roster answers is the point of that car, and the mock
/// has no second org chart to tell them apart with. The endpoint has
/// already dropped retired rows and sorted, so the wire carries
/// neither field. Shared with the chrome specs that mock no other
/// backend.
export const DEPARTMENTS: ReadonlyArray<Record<string, unknown>> = DEPARTMENT_CLASSES.map((c) => ({
  code: c.code, display_name: c.display_name, function: 'operations',
}));

/// `GET /api/departments` — the bare list, not the per-department
/// readiness read, which the SPA does not make.
export const DEPARTMENTS_ENDPOINT = /\/api\/departments(\?|$)/;

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
  // The org chart the chrome bar's tabs come from (dc5788ba). A shell
  // read like the manifest and the Class registries beside it: a crawl
  // whose bar has lost its department tabs is measuring a different
  // page.
  DEPARTMENTS_ENDPOINT,
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
/// The audit log's size-and-growth read (boss-events AuditStats). The
/// fixture /it/operate/audit sat in DEFERRED waiting for ("snapshot .length
/// needs a faithful fixture"; page audit 65a273d5, gap 0398c4d0): a `[]`
/// here paints `undefined.toLocaleString()` and the page throws.
export const EVENTS_STATS = /\/api\/events\/stats$/;
/// The churn watchlist's scores, `{ accounts }` (RiskScoreListSchema):
/// a `[]` is a wrong shape the page reports on the failure marker.
export const RISK_SCORES = /\/api\/people\/accounts\/risk-scores(\?|$)/;
/// The audit log's live stream. Not an object, but not a list either: a
/// JSON `[]` is not an event stream, so the EventSource fails and the
/// page says the stream is down, on the failure marker (sweep c3e4edcc).
export const EVENTS_STREAM = /\/api\/events\/stream(\?|$)/;
/// The /ux/finance statements (page audit 3f964c57, backlog e0732f75):
/// the fixtures the route sat in DEFERRED waiting for ("statements
/// .reduce needs object-shaped fixtures"). Each is a single object the
/// page reads fields off — under the `[]` catch-all the commerce
/// summary's `ar_aging.reduce`, AP aging's `total_invoice_count
/// .toLocaleString()` and every statement's `revenue.length` throw.
export const COMMERCE_SUMMARY = /\/api\/commerce\/summary$/;
export const AP_AGING = /\/api\/inventory\/ap-aging$/;
/// The `{ data: [...] }` envelopes the marshalling board and /it/design
/// read through the shared envelope reader (src/data/shape.ts,
/// backlog 67825067): a bare `[]` there is a malformed read the page
/// paints as its failure line, which is right for a broken backend and
/// wrong for the empty leg.
export const STATIONS_LOAD = /\/api\/stations\/load$/;
export const STATIONS_FLOW = /\/api\/stations\/flow(\?|$)/;
export const QUEUE_AGE = /\/api\/jobs\/queue-age$/;
export const DESIGN_STATION_QUEUES = /\/api\/stations\/design-(review|decided)\/queue$/;
export const LEDGER_STATEMENTS =
  /\/api\/ledger\/(income-statement|balance-sheet|cash-flow|trial-balance|deferred-revenue-runoff|tax-liability)(\?|$)/;
export const EMPTY_COMMERCE_SUMMARY = {
  revenue_ttm: [], total_revenue_ttm_cents: 0, total_cogs_ttm_cents: 0,
  total_gross_margin_ttm_cents: 0, ar_aging: [], total_outstanding_cents: 0,
  total_invoice_count: 0, revenue_by_month: [], currency: 'USD',
} as const;
export const EMPTY_AP_AGING = {
  buckets: [], total_outstanding_cents: 0, total_invoice_count: 0, currency: 'USD',
} as const;
/// The empty-but-valid body of each ledger statement (apps/web/src/finance
/// ledger.ts's types), keyed by the path LEDGER_STATEMENTS matched. The
/// cash-flow path answers two shapes, told apart by `?method=direct`.
export function emptyLedgerStatement(url: string): unknown {
  const u = new URL(url);
  const period = { from: '2026-01-01', to: '2026-09-03' };
  switch (u.pathname.split('/').pop()) {
    case 'income-statement':
      return { ...period, revenue: [], total_revenue_cents: 0, cogs: [], total_cogs_cents: 0,
        gross_profit_cents: 0, operating_expenses: [], total_operating_expenses_cents: 0,
        net_income_cents: 0, currency: 'USD' };
    case 'balance-sheet':
      return { as_of: '2026-09-03', assets: [], total_assets_cents: 0, liabilities: [],
        total_liabilities_cents: 0, equity: [], total_equity_cents: 0, imbalance_cents: 0,
        balanced: true, currency: 'USD' };
    case 'cash-flow':
      return u.searchParams.get('method') === 'direct'
        ? { ...period, method: 'direct', cash_in_from_customers_cents: 0, cash_out_to_vendors_cents: 0,
            cash_out_to_employees_cents: 0, cash_out_to_authorities_cents: 0, net_change_in_cash_cents: 0,
            gl_cash_pool_delta_cents: 0, gl_cash_1000_delta_cents: 0, reconciliation_gap_cents: 0,
            reconciled: true, currency: 'USD' }
        : { ...period, net_income_cents: 0, operating_activities: [], working_capital_adjustments: [],
            non_cash_adjustments: [], cash_from_operations_cents: 0, investing_activities: [],
            cash_from_investing_cents: 0, financing_activities: [], cash_from_financing_cents: 0,
            net_change_in_cash_cents: 0, cash_start_cents: 0, cash_end_cents: 0,
            reconciliation_gap_cents: 0, reconciled: true, currency: 'USD' };
    case 'trial-balance':
      return { as_of: '2026-09-03', rows: [], total_debits_cents: 0, total_credits_cents: 0,
        balanced: true, currency: 'USD' };
    case 'deferred-revenue-runoff':
      return { as_of: '2026-09-03', horizon_months: 12, deferred_account_balance_cents: 0,
        schedules_remaining_cents: 0, drift_cents: 0, months: [], beyond_horizon_cents: 0, currency: 'USD' };
    default:
      return { as_of: '2026-09-03', liabilities: [], accrued_filings: [], next_due: null, currency: 'USD' };
  }
}
/// The live read's figures on 2026-09-23 21:44Z (the audit's controls_md,
/// read 1), trimmed to two days and three kinds.
export const AUDIT_STATS = {
  total_rows: 384521,
  table_bytes: 515522560,
  oldest_at: '2026-09-16T23:54:00Z',
  newest_at: '2026-09-23T21:44:00Z',
  rows_last_24h: 78516,
  rows_last_7d: 420000,
  per_day: [
    { day: '2026-09-22', rows: 71000 },
    { day: '2026-09-23', rows: 73907 },
  ],
  top_kinds: [
    { kind: 'jobs.step.updated', rows: 120000 },
    { kind: 'dispatcher.rule.fired', rows: 60000 },
    { kind: 'credential.rotate.verified', rows: 3 },
  ],
} as const;
export const OBJECT_ENDPOINTS: ReadonlyArray<RegExp> = [
  JOBS_LIVE, JOBS_SUMMARY, YARD_STATUS, YARD_REGIONS, YARD_BORDERS, WORKFLOW_DETAIL, DISPATCHER_RULES, GATEWAY_PERF,
  MARKETING_ASSET_DETAIL, VIEW_RESULTS, SHIPMENT_DETAIL, EVENTS_STATS, RISK_SCORES, EVENTS_STREAM,
  COMMERCE_SUMMARY, AP_AGING, LEDGER_STATEMENTS,
  STATIONS_LOAD, STATIONS_FLOW, QUEUE_AGE, DESIGN_STATION_QUEUES,
  // `{data, total}`, not a list: a bare `[]` here is the shape a wrong
  // endpoint answers, and the bar reads it as a failed roster rather
  // than an empty one — deliberately, so the org chart cannot go
  // missing quietly (libs/web-kit/src/nav.ts).
  DEPARTMENTS_ENDPOINT,
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
  // The departments registry — the bar's tabs (dc5788ba). An object,
  // so the `[]` catch-all above would read as a failed roster.
  await page.route(DEPARTMENTS_ENDPOINT, (r) => json(r, { data: DEPARTMENTS, total: DEPARTMENTS.length }));
  // The unread badge: `{ count }` (boss-messages' UnreadResponse).
  await page.route(/\/api\/messages\/unread\/[^/]+(\?|$)/, (r) => json(r, { count: 0 }));
  // The route-open record: a fire-and-forget POST the API answers 204.
  await page.route(/\/api\/surface-opens$/, (r) => r.fulfill({ status: 204 }));
  // Server-sent streams (the sim clock, a job's, the event pulse's): a
  // 204 fails the EventSource cleanly — no reconnect — so the client
  // falls back to its poll, which the objects below answer.
  await page.route(/\/api\/.+\/stream(\?|$)/, (r) => r.fulfill({ status: 204 }));
  // Except the audit log's, the one stream whose refusal a page reports
  // as a failed read: its "Live stream down" line wears the failure
  // marker since sweep c3e4edcc, so under the 204 the empty leg found a
  // marker on a healthy backend. A well-formed empty stream that ends
  // instead — the browser waits `retry` to reconnect, and the page says
  // it is reconnecting, which is no failure.
  await page.route(EVENTS_STREAM, (r) =>
    r.fulfill({
      status: 200,
      headers: { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' },
      body: 'retry: 3600000\n\n',
    }),
  );

  // Live job state (objects, not lists — `[]` would break these).
  await page.route(JOBS_LIVE, (r) => json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(JOBS_SUMMARY, (r) => json(r, { counts: {}, total: 0 }));
  // The churn watchlist's scores: `{ accounts }` (RiskScoreListSchema).
  // Under the `[]` catch-all the page parses a wrong shape and says so on
  // the failure marker (sweep c3e4edcc) — right for a broken backend,
  // wrong for the empty leg.
  await page.route(RISK_SCORES, (r) => json(r, { accounts: [] }));
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
  // one region per declared territory, each clear with a count of 0 and
  // a trend with no samples — so the page draws every card and the
  // crawl walks every door. Under the `[]` catch-all the page would
  // render a failure line (a list where the map is due is a malformed
  // read), which is right for an outage and wrong for the empty leg.
  await page.route(YARD_REGIONS, (r) =>
    json(r, {
      window_hours: 24,
      now: '2026-09-03T12:00:00Z',
      regions: TERRITORIES.map(({ name }) => ({
        name, count: 0, state: 'clear', why: 'nothing here',
        trend: { metric: 'nothing measured', unit: 'per day', current: null, previous: null, samples: 0, previous_samples: 0 },
      })),
    }),
  );
  // The map's RAILS (design d2154293, car 2), for the same reason: the
  // empty leg is a quiet border per declared hop, not a failed read.
  // The hops are world.ts's, which the server's table is pinned equal
  // to.
  await page.route(YARD_BORDERS, (r) =>
    json(r, {
      window_hours: 24,
      now: '2026-09-03T12:00:00Z',
      borders: BORDERS.map(({ from, to }) => ({
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
  await page.route(EVENTS_STATS, (r) => json(r, {
    total_rows: 0, table_bytes: 0, oldest_at: null, newest_at: null,
    rows_last_24h: 0, rows_last_7d: 0, per_day: [], top_kinds: [],
  }));
  await page.route(COMMERCE_SUMMARY, (r) => json(r, EMPTY_COMMERCE_SUMMARY));
  await page.route(AP_AGING, (r) => json(r, EMPTY_AP_AGING));
  await page.route(LEDGER_STATEMENTS, (r) => json(r, emptyLedgerStatement(r.request().url())));
  // The envelope reads, well-formed and empty: every station answered
  // and nothing stands, so the board paints its clear state and the
  // design page its two empty lines (backlog 67825067).
  await page.route(STATIONS_LOAD, (r) => json(r, { data: [], total: 0, distinct_packets: 0 }));
  await page.route(STATIONS_FLOW, (r) => json(r, { window_hours: 24, as_of: '2026-09-03T12:00:00Z', data: [] }));
  await page.route(QUEUE_AGE, (r) => json(r, { data: [], total: 0, now: '2026-09-03T12:00:00Z' }));
  await page.route(DESIGN_STATION_QUEUES, (r) => {
    const station = /design-(review|decided)/.exec(r.request().url())?.[0] ?? 'design-review';
    return json(r, { station, kind: 'batch', discipline: [], total: 0, data: [], steps: {} });
  });
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
  await installTenantManifest(page, MODULES_ON);
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
