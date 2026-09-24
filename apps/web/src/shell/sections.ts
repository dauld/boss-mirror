// Which sidebar section highlights for each route kind — and, through
// `appForSection`, which app tab the page renders under.
//
// This was a 60-line ternary inside App.svelte. It moved out for the
// same two reasons the nav catalog itself moved out of AppShell:
//
//  1. Exhaustiveness is now the type system's job. The ternary's
//     fall-through shipped 21 of 74 kinds rendering inside the Home
//     chrome, and the test guarding it had to scrape the Svelte source
//     with regexes. `Record<Route['kind'], string>` makes a missing
//     kind a typecheck failure instead.
//
//  2. The section ids on the right-hand side are ROUTE_CATALOG keys —
//     the same string must exist there for the sidebar row to
//     highlight and for `appForSection` to find the owning app. That
//     agreement was unpinned, and it drifted: 'systemMonitoring' and
//     'systemStepPlugins' (camelCase route kinds used as section ids)
//     miss their kebab-case catalog keys, so /it/operate/audit — the
//     IT tab's own landing page — rendered under Home chrome.
//     `sections.test.ts` pins every value now (CLAUDE.md §9a).

import { notFoundBack, parseRoute, type Route } from '../router';
import { ROUTE_CATALOG, appForSection, type AppId, type NavItem } from './nav-catalog';

/// Sections that deliberately resolve to the Home app instead of a
/// catalog entry, each with the reason. The bar for adding one is
/// "this route renders outside AppShell or has no sidebar row" — not
/// "I could not find where it goes".
export const HOME_CHROME_SECTIONS: ReadonlyMap<string, string> = new Map([
  [
    'me',
    'the personal fallback: login/stepFocus/home render outside AppShell ' +
      'entirely, and search is cross-cutting with no sidebar row',
  ],
  // 'hr' and 'manual' graduated out of this map on 2026-09-02 (CAR-6):
  // People claimed HR and Home claimed the manual, so both are real
  // catalog entries now — exactly the departure their rows here said
  // they were waiting for.
]);

/// Sections whose APP is carried by the route rather than by a catalog
/// entry, each with the reason. The catalog's `app` field is static —
/// one surface, one department — and that is right for every surface
/// built for a department by name. A department's jobs view is one
/// surface for EVERY department the Class registry declares
/// (cc76f755, 2026-09-18): the app it renders under is the route's
/// `code`, so no catalog row can answer for it, and `appForRoute`
/// reads the route instead. The sidebar row that highlights for it is
/// the permKey-less "Jobs" row AppShell adds to every department group.
export const DYNAMIC_APP_SECTIONS: ReadonlyMap<string, string> = new Map([
  [
    'department-jobs',
    'the department jobs view renders under the department named in the ' +
      'route (/ux/departments/<code>), which is registry data',
  ],
]);

/// Which app a route renders under. The catalog answers for every
/// surface with a static owner (`appForSection`); a route whose app is
/// its own data answers for itself. This is the ONE derivation
/// App.svelte's tab highlight reads — before it existed the
/// department view would have highlighted Home, the exact defect the
/// packet was filed on ("lands on All jobs with Home highlighted").
export function appForRoute(route: Route): AppId {
  if (route.kind === 'department') return route.code;
  return appForSection(sectionForRoute(route));
}

/// The two yards that kept rows of their own (feedback 92921c2f,
/// 2026-09-18) are REGIONS of the world since car 4 of design
/// d2154293, so their route is a yard floor like any other. The kind
/// alone can no longer say which row to light: the region does, and
/// this is the one place that reads it. Without it both would light
/// the Train Yard's row, and a sidebar row that never highlights is
/// a row an operator stops trusting. Private for the reason
/// SECTION_FOR_KIND is: `sectionForRoute` is the one reader.
const REGION_SECTIONS: Readonly<Record<string, string>> = {
  receiving: 'system-receiving',
  marshalling: 'system-marshalling',
};

/// Which tenant module a route needs, and the label to say it with —
/// or null when the surface is always-on.
///
/// DERIVED from the catalog entry the route lights, because the module
/// that HIDES a sidebar row and the module that gates the ROUTE behind
/// it are one fact. App.svelte carried the second copy as a
/// hand-written `routeRequiredModule` switch, and it drifted exactly
/// the way §9a says a fact that lives twice does: /ux/support was
/// gated on 'shipping' while its nav row was gated on 'support', so a
/// direct visit answered "Shipments is not enabled" for a page with
/// nothing to do with shipments (backlog f9b43965, found by the
/// /ux/support page audit 9876ef0d, 2026-09-19). The switch also
/// listed no module for finance, warehouse, parts or products, whose
/// nav rows the catalog hides — the same disagreement pointing the
/// other way.
export function moduleForRoute(route: Route): { id: string; label: string } | null {
  const entry = (ROUTE_CATALOG as Record<string, NavItem | undefined>)[sectionForRoute(route)];
  if (!entry?.module) return null;
  return { id: entry.module, label: entry.label };
}

/// Which sidebar row a route lights — the ONE answer, and the only
/// one this module exports. Most rows are named by the route's kind
/// (SECTION_FOR_KIND); a few depend on a route PARAMETER, which a
/// kind-keyed map cannot express (REGION_SECTIONS, a yard floor's
/// region). A route whose row depends on a parameter gets its branch
/// HERE, never a second exported lookup.
export function sectionForRoute(route: Route): string {
  if (route.kind === 'systemYardFloor') {
    return REGION_SECTIONS[route.region] ?? SECTION_FOR_KIND.systemYardFloor!;
  }
  // An unmatched path lights the row of the page its one back link
  // opens, so it renders in the department it was under: /it/<typo> in
  // the IT chrome, anything else in Home (design ee3a3a2f Q4). The /it
  // catch-all returned the yard until then, which is how that chrome
  // came for free.
  if (route.kind === 'notFound' && route.path) {
    return sectionForRoute(parseRoute(notFoundBack(route.path).href));
  }
  return SECTION_FOR_KIND[route.kind];
}

/// The kind half of `sectionForRoute`: NOT authoritative alone, and so
/// not exported. It was the public answer (as SECTION_FOR_ROUTE) until
/// car 4 of design d2154293 retired /it/operate/receiving and
/// /it/operate/marshalling as pages: both are yard floors now, and
/// read off this map both light the Train Yard's row — a plausible
/// wrong answer with no error, which is the failure a reader cannot
/// see (backlog c6f91515, 2026-09-20). Its companion is
/// REGION_SECTIONS; its reader is `sectionForRoute`.
const SECTION_FOR_KIND: Readonly<Record<Route['kind'], string>> = {
  // Renders outside AppShell (or has no sidebar row) — see
  // HOME_CHROME_SECTIONS for the reasons.
  login: 'me',
  stepFocus: 'me',
  home: 'me',
  // The kind half only: `sectionForRoute` answers by the path's
  // department, the way it answers a yard floor by its region.
  notFound: 'me',
  search: 'me',
  me: 'me',
  hr: 'hr',
  manual: 'manual',
  manualSection: 'manual',

  inbox: 'inbox',
  jobs: 'jobs',
  jobDetail: 'jobs',
  service: 'service',
  sales: 'sales',
  assets: 'assets',
  asset: 'assets',
  accounts: 'accounts',
  account: 'accounts',
  watchlist: 'accounts',
  vendors: 'vendors',
  vendor: 'vendors',
  // A purchase order and a vendor invoice are both about a vendor;
  // neither has a sidebar row of its own.
  po: 'vendors',
  vendorInvoice: 'vendors',
  people: 'people',
  employee: 'people',
  parts: 'parts',
  part: 'parts',
  finance: 'finance',
  invoice: 'finance',
  newInvoice: 'finance',
  newJournalEntry: 'finance',
  shipping: 'shipping',
  shipmentDetail: 'shipping',
  support: 'support',
  qa: 'qa',
  // /ux/calendar/me is the `schedule` row's own path (Home, "My
  // schedule"). It lit `calendar` until 2026-09-24, so it highlighted
  // Release calendar and was gated on the calendar module — off on the
  // live instance, so the page was ModuleDisabled (eff0c5e5).
  myCalendar: 'schedule',
  // /ux/service/schedule is the service department's week grid of
  // every tech, not a personal schedule: it lights the Service queue
  // row and takes that row's module gate. It lit `schedule` until
  // 2026-09-24, so with the line above two pages lit My schedule
  // (backlog 3b50fe11).
  schedule: 'service',
  exec: 'exec',
  warehouse: 'warehouse',
  // The department jobs view: its app is the route's own code, not a
  // catalog field — see DYNAMIC_APP_SECTIONS and `appForRoute`.
  department: 'department-jobs',
  catalog: 'catalog',
  device: 'catalog',
  marketingAssets: 'marketing-assets',
  marketingAsset: 'marketing-assets',
  products: 'products',
  product: 'products',
  shop: 'shop',
  shopProduct: 'shop',
  views: 'views',

  // Operate-family kinds highlight the Operate row (2026-08-31
  // consolidation); registry- and design-family kinds highlight
  // theirs the same way, via their own catalog ids whose paths now
  // live under the family surface.
  systemMonitoringPerf: 'system-incidents',
  systemMonitoringEvents: 'system-incidents',
  systemMonitoringAtlas: 'system-incidents',
  systemMonitoringConductor: 'system-incidents',
  systemFleet: 'system-incidents',
  systemYardStatus: 'system-incidents',
  systemStepPlugins: 'system-step-plugins',
  systemStepPluginDetail: 'system-step-plugins',
  systemSubjects: 'system-subjects',
  systemRegistryDrift: 'system-registry-drift',
  systemFeedback: 'system-feedback',
  systemBacklog: 'system-backlog',
  systemYard: 'system-yard',
  // A yard floor is the Train Yard opened on one panel (0524fc95 car
  // 2): it highlights the yard's own row — except for the two floors
  // that are queue boards with sidebar rows of their own, which
  // `sectionForRoute` answers for.
  systemYardFloor: 'system-yard',
  systemCrew: 'system-crew',
  systemEstate: 'system-estate',
  incidents: 'system-incidents',
  systemKb: 'system-kb',
  systemDesign: 'system-design',
  // The codebase has its own row since 2026-09-14 (feedback 9827c699).
  systemCodebase: 'system-codebase',
  experiments: 'system-experiments',
  policy: 'policy',
  authAdmin: 'auth-admin',
  dispatcherRules: 'system-dispatcher',
  dispatcherRulesList: 'system-dispatcher',
  dispatcherRuleEdit: 'system-dispatcher',
  workflows: 'workflows',
  workflowsAdmin: 'workflows',
  workflowNew: 'workflows',
  workflowDesign: 'workflows',
  workflowDetail: 'workflows',
};

/// Every route kind — the map's keys, which the `Record` type holds
/// complete. Exported so a pin can walk every kind through
/// `sectionForRoute` without reaching for the map itself.
export const ROUTE_KINDS: ReadonlyArray<Route['kind']> = Object.keys(
  SECTION_FOR_KIND,
) as ReadonlyArray<Route['kind']>;

/// Every section a route can light: the kind's own and each one a
/// parameter answers for. Derived here, beside both halves, so no
/// caller has to know there are two (backlog c6f91515).
export const SECTIONS_PRODUCED: ReadonlySet<string> = new Set([
  ...Object.values(SECTION_FOR_KIND),
  ...Object.values(REGION_SECTIONS),
]);
