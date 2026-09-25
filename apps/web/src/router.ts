// Tiny router — same shape as apps/web/src/router.ts.
//
// Phase 1: covers the routes the primitive-touching pages hang off
// (jobs/service/sales + assets + asset detail + home).
// Phase 2 expands to every route the React app knows about.
// URLs stay identical so deep-links work across the flip.

import { readFinanceView, type FinanceView } from './finance/financeQuery';

export type Route =
  /// The landing page (the System Model live view), at /ux/system-model.
  | { kind: 'home' }
  /// A path nothing in the app answers — rendered in place, naming the
  /// path as it was asked for (design ee3a3a2f).
  | { kind: 'notFound'; path: string }
  | { kind: 'login' }
  | { kind: 'authAdmin' }
  | { kind: 'me' }
  | {
      kind: 'jobs';
      workflow?: string;
      workflowPrefix?: string;
      jobStatus?: string;
      // #93: filter by Job.owner_id so "View this employee's
      // assigned jobs" links actually filter the list.
      jobOwnerId?: string;
      // #93: filter by Job.subject_id. For "View this account's
      // jobs", "View this vendor's POs", etc. There is no subject-kind
      // half: the jobs API takes none, and the one that was parsed
      // here was never sent (backlog 45ca0f89).
      jobSubjectId?: string;
      // Phase 3 of the create-Job UX work: deep-link from a
      // Subject detail page opens the form pre-filled.
      newJobOpen?: boolean;
      // The new job's Kind — `kind` under `new=1` (backlog 3f5cce16).
      newJobKind?: string;
      newJobSubjectKind?: string;
      newJobSubjectId?: string;
    }
  | { kind: 'jobDetail'; jobId: string }
  /// Full results for a global-search query. Lives in Home because
  /// Home is the cross-app surface — the chrome dropdown is scoped to
  /// the app you are in, this is the unscoped view it escalates to.
  | { kind: 'search'; q: string }
  /// Personal Views — the Home-app surface for composing your own
  /// reads over the information layer.
  | { kind: 'views' }
  /// The codebase trend — the daily metrics packets, rendered.
  | { kind: 'systemCodebase' }
  /// Full-page step surface. A step whose UX is a plugin gets the
  /// whole viewport instead of a panel inside the job page — review
  /// and authoring steps are reading tasks, and reading competes
  /// badly with a sidebar and a step list.
  | {
      kind: 'stepFocus';
      jobId: string;
      stepId: string;
      /// In-app path the lens that opened this step wants Back to
      /// return to, with a label for it. Absent for a deep link.
      from?: string;
      fromLabel?: string;
    }
  | { kind: 'service' }
  | { kind: 'sales' }
  | { kind: 'accounts' }
  | { kind: 'account'; accountId: string }
  | { kind: 'vendors' }
  | { kind: 'vendor'; vendorLookup: string }
  | { kind: 'people' }
  | { kind: 'employee'; empId: string }
  | { kind: 'parts' }
  | { kind: 'part'; partSku: string }
  /// `q` is the list's search box, read back off the query so a
  /// filtered view survives a reload and can be linked (1c2db4c2).
  | { kind: 'products'; q: string }
  | { kind: 'product'; productSku: string }
  /// The tab, and the entry or fact a link opened, read back off the
  /// query so a posted entry is shown and a tab survives back and
  /// reload (2ab44d55).
  | { kind: 'finance'; view: FinanceView }
  | { kind: 'newInvoice' }
  | { kind: 'newJournalEntry' }
  | { kind: 'invoice'; invoiceId: string }
  | { kind: 'vendorInvoice'; vendorInvoiceId: string }
  | { kind: 'shipping' }
  | { kind: 'shipmentDetail'; shipmentId: string }
  | { kind: 'support' }
  | { kind: 'hr' }
  | { kind: 'qa' }
  // 'itSim' retired 2026-05-03 with boss-sim-api (HumanWorker step 9b).
  // 'systemMonitoring' (the index page), 'systemModel', 'systemMap' and
  // 'systemFlow' retired 2026-08-31 with the IT consolidation
  // (1f6d55e0): the monitoring index became the Operate tab strip, and
  // the map/flow renderings folded into Atlas.
  | { kind: 'systemKb' }
  | { kind: 'systemMonitoringPerf' }
  | { kind: 'systemMonitoringEvents' }
  | { kind: 'systemMonitoringAtlas' }
  | { kind: 'policy' }
  | { kind: 'workflows' }
  | { kind: 'workflowsAdmin' }
  | { kind: 'workflowNew' }
  | { kind: 'workflowDesign'; jobId: string }
  | { kind: 'workflowDetail'; kindSlug: string }
  | { kind: 'systemStepPlugins' }
  | { kind: 'systemStepPluginDetail'; pluginSlug: string }
  | { kind: 'systemDesign' }
  /** The Department Map — the IT landing (design e765b3fc, car N1): the
   *  map on top, and below it the detail of `at`, the station the query
   *  selects (`/it?at=gates`). Absent, nothing is selected. */
  | { kind: 'systemYard'; at?: string }
  // 'systemYardFloor' (/it/yard/<region>), 'systemCrew' (/it/crew),
  // 'systemYardStatus', 'systemMonitoringConductor', 'systemFeedback'
  // and 'systemBacklog' retired 2026-09-25 with car N3 of design
  // e765b3fc: each page's content is a selection's panel on the
  // Department Map now (MapPage.svelte says which), and the paths are
  // not found like any other.
  /// Fleet lives on as Operate's Bottlenecks tab (1f6d55e0 Q3: the
  /// per-kind dashboard is unique, not a duplicate rendering).
  | { kind: 'systemFleet' }
  /// The hardware registry, declared beside observed (59ef456a).
  | { kind: 'systemEstate' }
  /// The IT incidents surface — active incident packets +
  /// the closed ones rendered as a durable archive.
  | { kind: 'incidents' }
  | { kind: 'systemSubjects' }
  /// The Drift tab on Registry — the newest maintenance-protocol-drift
  /// packet rendered (4ae9969e, car 2 of 8f4e9cc0).
  | { kind: 'systemRegistryDrift' }
  | { kind: 'experiments' }
  | { kind: 'dispatcherRules' }
  | { kind: 'dispatcherRulesList' }
  | { kind: 'dispatcherRuleEdit'; ruleName: string }
  | { kind: 'inbox' }
  | { kind: 'myCalendar' }
  | { kind: 'schedule' }
  | { kind: 'exec' }
  /// A department's own jobs — its in / working / out over the
  /// packets whose workflow declares it (cc76f755). The code is Class
  /// registry data, so the route carries it; the router knows no
  /// department by name.
  | { kind: 'department'; code: string }
  | { kind: 'warehouse' }
  | { kind: 'catalog' }
  | { kind: 'device'; sku: string }
  | { kind: 'assets' }
  | { kind: 'asset'; assetId: string }
  | { kind: 'marketingAssets' }
  | { kind: 'marketingAsset'; assetId: string }
  | { kind: 'manual' }
  | { kind: 'manualSection'; slug: string }
  | { kind: 'po'; poId: string }
  | { kind: 'watchlist' }
  | { kind: 'shop' }
  | { kind: 'shopProduct'; sku: string };

/// The path as the router matches it: the /dashboard mount and a
/// trailing slash dropped.
function routable(pathname: string): string {
  return pathname.replace(/^\/dashboard/, '').replace(/\/$/, '') || '/';
}

function underIt(raw: string): boolean {
  return raw === '/it' || raw.startsWith('/it/');
}

/// One path segment, decoded — or as it was typed when its escape is
/// malformed, so a mistyped URL renders a page instead of throwing out
/// of the router.
function segment(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

/// The one door an unmatched path offers: back to the department it was
/// under (design ee3a3a2f Q3). No search box and no "did you mean" — a
/// suggestion is a guess, which is what the not-found page refuses.
export function notFoundBack(pathname: string): { href: string; label: string } {
  return underIt(routable(pathname))
    ? { href: '/it', label: 'Back to the Department Map' }
    : { href: '/ux', label: 'Back to My Day' };
}

/// `search` is the query string (`window.location.search` at the SPA's
/// call site, leading `?` and all). It is an ARGUMENT, not a read of
/// `window`: the router read that global until backlog cb211b39, so
/// every caller outside a browser had to plant a window first, and one
/// test file's planted window made another file pass only when bun
/// happened to run it second. Omitted, it is no query — which is what a
/// path with no query means.
export function parseRoute(pathname: string, search = ''): Route {
  const raw = routable(pathname);
  if (raw === '/login') return { kind: 'login' };

  // ===== The IT department — /it/* =====
  //
  // Six surfaces (the 2026-08-31 consolidation, packet 1f6d55e0):
  // the yard IS the landing, Operate / Registry / Design carry their
  // families as tabs, Estate and the KB stand alone. /system is GONE
  // — David's Q1/Q4 verdicts: department-first, and "we don't need to
  // worry about legacy users. It is just me" — which reverses the
  // "kept permanently" promise the old alias comment made (feedback
  // 0fc8b216 got the /it half; this finishes it). A /system path now
  // falls through to the catch-all like any other unknown route.
  if (underIt(raw)) {
    const p = raw.slice('/it'.length) || '/';
    // 1. The landing is the yard — delivery truth first. Since design
    //    0524fc95 (car 2) the landing is the yard's MAP. Since design
    //    e765b3fc (car N1) it is the DEPARTMENT MAP: the map stays on
    //    top and a selection opens its detail below, named in the query
    //    so a link says what it selects. A query, not a path: the map is
    //    not torn down and rebuilt by a selection. Car N3 retired the
    //    floor pages at /it/yard[/<region>] and every page whose content
    //    moved into a selection's panel — /it/crew, yard status, the
    //    conductor's feed, the feedback and backlog boards — and with
    //    them the four aliases that still answered (/it/yard,
    //    /it/operate/receiving, /it/operate/marshalling,
    //    /it/design/codebase). No alias and no redirect (David,
    //    2026-09-25: no shims before 1.0.0): each falls to not-found
    //    below, whose one door is the Department Map.
    if (p === '/') {
      const r: Route = { kind: 'systemYard' };
      const at = new URLSearchParams(search).get('at');
      if (at) (r as { at?: string }).at = at;
      return r;
    }
    // 2. Operate — incidents lead; audit/perf/atlas/bottlenecks tabs.
    if (p === '/operate') return { kind: 'incidents' };
    if (p === '/operate/audit') return { kind: 'systemMonitoringEvents' };
    if (p === '/operate/perf') return { kind: 'systemMonitoringPerf' };
    if (p === '/operate/atlas') return { kind: 'systemMonitoringAtlas' };
    if (p === '/operate/bottlenecks') return { kind: 'systemFleet' };
    // 3. Registry — one surface over the registry family.
    if (p === '/registry') return { kind: 'workflows' };
    if (p === '/registry/new') return { kind: 'workflowNew' };
    if (p === '/registry/authoring') return { kind: 'workflowsAdmin' };
    const jkDesignM = p.match(/^\/registry\/authoring\/([^/]+)$/);
    if (jkDesignM) return { kind: 'workflowDesign', jobId: segment(jkDesignM[1]!) };
    if (p === '/registry/step-plugins') return { kind: 'systemStepPlugins' };
    const spM = p.match(/^\/registry\/step-plugins\/([^/]+)$/);
    if (spM) return { kind: 'systemStepPluginDetail', pluginSlug: segment(spM[1]!) };
    if (p === '/registry/rules') return { kind: 'dispatcherRulesList' };
    const drM = p.match(/^\/registry\/rules\/([^/]+)$/);
    if (drM) return { kind: 'dispatcherRuleEdit', ruleName: segment(drM[1]!) };
    if (p === '/registry/dispatcher') return { kind: 'dispatcherRules' };
    if (p === '/registry/policy') return { kind: 'policy' };
    if (p === '/registry/subjects') return { kind: 'systemSubjects' };
    if (p === '/registry/drift') return { kind: 'systemRegistryDrift' };
    // 4. Design — reviews lead; the experiments tab.
    if (p === '/design') return { kind: 'systemDesign' };
    if (p === '/design/experiments') return { kind: 'experiments' };
    // /it/codebase is the row (9827c699).
    if (p === '/codebase') return { kind: 'systemCodebase' };
    // 5. Estate. 6. KB. Plus the unlisted auth door.
    if (p === '/estate') return { kind: 'systemEstate' };
    if (p === '/kb') return { kind: 'systemKb' };
    if (p === '/auth-admin') return { kind: 'authAdmin' };
    // Workflow detail LAST — its wildcard would eclipse the
    // specific /registry/* cases above.
    const jkM = p.match(/^\/registry\/([^/]+)$/);
    if (jkM) return { kind: 'workflowDetail', kindSlug: segment(jkM[1]!) };
    // Unknown /it path: it says so, inside the IT chrome. It returned the
    // yard until design ee3a3a2f (Q4), so a mistyped IT link landed on the
    // map and looked like a working one.
    return { kind: 'notFound', path: pathname };
  }

  // ===== User Experiences perspective — /ux/* (canonical); bare / is the public alias for the UX home.
  // Unprefixed legacy paths still resolve here (defensive). =====
  const p = raw === '/' || raw === '/ux' ? '/' : raw.startsWith('/ux/') ? raw.slice('/ux'.length) : raw;
  // User Experiences lands on My Day by default — the actor's personal
  // work view, not a marketing landing.
  //
  // EVERY SINGLE-ID WILDCARD BELOW TAKES ONE SEGMENT, `([^/]+)`. They
  // were greedy `(.+)` until design ee3a3a2f (Q6), and a deeper path
  // under a list became a convincing "missing X": /ux/accounts/
  // agreements/<id> rendered the ACCOUNT page for accountId
  // "agreements/<id>". An id holding a slash arrives percent-encoded
  // (entityHref encodes every id) and is decoded by `segment`. The one
  // exception is /manual, whose slug may hold a slash by design — the
  // content API routes it as `{*slug}`.
  if (p === '/') return { kind: 'me' };
  if (p === '/me') return { kind: 'me' };
  if (p === '/inbox') return { kind: 'inbox' };
  if (p === '/views') return { kind: 'views' };
  // The landing page's own door. The catch-all was its ONLY way in until
  // design ee3a3a2f (Q5) — `/` and `/ux` are My Day — so turning the
  // catch-all into a not-found would have orphaned a real page.
  if (p === '/system-model') return { kind: 'home' };
  if (p === '/accounts') return { kind: 'accounts' };
  const cm = p.match(/^\/accounts\/([^/]+)$/);
  if (cm) return { kind: 'account', accountId: segment(cm[1]!) };

  if (p === '/vendors') return { kind: 'vendors' };
  const vm = p.match(/^\/vendors\/([^/]+)$/);
  if (vm) return { kind: 'vendor', vendorLookup: segment(vm[1]!) };

  if (p === '/people') return { kind: 'people' };
  const em = p.match(/^\/people\/([^/]+)$/);
  if (em) return { kind: 'employee', empId: segment(em[1]!) };

  if (p === '/parts') return { kind: 'parts' };
  const partM = p.match(/^\/parts\/([^/]+)$/);
  if (partM) return { kind: 'part', partSku: segment(partM[1]!) };

  if (p === '/products') {
    return { kind: 'products', q: new URLSearchParams(search).get('q') ?? '' };
  }
  const prodM = p.match(/^\/products\/([^/]+)$/);
  if (prodM) return { kind: 'product', productSku: segment(prodM[1]!) };

  if (p === '/finance') return { kind: 'finance', view: readFinanceView(search) };
  if (p === '/finance/new') return { kind: 'newInvoice' };
  if (p === '/finance/journal-entries/new') return { kind: 'newJournalEntry' };
  // Wildcard MUST come after every specific `/finance/X` case above —
  // it matches any one-segment tail and would otherwise eclipse /new.
  const invM = p.match(/^\/finance\/([^/]+)$/);
  if (invM) return { kind: 'invoice', invoiceId: segment(invM[1]!) };

  if (p === '/shipping') return { kind: 'shipping' };
  const shipM = p.match(/^\/shipments\/([^/]+)$/);
  if (shipM) return { kind: 'shipmentDetail', shipmentId: segment(shipM[1]!) };

  if (p === '/support') return { kind: 'support' };

  if (p === '/calendar/me') return { kind: 'myCalendar' };
  if (p === '/service/schedule') return { kind: 'schedule' };
  if (p === '/exec') return { kind: 'exec' };
  const deptM = p.match(/^\/departments\/([^/]+)$/);
  if (deptM) return { kind: 'department', code: segment(deptM[1]!) };
  if (p === '/warehouse') return { kind: 'warehouse' };
  if (p === '/catalog') return { kind: 'catalog' };
  const catM = p.match(/^\/catalog\/([^/]+)$/);
  if (catM) return { kind: 'device', sku: segment(catM[1]!) };
  if (p === '/assets') return { kind: 'assets' };
  const assetM = p.match(/^\/assets\/([^/]+)$/);
  if (assetM) return { kind: 'asset', assetId: segment(assetM[1]!) };
  if (p === '/marketing-assets') return { kind: 'marketingAssets' };
  const mktM = p.match(/^\/marketing-assets\/([^/]+)$/);
  if (mktM) return { kind: 'marketingAsset', assetId: segment(mktM[1]!) };
  if (p === '/manual') return { kind: 'manual' };
  // `(.+)` on purpose: a manual slug may hold a slash (see above).
  const mManual = p.match(/^\/manual\/(.+)$/);
  if (mManual) return { kind: 'manualSection', slug: segment(mManual[1]!) };
  const poM = p.match(/^\/purchase-orders\/([^/]+)$/);
  if (poM) return { kind: 'po', poId: segment(poM[1]!) };
  const viM = p.match(/^\/vendor-invoices\/([^/]+)$/);
  if (viM) return { kind: 'vendorInvoice', vendorInvoiceId: segment(viM[1]!) };
  if (p === '/watchlist') return { kind: 'watchlist' };
  if (p === '/shop') return { kind: 'shop' };
  const shopM = p.match(/^\/shop\/([^/]+)$/);
  if (shopM) return { kind: 'shopProduct', sku: segment(shopM[1]!) };

  if (p === '/search') {
    const sp = new URLSearchParams(search);
    return { kind: 'search', q: sp.get('q') ?? '' };
  }

  if (p === '/hr') return { kind: 'hr' };
  if (p === '/qa') return { kind: 'qa' };

  if (p === '/service') return { kind: 'service' };
  const tm = p.match(/^\/service\/([^/]+)$/);
  if (tm) return { kind: 'jobDetail', jobId: segment(tm[1]!) };

  if (p === '/sales') return { kind: 'sales' };
  const sm = p.match(/^\/sales\/([^/]+)$/);
  if (sm) return { kind: 'jobDetail', jobId: segment(sm[1]!) };

  if (p === '/jobs') {
    const sp = new URLSearchParams(search);
    const jk = sp.get('kind');
    const jkp = sp.get('kind_prefix');
    const js = sp.get('status');
    const newJob = sp.get('new');
    const sk = sp.get('subject_kind');
    const sid = sp.get('subject_id');
    // #93: read list-filter params (separate from new-job params).
    // owner_id filters by Job.owner_id; subject_id filters by
    // Job.subject_id.
    const ownerId = sp.get('owner_id');
    const filterSubjectId = sp.get('subject_id');
    const r: Route = { kind: 'jobs' };
    // Under `new=1` the kind is the new job's Kind, as the subject_id
    // below is its subject: HrPage's link names its workflow there, and
    // read as the list's filter it narrowed the list behind the form
    // and left the form's own Kind unpicked (backlog 3f5cce16).
    if (jk && newJob !== '1') (r as { workflow?: string }).workflow = jk;
    if (jkp) (r as { workflowPrefix?: string }).workflowPrefix = jkp;
    // `!== null`, not truthiness: an EMPTY status is a deep link asking
    // for every status (EmployeePage's owned-jobs link sends
    // `status=`), and only an ABSENT one takes the page's open default.
    // The truthiness check dropped the empty value (backlog 03e198e5).
    if (js !== null) (r as { jobStatus?: string }).jobStatus = js;
    if (ownerId) (r as { jobOwnerId?: string }).jobOwnerId = ownerId;
    // Under `new=1` the subject_id is the new job's subject and not a
    // filter: it narrowed the list behind the form too, and Cancel left
    // it narrowed under a URL that no longer said so (backlog d0b93b80).
    // filterQuery's write reads the parameter the same way.
    if (filterSubjectId && newJob !== '1') (r as { jobSubjectId?: string }).jobSubjectId = filterSubjectId;
    // The new-job half exists only under `new=1`; without it there is
    // no form to seed, and the same parameters are the list's filters.
    if (newJob === '1') {
      (r as { newJobOpen?: boolean }).newJobOpen = true;
      if (jk) (r as { newJobKind?: string }).newJobKind = jk;
      if (sk) (r as { newJobSubjectKind?: string }).newJobSubjectKind = sk;
      if (sid) (r as { newJobSubjectId?: string }).newJobSubjectId = sid;
    }
    return r;
  }
  // Before /jobs/([^/]+) below — which, while it was the greedy
  // /jobs/(.+), swallowed the whole `{id}/steps/{stepId}` tail as a job id.
  const sfm = p.match(/^\/jobs\/([^/]+)\/steps\/([^/]+)$/);
  if (sfm) {
    const sp = new URLSearchParams(search);
    const r: Route = { kind: 'stepFocus', jobId: sfm[1]!, stepId: sfm[2]! };
    // Where "back" goes, and what to call it. Only the lens that sent
    // the operator here knows — the step surface cannot infer it, and
    // guessing from the Job's kind would put a per-workflow branch in
    // core routing (CLAUDE.md 9). Leading-slash check keeps this an
    // in-app path: a `from` naming another origin would turn a Back
    // button into an open redirect.
    const from = sp.get('from');
    const fromLabel = sp.get('from_label');
    if (from?.startsWith('/') && !from.startsWith('//')) {
      (r as { from?: string }).from = from;
      if (fromLabel) (r as { fromLabel?: string }).fromLabel = fromLabel;
    }
    return r;
  }

  const jm = p.match(/^\/jobs\/([^/]+)$/);
  if (jm) return { kind: 'jobDetail', jobId: segment(jm[1]!) };

  // Nothing answers this path, and the page says so, naming it as it was
  // asked for (design ee3a3a2f). This returned the landing page until
  // then, so a dead link rendered a real, working, plausible page and
  // the reader concluded they had misremembered (backlog c4f2ae24).
  // Rendered in place, never redirected: a redirect replaces the
  // evidence in the address bar with a working page, which is the defect.
  return { kind: 'notFound', path: pathname };
}

// `href` (honors the /dashboard mount) + `navigate` (pushState SPA nav)
// now live in the shared @boss/web-kit/nav module. Re-exported here so
// the ~55 files importing them from '../router' need no change.
export { href, navigate } from '@boss/web-kit/nav';
