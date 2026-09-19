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

import type { Route } from '../router';
import { appForSection, type AppId } from './nav-catalog';

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
  return appForSection(SECTION_FOR_ROUTE[route.kind]);
}

export const SECTION_FOR_ROUTE: Readonly<Record<Route['kind'], string>> = {
  // Renders outside AppShell (or has no sidebar row) — see
  // HOME_CHROME_SECTIONS for the reasons.
  login: 'me',
  stepFocus: 'me',
  home: 'me',
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
  calendar: 'calendar',
  myCalendar: 'calendar',
  schedule: 'schedule',
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
  // The two yards have rows of their own since feedback 92921c2f
  // (2026-09-18); they highlight those, not Operate, though the
  // Operate tab strip still lists them.
  systemMarshallingYard: 'system-marshalling',
  systemReceivingYard: 'system-receiving',
  systemYardStatus: 'system-incidents',
  systemStepPlugins: 'system-step-plugins',
  systemStepPluginDetail: 'system-step-plugins',
  systemSubjects: 'system-subjects',
  systemRegistryDrift: 'system-registry-drift',
  systemFeedback: 'system-feedback',
  systemBacklog: 'system-backlog',
  systemYard: 'system-yard',
  // A yard floor is the Train Yard opened on one panel (0524fc95 car
  // 2): it highlights the yard's own row.
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
