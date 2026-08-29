// Default Work-menu entries per role.
//
// "Work" is the personal half of the sidebar — the 3-5 surfaces the
// viewer's role spends most time in. A sales-rep's Work is their
// pipeline + their accounts; a service-tech's is their queue + their
// calendar; a CEO's is the exec dashboard + the all-jobs board.
//
// A role's Work list intersects the manifest (so a brewery sales-rep
// doesn't get a /shipping entry the tenant has turned off) and the
// route-access matrix from `permissions.ts` (so audit-readonly is
// never offered admin links).

import type { RouteName, Role } from './permissions';

/// Each role's default Work-surface list, ordered top-to-bottom in
/// the sidebar. Roles not listed fall back to [DEFAULT_WORK].
export const WORK_BY_ROLE: Record<Role, ReadonlyArray<RouteName>> = {
  // ----- Executive -----
  ceo:  ['jobs', 'exec', 'sales'],
  coo:  ['jobs', 'exec'],
  cto:  ['jobs', 'system-monitoring'],
  cfo:  ['jobs', 'finance', 'exec'],

  // ----- Sales -----
  'vp-sales':   ['sales', 'accounts', 'jobs'],
  'sales-mgr':  ['sales', 'accounts', 'jobs'],
  'sales-rep':  ['sales', 'accounts'],

  // ----- Service & refurb -----
  'service-mgr':       ['jobs', 'service', 'support'],
  'service-tech':      ['service', 'jobs', 'schedule'],
  // The refurb ROLES remain — they are real people on the device-shop
  // roster — but their surface does not. /ux/refurb was a device-shop
  // page in the shared shell that every other tenant saw as an empty
  // list, removed 2026-08-28 (feedback 96c37dbe). They land on jobs and
  // qa, which is where their work actually is.
  'refurb-supervisor': ['jobs', 'qa'],
  'refurb-tech':       ['jobs'],

  // ----- QA -----
  'qa-lead': ['qa', 'jobs'],
  'qa-tech': ['qa', 'jobs'],

  // ----- Warehouse / parts -----
  'warehouse-mgr':   ['warehouse', 'parts', 'shipping'],
  'warehouse-clerk': ['warehouse', 'shipping'],
  'parts-buyer':     ['parts', 'warehouse'],

  // ----- Finance -----
  controller:     ['finance', 'jobs'],
  'ap-specialist': ['finance', 'jobs'],

  // ----- HR -----
  'hr-generalist': ['people', 'jobs'],
  recruiter:       ['people', 'jobs'],

  // ----- Support / IT -----
  'support-specialist': ['support', 'jobs'],
  'it-manager':         ['policy', 'workflows', 'system-step-plugins', 'system-monitoring', 'jobs'],

  // ----- Audit -----
  auditor:          ['finance', 'jobs'],
  'audit-readonly': ['jobs', 'finance'],

  // ----- Owner / fixture -----
  owner:          ['jobs', 'exec'],
  'smoke-tester': ['jobs'],

  // ----- Brewery: production (cellar + brewhouse) -----
  // Brewers work the morning-brew, ingredient-restock, and
  // seasonal-release Workflows. Their day is steps + the
  // ingredient inventory.
  'head-brewer':   ['jobs', 'parts', 'qa'],
  'senior-brewer': ['jobs', 'parts', 'schedule'],
  brewer:          ['jobs', 'parts'],
  'cellar-tech':   ['jobs', 'parts'],
  'shift-lead':    ['jobs', 'people', 'schedule'],

  // ----- Brewery: packaging (kegs, bottles, cans) -----
  'packaging-mgr':  ['jobs', 'warehouse', 'shipping'],
  'packaging-tech': ['jobs', 'warehouse'],
  palletizer:       ['warehouse', 'jobs'],

  // ----- Brewery: QA / lab -----
  'qa-supervisor': ['qa', 'jobs', 'parts'],
  'lab-tech':      ['qa', 'jobs'],

  // ----- Brewery: warehouse + shipping -----
  'forklift-operator': ['warehouse', 'shipping'],
  'inventory-clerk':   ['warehouse', 'parts'],
  'shipping-clerk':    ['shipping', 'warehouse'],

  // ----- Brewery: distribution (drivers) -----
  'distribution-driver': ['shipping', 'schedule'],

  // ----- Brewery: maintenance -----
  // Equipment-preventive maintenance Workflow drives most of these tickets.
  'maintenance-mgr': ['jobs', 'parts', 'schedule'],
  electrician:       ['jobs', 'parts'],
  mechanic:          ['jobs', 'parts'],

  // ----- Brewery: sales -----
  'account-manager': ['sales', 'accounts', 'jobs'],

  // ----- Brewery: marketing -----
  'brand-manager':        ['marketing-assets', 'calendar', 'jobs'],
  'events-coord':         ['calendar', 'jobs'],
  'social-media-coord':   ['marketing-assets', 'calendar'],
  'marketing-mgr':        ['marketing-assets', 'calendar', 'jobs'],
  'marketing-specialist': ['marketing-assets', 'jobs'],
  'content-writer':       ['marketing-assets'],
  'brand-designer':       ['marketing-assets'],

  // ----- Brewery: taproom -----
  bartender:        ['calendar', 'schedule'],
  'taproom-server': ['calendar', 'schedule'],

  // ----- Brewery: finance -----
  bookkeeper:    ['finance', 'accounts', 'jobs'],
  'ar-clerk':    ['finance', 'accounts'],
  'ap-clerk':    ['finance', 'vendors'],
  'fp-analyst':  ['finance', 'exec'],
  'payroll-mgr': ['finance', 'people'],

  // ----- Brewery: people (HR) -----
  'benefits-coord': ['people', 'jobs'],

  // ----- Brewery: IT -----
  // IT roles' Work IS platform admin per the three-axis IA
  // simplifier — policy edits, Workflow authoring, step-plugin
  // publishes are all administrative work that lives here, not
  // in a separate /admin tier.
  'it-director': ['policy', 'workflows', 'system-step-plugins', 'system-monitoring', 'exec'],
  sysadmin:      ['policy', 'workflows', 'system-step-plugins', 'system-monitoring'],
  helpdesk:      ['system-monitoring', 'jobs'],

  // ----- Brewery: heads of department -----
  // Each head's Work is "their dept dashboard + the all-jobs view"
  // — same shape as the platform's vp-sales / service-mgr entries.
  'head-of-distribution': ['exec', 'shipping', 'jobs'],
  'head-of-marketing':    ['exec', 'marketing-assets', 'jobs'],
  'head-of-people':       ['exec', 'people', 'jobs'],
  'head-of-sales':        ['exec', 'sales', 'accounts'],
};

/// Fallback when a role isn't in the map (a freshly-added class
/// registry entry the SPA hasn't bundled for, or a custom tenant
/// role). Keeps the sidebar useful even on cold-cache.
export const DEFAULT_WORK: ReadonlyArray<RouteName> = ['jobs'];

export function workForRole(role: Role | undefined | null): ReadonlyArray<RouteName> {
  if (!role) return DEFAULT_WORK;
  return WORK_BY_ROLE[role] ?? DEFAULT_WORK;
}
