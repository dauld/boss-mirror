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
import type { AppId, AppTab } from '@boss/web-kit/nav';
import { DEPARTMENTS, HOME_APP, SIMULATOR_APP } from '@boss/web-kit/nav';

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
  | 'system-fleet'
  | 'system-backlog'
  | 'hr'
  | 'watchlist'
  | 'manual';

export const ROUTE_CATALOG: Readonly<Record<RouteName | UngatedSurfaceId, NavItem>> = {
  jobs:      { id: 'jobs',      label: 'All jobs',         path: '/ux/jobs',      permKey: 'jobs',      app: 'home' },
  sales:     { id: 'sales',     label: 'Sales pipeline',   path: '/ux/sales',     permKey: 'sales',     app: 'sales' },
  service:   { id: 'service',   label: 'Service queue',    path: '/ux/service',   permKey: 'service',   module: 'support', app: 'service' },
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
  shop:      { id: 'shop',      label: 'Shop',             path: '/ux/shop',      permKey: 'shop',      app: 'sales' },
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
  // (departure-board.md Q1): the yard, now AT /it itself.
  'system-yard':              { id: 'system-yard',              label: 'Train Yard',          path: '/it',              permKey: 'system-yard',             app: 'it' },
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
  'system-dispatcher-rules': { id: 'system-dispatcher-rules', label: 'Dispatcher rules — authoring', path: '/it/registry/rules', permKey: 'system-dispatcher-rules', app: 'it' },
  'system-dispatcher-rule':  { id: 'system-dispatcher-rule',  label: 'Dispatcher rule — editor',     path: '/it/registry/rules', permKey: 'system-dispatcher-rule',  app: 'it' },
  'system-design':           { id: 'system-design',           label: 'Design',              path: '/it/design',       permKey: 'system-design',           app: 'it' },
  'system-experiments':      { id: 'system-experiments',      label: 'Experiments',         path: '/it/design/experiments', permKey: 'system-experiments', app: 'it' },
  'system-feedback':         { id: 'system-feedback',         label: 'Feedback triage',     path: '/it/design/feedback', permKey: 'system-feedback',      app: 'it' },
  'system-backlog':          { id: 'system-backlog',          label: 'IT backlog',          path: '/it/design/backlog', permKey: 'system-feedback',      app: 'it' },
  // The hardware registry — declared beside observed beside the
  // difference, plus the dev-workspace door (59ef456a).
  'system-estate':           { id: 'system-estate',           label: 'Estate',              path: '/it/estate',       permKey: 'system-estate',           app: 'it' },
  'system-kb':               { id: 'system-kb',               label: 'Knowledge Base',      path: '/it/kb',           permKey: 'system-kb',               app: 'it' },
  // Unlisted door: reachable, never a sidebar row.
  'auth-admin':              { id: 'auth-admin',              label: 'Auth admin',          path: '/it/auth-admin',   permKey: 'auth-admin',              app: 'it' },
};

/// The apps this host offers: Home, Simulator, and one per department
/// that actually owns a surface.
///
/// DERIVED, not listed. The previous version was a hand-maintained
/// `DEPARTMENT_APP` map from each department to one of eight invented
/// apps, and its own comment predicted this change — "the app list
/// probably wants DERIVING from the Class registry rather than
/// hand-listing". It does, and now it is: an app exists because a
/// department owns a surface, so adding a surface with a new `app`
/// creates the tab and nothing here changes (CLAUDE.md §9).
///
/// Departments with NO surface get no tab, deliberately. Algedonic
/// Ales has packaging, taproom and audit employees and not one screen
/// built for them yet; a tab opening an empty sidebar would claim
/// otherwise. `departmentsWithoutSurfaces()` reports them so the gap
/// stays visible instead of silently reading as covered.
const OWNED = new Set<string>(
  Object.values(ROUTE_CATALOG)
    .map((e) => e.app)
    .filter((a): a is AppId => a !== undefined && a !== 'home' && a !== 'simulator'),
);

/// Departments that own at least one surface, in registry order.
export const DEPARTMENT_APPS: ReadonlyArray<AppTab> = DEPARTMENTS.filter((d) =>
  OWNED.has(d.code),
).map((d) => ({
  id: d.code,
  label: d.label,
  // The department's landing page is its first surface in catalog
  // order — the same order the sidebar lists them in, so the tab opens
  // on the row the sidebar shows first rather than an arbitrary pick.
  href:
    Object.values(ROUTE_CATALOG).find((e) => e.app === d.code)?.path ?? '/',
}));

/// The full tab list, left to right.
export const APPS: ReadonlyArray<AppTab> = [
  HOME_APP,
  SIMULATOR_APP,
  ...DEPARTMENT_APPS,
];

/// Departments with no surface of their own. Not an error — a report.
export function departmentsWithoutSurfaces(): ReadonlyArray<string> {
  return DEPARTMENTS.filter((d) => !OWNED.has(d.code)).map((d) => d.code);
}

/// Which app an employee of `department` lands in.
///
/// Identity now that apps are departments, except that a department
/// with no surface falls back to Home rather than a tab that does not
/// exist. That fallback is the honest one: Home is personal work
/// whichever department it belongs to.
export function appForDepartment(department: string): AppId {
  return OWNED.has(department) ? (department as AppId) : 'home';
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
