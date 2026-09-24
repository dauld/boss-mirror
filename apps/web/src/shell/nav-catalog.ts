// The nav catalog — the single registry of every routable nav entry:
// its label, path, permKey, tenant-module gate, and which top-level
// **app** it belongs to.
//
// This lived inside AppShell.svelte's `<script>` block. It moved out
// for two reasons, both about drift:
//
//  1. App membership was duplicated. `AppShell.svelte` carried a
//     `MODEL_ROUTES: Set<RouteName>` (driving which sidebar entries
//     show) and `App.svelte` carried a `MODEL_KINDS: Set<Route['kind']>`
//     (driving which tab highlights) — two sets, keyed off two
//     different vocabularies, that had to agree for every routed
//     surface or a page would render under the wrong tab. The comment
//     on each said as much. `app` below is now the only place that
//     answers "which app does this surface belong to", and both
//     consumers derive from it.
//
//  2. `sidebar-router-consistency.test.ts` hand-mirrored every sidebar
//     path, because (its header explains) parsing a TypeScript const
//     out of a Svelte `<script>` from a Bun test is fragile. As a
//     plain module the test imports the real thing, so the mirror —
//     and the drift it was there to catch — is gone.
//
// Adding a surface: add one entry here with its `app`, and add a
// matching branch to router.ts. The consistency test enforces the
// second half.

import type { RouteName } from '@boss/web-kit/session/permissions';
import type { AppId, AppTab, Department } from '@boss/web-kit/nav';
import { HOME_APP, SIMULATOR_APP } from '@boss/web-kit/nav';

export type { AppId, AppTab };

// `AppId` and the tab list live in @boss/web-kit/nav (the bar is
// rendered by apps/web AND apps/simulator). THIS file answers the
// other half — which surface belongs to which app — because web-kit
// has no business knowing about /ux/warehouse.

export type NavItem = Readonly<{
  id: string;
  label: string;
  path: string;
  permKey?: RouteName;
  /// Tenant module that this nav entry belongs to. When the
  /// manifest disables the module (e.g. brewery turns off
  /// `equipment` and `shipping`), the entry is hidden. Items
  /// without a module field are always-on (e.g. /jobs).
  module?: string;
  /// Which app owns this surface. Required on every catalog entry —
  /// an unassigned surface is how a page ends up rendering under the
  /// wrong tab. Plain sub-page links (Audit Log, Atlas) declared
  /// inline in a nav group carry no `app`; they inherit the group
  /// they sit in.
  app?: AppId;
  /// The department whose packets this surface lists, by Class code.
  ///
  /// This is here for the reason `app` is (backlog 423a531d,
  /// 2026-09-22): what a surface FILTERS on is part of what the
  /// surface is, and it had no home. Two routes carried a workflow
  /// kind as a literal prop instead — `initialKind="field-service"`
  /// and `initialKind="sale"`, both tenant kinds this instance does
  /// not publish — so both pages rendered a title and a permanent
  /// "No jobs match" with nothing to catch it.
  ///
  /// A DEPARTMENT, not a kind, because a department's work is several
  /// protocols: Sales runs `receive-a-sponsorship` AND
  /// `receive-an-inquiry`, so no single kind= could express it even
  /// once the literal was corrected. The server resolves a department
  /// to the kinds whose workflow row declares it
  /// (`/api/jobs?department=<code>`), which keeps the mapping in
  /// registry data where it belongs.
  department?: string;
}>;

export type NavGroup = Readonly<{ label: string; items: ReadonlyArray<NavItem> }>;

/// Catalog keys that are surfaces without a permission key of their
/// own. `RouteName` (libs/web-kit) is the role-gating vocabulary;
/// these entries are visible to every role by construction (like the
/// permKey-less Audit Log / Atlas rows), so they extend the CATALOG
/// without widening the PERMISSION vocabulary — the catalog still
/// answers "which app owns this surface" and "which sidebar row
/// highlights" for them.
// 'system-fleet' left the permission vocabulary with the 2026-08-31
// consolidation (its tab gates under system-monitoring), but the
// catalog still answers "which app / which row" for its route kind.
// 'hr', 'watchlist', 'manual' (CAR-6, 6edb1b77): routable since their
// pages shipped but never listed here, so a 574-line HR page and the
// app's best list were typed-URL-only. Like system-fleet they borrow
// gate parity from the surface they sit beside (hr → 'people',
// watchlist → 'accounts') rather than widening the vocabulary; the
// manual is permKey-less like the docs it renders.
export type UngatedSurfaceId =
  | 'system-incidents'
  // 'system-crew' (the Crew Board): permKey-less like the Operate row
  // above, and for the same reason — it is a read-only observation
  // surface over the delivery pipeline, readable by any operator, and
  // adding a permKey would mean widening the RouteName vocabulary in
  // libs/web-kit and every tenant's declared `surfaces` lists.
  | 'system-crew'
  // 'system-receiving' / 'system-marshalling' (the Receiving Yard and
  // Marshalling Yard rows): permKey-less for the Crew Board's reason —
  // a yard is the department's own floor, readable by any operator
  // (feedback 92921c2f, design 55417146, 2026-09-18).
  | 'system-receiving'
  | 'system-marshalling'
  // 'system-codebase' (the Codebase row): the department's own numbers,
  // readable by any operator — same shape as the Crew Board, and for
  // the same reason (feedback 9827c699, 2026-09-14).
  | 'system-codebase'
  // 'system-registry-drift' (the Drift tab on Registry): a view of the
  // workflow registry against its authored bundle, so it borrows the
  // `workflows` gate of the family it sits in rather than widening the
  // RouteName vocabulary (4ae9969e, 2026-09-15).
  | 'system-registry-drift'
  | 'system-fleet'
  | 'system-backlog'
  | 'hr'
  | 'watchlist'
  | 'manual';

export const ROUTE_CATALOG: Readonly<Record<RouteName | UngatedSurfaceId, NavItem>> = {
  jobs:      { id: 'jobs',      label: 'All jobs',         path: '/ux/jobs',      permKey: 'jobs',      app: 'home' },
  sales:     { id: 'sales',     label: 'Sales pipeline',   path: '/ux/sales',     permKey: 'sales',     app: 'sales', department: 'sales' },
  service:   { id: 'service',   label: 'Service queue',    path: '/ux/service',   permKey: 'service',   module: 'support', app: 'service', department: 'support' },
  qa:        { id: 'qa',        label: 'QA',               path: '/ux/qa',        permKey: 'qa',        module: 'qa',      app: 'qa' },
  finance:   { id: 'finance',   label: 'Finance',          path: '/ux/finance',   permKey: 'finance',   module: 'finance', app: 'finance' },
  warehouse: { id: 'warehouse', label: 'Inventory',        path: '/ux/warehouse', permKey: 'warehouse', module: 'warehouse', app: 'warehouse' },
  shipping:  { id: 'shipping',  label: 'Shipments',        path: '/ux/shipping',  permKey: 'shipping',  module: 'shipping', app: 'distribution' },
  support:   { id: 'support',   label: 'Support',          path: '/ux/support',   permKey: 'support',   module: 'support', app: 'support' },
  exec:      { id: 'exec',      label: 'Exec',             path: '/ux/exec',      permKey: 'exec',      module: 'exec',    app: 'executive' },
  schedule:  { id: 'schedule',  label: 'My schedule',      path: '/ux/calendar/me', permKey: 'schedule', app: 'home' },
  catalog:   { id: 'catalog',   label: 'Equipment',        path: '/ux/catalog',   permKey: 'catalog',   module: 'equipment', app: 'maintenance' },
  parts:     { id: 'parts',     label: 'Ingredients & parts', path: '/ux/parts',  permKey: 'parts',     module: 'parts',   app: 'warehouse' },
  products:  { id: 'products',  label: 'Products',         path: '/ux/products',  permKey: 'parts',     module: 'parts',   app: 'production' },
  accounts:  { id: 'accounts',  label: 'Accounts',         path: '/ux/accounts',  permKey: 'accounts',  app: 'sales' },
  vendors:   { id: 'vendors',   label: 'Vendors',          path: '/ux/vendors',   permKey: 'vendors',   app: 'finance' },
  people:    { id: 'people',    label: 'Employees',        path: '/ux/people',    permKey: 'people',    app: 'people' },
  assets:    { id: 'assets',    label: 'Assets',           path: '/ux/assets',    permKey: 'assets',    module: 'equipment', app: 'maintenance' },
  shop:      { id: 'shop',      label: 'Shop',             path: '/ux/shop',      permKey: 'shop',      module: 'shop',    app: 'sales' },
  inbox:     { id: 'inbox',     label: 'Inbox',            path: '/ux/inbox',     permKey: 'inbox',     app: 'home' },
  views:     { id: 'views',     label: 'Views',            path: '/ux/views',     permKey: 'views',     app: 'home' },
  'marketing-assets': { id: 'marketing-assets', label: 'Marketing assets', path: '/ux/marketing-assets', permKey: 'marketing-assets', module: 'marketing-assets', app: 'marketing' },
  calendar:  { id: 'calendar',  label: 'Release calendar', path: '/ux/calendar',  permKey: 'calendar',  module: 'calendar', app: 'production' },
  hr:        { id: 'hr',        label: 'HR',               path: '/hr',           permKey: 'people',    app: 'people' },
  watchlist: { id: 'watchlist', label: 'Churn watchlist',  path: '/watchlist',    permKey: 'accounts',  app: 'sales' },
  manual:    { id: 'manual',    label: 'Manual',           path: '/manual',       app: 'home' },

  // The IT department — SIX surfaces (the 2026-08-31 consolidation,
  // packet 1f6d55e0; was 17 pages, four of them dual-routed). The
  // catalog keeps non-sidebar entries only where a RouteName still
  // exists (tab pages, unlisted doors) so appForSection() can answer
  // for them; map/flow/model died outright and fleet became Operate's
  // Bottlenecks tab.
  // First IT surface in catalog order = the IT app's landing
  // (departure-board.md Q1): the yard, now AT /it itself. Catalog
  // order decides the LANDING (`departmentHref`), not the IT sidebar's
  // order — that is AppShell's IT_GROUPS list — which is how design
  // 55417146 (2026-09-18) keeps the Train Yard the /it landing while
  // the sidebar leads with the two yards upstream of it.
  'system-yard':              { id: 'system-yard',              label: 'Train Yard',          path: '/it',              permKey: 'system-yard',             app: 'it' },
  // The Receiving Yard and the Marshalling Yard — SIDEBAR ROWS since
  // David's feedback 92921c2f (2026-09-18): "graduate Receiving Yard
  // and Marshalling Yard to the left navbar ... the three yards plus
  // the Crew Board as the top 4". Second doors onto the Operate tabs
  // that already answer these paths — the routes did not move, and
  // the tabs stay. PermKey-less like the Crew Board: a yard is the
  // department's own floor, readable by any operator.
  'system-receiving':        { id: 'system-receiving',        label: 'Receiving Yard',      path: '/it/yard/receiving', app: 'it' },
  'system-marshalling':      { id: 'system-marshalling',      label: 'Marshalling Yard',    path: '/it/yard/marshalling', app: 'it' },
  // The Operate row is permKey-less like the incidents surface it
  // leads with — readable by any operator; the tabs behind it keep
  // their own gates.
  'system-incidents':        { id: 'system-incidents',        label: 'Operate',             path: '/it/operate',      app: 'it' },
  'system-monitoring':       { id: 'system-monitoring',       label: 'Monitoring',          path: '/it/operate/audit', permKey: 'system-monitoring',      app: 'it' },
  'system-fleet':            { id: 'system-fleet',            label: 'Bottlenecks',         path: '/it/operate/bottlenecks', permKey: 'system-monitoring', app: 'it' },
  workflows:                 { id: 'workflows',               label: 'Registry',            path: '/it/registry',     permKey: 'workflows',               app: 'it' },
  policy:                    { id: 'policy',                  label: 'Policy',              path: '/it/registry/policy', permKey: 'policy',               app: 'it' },
  'system-step-plugins':     { id: 'system-step-plugins',     label: 'Step plugins',        path: '/it/registry/step-plugins', permKey: 'system-step-plugins', app: 'it' },
  'system-dispatcher':       { id: 'system-dispatcher',       label: 'Dispatcher rules',    path: '/it/registry/dispatcher', permKey: 'system-dispatcher', app: 'it' },
  'system-subjects':         { id: 'system-subjects',         label: 'Subjects & Classes',  path: '/it/registry/subjects', permKey: 'system-subjects',    app: 'it' },
  'system-registry-drift':   { id: 'system-registry-drift',   label: 'Protocol drift',      path: '/it/registry/drift', permKey: 'workflows',             app: 'it' },
  'system-dispatcher-rules': { id: 'system-dispatcher-rules', label: 'Dispatcher rules — authoring', path: '/it/registry/rules', permKey: 'system-dispatcher-rules', app: 'it' },
  // The editor's path is a PATTERN, spelled the way surface-opens records
  // every open of it (routePattern). It shared the list's path until
  // backlog 3071e235 (2026-09-24), so the page march — one audit per
  // catalog path — never reached the page carrying all four rule writes.
  'system-dispatcher-rule':  { id: 'system-dispatcher-rule',  label: 'Dispatcher rule — editor',     path: '/it/registry/rules/:ruleName', permKey: 'system-dispatcher-rule',  app: 'it' },
  'system-design':           { id: 'system-design',           label: 'Design',              path: '/it/design',       permKey: 'system-design',           app: 'it' },
  'system-experiments':      { id: 'system-experiments',      label: 'Experiments',         path: '/it/design/experiments', permKey: 'system-experiments', app: 'it' },
  'system-feedback':         { id: 'system-feedback',         label: 'Feedback triage',     path: '/it/design/feedback', permKey: 'system-feedback',      app: 'it' },
  'system-backlog':          { id: 'system-backlog',          label: 'IT backlog',          path: '/it/design/backlog', permKey: 'system-feedback',      app: 'it' },
  // The hardware registry — declared beside observed beside the
  // difference, plus the dev-workspace door (59ef456a).
  // The Crew Board — the middle third of the operator surface. A SIDEBAR
  // ROW, not a tab: David's decision on backlog 04c5bbc0 (2026-09-11)
  // reversed the proposal to fold it into an existing IT family.
  'system-crew':             { id: 'system-crew',             label: 'Crew Board',          path: '/it/crew',         app: 'it' },
  // The Codebase — a SIDEBAR ROW, not the Design tab it was: David's
  // feedback 9827c699 (2026-09-14) asked for "a page to the IT department
  // showing the Code base stats" while the trend sat one tab in. permKey-
  // less like the Crew Board: the department's own numbers, readable by
  // any operator.
  'system-codebase':         { id: 'system-codebase',         label: 'Codebase',            path: '/it/codebase',     app: 'it' },
  'system-estate':           { id: 'system-estate',           label: 'Estate',              path: '/it/estate',       permKey: 'system-estate',           app: 'it' },
  'system-kb':               { id: 'system-kb',               label: 'Knowledge Base',      path: '/it/kb',           permKey: 'system-kb',               app: 'it' },
  // Unlisted door: reachable, never a sidebar row.
  'auth-admin':              { id: 'auth-admin',              label: 'Auth admin',          path: '/it/auth-admin',   permKey: 'auth-admin',              app: 'it' },
};

/// The apps this host offers: Home, Simulator when the tenant has one,
/// and one per department the tenant's Class registry declares.
///
/// DERIVED, not listed, and since ce68f137 derived from the REGISTRY:
/// the departments are the `(employee, *, department)` rows the SPA
/// loads at boot, in their sort order, so a tenant adds a tab by
/// adding a Class row (CLAUDE.md §9). The previous version filtered a
/// hardcoded list down to the departments owning a catalog surface,
/// which read one tenant's org chart and left Algedonic's
/// `operations` with no tab at all.
///
/// A department with no surface of its own still gets its tab — that
/// is the org chart, and the tenant declared it — and lands on its
/// own jobs view: the packets whose workflow declares the department,
/// as in / working / out (cc76f755, 2026-09-18). Until then it landed
/// on All jobs with Home highlighted, which read as "this department
/// has no work" for Algedonic's operations, sales, finance and
/// marketing. `departmentsWithoutSurfaces()` still reports which
/// departments have no built surface, so the gap stays visible
/// instead of reading as covered.
const OWNED = new Set<string>(
  Object.values(ROUTE_CATALOG)
    .map((e) => e.app)
    .filter((a): a is AppId => a !== undefined && a !== 'home' && a !== 'simulator'),
);

/// The department jobs view's path — one surface for every declared
/// department, keyed by its Class code. The router's `department`
/// route is the other half of this spelling.
export function departmentJobsPath(code: string): string {
  return `/ux/departments/${encodeURIComponent(code)}`;
}

/// Where a department's tab lands: its first surface in catalog order
/// — the same order the sidebar lists them in, so the tab opens on the
/// row the sidebar shows first — or its jobs view when it owns none.
/// IT is the one department whose sidebar order is its own list
/// (AppShell's IT_GROUPS): it leads with the two yards upstream of the
/// Train Yard and still lands on the Train Yard, because the catalog
/// keeps 'system-yard' first (design 55417146, 2026-09-18).
function departmentHref(code: string): string {
  return Object.values(ROUTE_CATALOG).find((e) => e.app === code)?.path ?? departmentJobsPath(code);
}

/// One tab per declared department, in registry order.
export function departmentApps(departments: ReadonlyArray<Department>): ReadonlyArray<AppTab> {
  return departments.map((d) => ({ id: d.code, label: d.label, href: departmentHref(d.code) }));
}

/// The full tab list, left to right. Simulator is a tab only for a
/// tenant whose manifest lists the `sim` module (ce68f137): it drives
/// the playground's model, and a company running on BOSS has no
/// simulation to drive.
export function appsFor(
  departments: ReadonlyArray<Department>,
  opts: Readonly<{ simulator: boolean }>,
): ReadonlyArray<AppTab> {
  return [HOME_APP, ...(opts.simulator ? [SIMULATOR_APP] : []), ...departmentApps(departments)];
}

/// Departments with no surface of their own. Not an error — a report.
export function departmentsWithoutSurfaces(
  departments: ReadonlyArray<Department>,
): ReadonlyArray<string> {
  return departments.filter((d) => !OWNED.has(d.code)).map((d) => d.code);
}

/// Which app a surface belongs to, looked up by the `activeSection`
/// id `App.svelte` derives from the current route. Unknown ids (the
/// `me` fallback, plain sub-pages) fall back to `user` — the same
/// answer the previous `MODEL_KINDS.has(route.kind)` check gave for
/// anything it didn't list.
export function appForSection(section: string): AppId {
  const entry = (ROUTE_CATALOG as Record<string, NavItem | undefined>)[section];
  // `me` (App.svelte's terminal fallback) and any plain sub-page id
  // resolve to Home — personal surfaces, which is where the fallback
  // belongs now that there is an app for them.
  return entry?.app ?? 'home';
}

/// Subject kinds each app is "about".
///
/// Feeds `app_kinds` on `/api/search`, which floats these to the top of
/// the dropdown — Q4's "prioritise results from the immediate app".
/// It is a ranking hint, never a filter: the whole value of a global
/// box is that it still finds the thing when you are looking in the
/// wrong app, so a CRM search for a part number must still surface the
/// part, just below the accounts.
///
/// Kinds may repeat across apps. An invoice is Finance's to reconcile
/// and CRM's to chase, and both are right — this maps attention, not
/// ownership. Home lists nothing: it is the cross-app surface, so it
/// prioritises nothing and shows the unweighted ranking.
/// Subject kinds each app claims, for search's app-scoped ranking.
///
/// Must cover every **concrete** kind. The subject-kind registry is a
/// taxonomy: `person`, `object` and `intangible` are abstract roots
/// that nothing is ever an instance of — `account` and `employee`
/// specialize `person` — so they are deliberately unclaimed, and the
/// test beside this exempts roots-with-children on that basis rather
/// than by name.
///
/// Everything else must land somewhere or search silently never
/// floats it for the app whose surface shows it. That is what happened
/// to `message`: Inbox is a Home surface listing 13,483 message
/// Subjects, and no app claimed the kind.
export const APP_SUBJECT_KINDS: Readonly<Partial<Record<AppId, ReadonlyArray<string>>>> = {
  // Inbox lives here, and messages are what it lists.
  home: ['message'],
  simulator: [],
  // `custom` is the escape hatch for Jobs about things that are not
  // domain Subjects — a design doc is the shipped example, and
  // /it/design is an IT surface.
  it: ['workflow', 'company', 'custom'],
  // Was one `crm` bucket. Split along the departments that actually do
  // the work: Sales owns the accounts and the shop, Marketing owns the
  // campaigns and their assets.
  sales: ['account', 'customer'],
  marketing: ['campaign', 'marketing-asset'],
  finance: ['invoice', 'vendor', 'vendor-invoice', 'purchase_order'],
  // Was `supply-chain`, which spanned four departments. A purchase
  // order is Warehouse's to raise and Finance's to pay, and both claim
  // it — this maps attention, not ownership.
  warehouse: ['purchase_order', 'vendor'],
  production: ['product', 'calendar'],
  distribution: ['shipment'],
  maintenance: ['asset', 'location'],
  people: ['employee'],
};
