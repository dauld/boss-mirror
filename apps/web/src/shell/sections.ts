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
  systemFleet: 'system-incidents',
  systemStepPlugins: 'system-step-plugins',
  systemStepPluginDetail: 'system-step-plugins',
  systemSubjects: 'system-subjects',
  systemFeedback: 'system-feedback',
  systemBacklog: 'system-backlog',
  systemYard: 'system-yard',
  systemEstate: 'system-estate',
  incidents: 'system-incidents',
  systemKb: 'system-kb',
  systemDesign: 'system-design',
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
