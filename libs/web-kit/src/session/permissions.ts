// Route visibility — which top-level sections a role's sidebar shows.
//
// This is SPA visibility, not authority: what a surface can READ is
// policed by the endpoints behind it (boss-policy, row-level), and
// the policy-scope overlay from the React app is still phase 2.
//
// WHO SAYS. A role is a Class (subject_kind=employee,
// member_attribute=role), tenant reference data the SPA reads at boot
// via `classesFor('employee', 'role')`. The role's Class row is where a
// tenant narrows that role's sidebar: `metadata.surfaces = [...]`, a
// list of the RouteNames below. A role whose row declares none — or
// a role the registry has not answered for yet — sees every surface
// the tenant's modules turn on (a module is on only when the manifest
// lists it true, ce68f137), which is the tenant's own statement of
// what exists.
//
// Until 2026-09-17 (backlog 18d6a6c9) this file carried ROUTE_ACCESS:
// a hand-curated matrix of 33 brewery roles and 26 device-shop roles
// and what each could see. Measured on prod, the company's own roles
// were in it nowhere, so their sidebars collapsed to the six ungated
// routes — one tenant's org chart, compiled into every deployment.
// The brewery's entries moved VERBATIM to its classes.json as
// `metadata.surfaces`, so the playground renders as before, from the
// tenant's data; a role that wants a narrower sidebar declares it on
// its own row.

// A role code — an open string, the Class registry's `code`.
export type Role = string;

export type RouteName =
  | 'shop' | 'exec' | 'catalog' | 'accounts' | 'assets' | 'sales' | 'service'
  | 'parts' | 'products' | 'finance' | 'people' | 'qa' | 'warehouse' | 'support'
  | 'system-monitoring' | 'inbox' | 'shipping' | 'views'
  | 'vendors' | 'marketing-assets' | 'schedule' | 'jobs'
  // Platform-administration surfaces. Same `permKey: 'it'` gate
  // as the legacy ADMIN footer; these route names exist so a role's
  // Class row can name the surfaces in `metadata.surfaces` per the
  // three-axis IA simplifier ("administering is someone's job").
  | 'policy' | 'workflows' | 'system-step-plugins' | 'system-dispatcher' | 'system-design'
  // The executor network — who moves work and where it goes.
  // Sits beside the dispatcher cascade: same IT audience, different
  // question (job traffic, not rule wiring).
  | 'system-yard'
  // system-map / system-flow / system-fleet / system-model retired by
  // the 2026-08-31 IT consolidation (packet 1f6d55e0): map and flow's
  // renderings folded into the Atlas tab, fleet lives on as the
  // Bottlenecks tab under Operate (gated by system-monitoring), and
  // the System Model wrapper page died with the /system prefix.
  // The hardware registry page (59ef456a).
  | 'system-estate'
  // The model-vocabulary surface — SubjectKind taxonomy + Class registry
  // (read-only). Same `it-*` audience as the dispatcher cascade it sits beside.
  | 'system-subjects'
  // Dispatcher rule-authoring surfaces — same `it-dispatcher` audience as
  // the cascade viz they hang off (reached via links from it, not their
  // own sidebar entries).
  | 'system-dispatcher-rules' | 'system-dispatcher-rule'
  | 'system-kb' | 'auth-admin'
  // The "Evolve" surface — controlled, sandboxed modifications to the
  // running model (placeholder for now). Visible to every role.
  | 'system-experiments'
  | 'workflows';

/// Every gated RouteName, once — the vocabulary a Class row's
/// `metadata.surfaces` is written in (the always-on routes below are
/// not in it; they need no declaring).
export const ROUTES: ReadonlyArray<RouteName> = [
  'shop', 'exec', 'catalog', 'accounts', 'assets', 'sales', 'service',
  'parts', 'products', 'finance', 'people', 'qa', 'warehouse', 'support', 'system-monitoring',
  'shipping', 'vendors', 'marketing-assets',
  'schedule', 'jobs',
  'policy', 'workflows', 'system-step-plugins', 'system-dispatcher',
  'system-dispatcher-rules', 'system-dispatcher-rule', 'system-design', 'system-yard', 'system-estate', 'system-subjects', 'system-kb', 'auth-admin',
  'system-experiments',
];

/// The shape of a role's Class row this reader needs; the full row is
/// the classes client's (`session/classes.svelte.ts`).
export type RoleRow = Readonly<{
  code: string;
  metadata: Readonly<Record<string, unknown>>;
}>;

/// The surfaces a role's Class row declares (`metadata.surfaces`), or
/// `undefined` when it declares none — an absent row, an absent key, or
/// a value that is not a list all read as "nothing declared", never as
/// a crash and never as an empty list (an empty list IS a declaration:
/// the always-on routes alone).
export function declaredSurfaces(row: RoleRow | undefined): ReadonlyArray<string> | undefined {
  const v = row?.metadata?.['surfaces'];
  if (!Array.isArray(v)) return undefined;
  return v.filter((s): s is string => typeof s === 'string');
}

/// Whether `role` sees `route`. `row` is the role's Class row from the
/// registry (`classesFor('employee', 'role')`), or undefined before it
/// has loaded.
export function canSeeRoute(role: Role, route: RouteName, row?: RoleRow): boolean {
  void role;
  if (route === 'shop' || route === 'inbox' || route === 'workflows' || route === 'system-experiments') return true;
  // Views are personal by construction — everyone has their own, so
  // there is nothing to gate. What a View can READ is still policed
  // by the endpoints it reads through.
  if (route === 'views') return true;
  // 'system-feedback' was an always-on route here until car N3 of
  // design e765b3fc (2026-09-25): the feedback board is the receiving
  // station's panel on the Department Map now, which `system-yard` gates.
  const declared = declaredSurfaces(row);
  return declared ? declared.includes(route) : true;
}
