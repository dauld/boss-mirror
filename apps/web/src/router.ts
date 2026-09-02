// Tiny router — same shape as apps/web/src/router.ts.
//
// Phase 1: covers the routes the primitive-touching pages hang off
// (jobs/service/sales + assets + asset detail + home).
// Phase 2 expands to every route the React app knows about.
// URLs stay identical so deep-links work across the flip.

export type Route =
  | { kind: 'home' }
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
      // #93: filter by Job.subject_id (with optional
      // subject_kind disambiguator). For "View this account's
      // jobs", "View this vendor's POs", etc.
      jobSubjectKind?: string;
      jobSubjectId?: string;
      // Phase 3 of the create-Job UX work: deep-link from a
      // Subject detail page opens the form pre-filled.
      newJobOpen?: boolean;
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
  /// IT feedback triage board.
  | { kind: 'systemFeedback' }
  | { kind: 'systemBacklog' }
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
  | { kind: 'products' }
  | { kind: 'product'; productSku: string }
  | { kind: 'finance' }
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
  | { kind: 'systemYard' }
  /// Fleet lives on as Operate's Bottlenecks tab (1f6d55e0 Q3: the
  /// per-kind dashboard is unique, not a duplicate rendering).
  | { kind: 'systemFleet' }
  /// The hardware registry, declared beside observed (59ef456a).
  | { kind: 'systemEstate' }
  /// The IT incidents surface — active incident-post-mortem packets +
  /// the closed ones rendered as a durable archive.
  | { kind: 'incidents' }
  | { kind: 'systemSubjects' }
  | { kind: 'experiments' }
  | { kind: 'dispatcherRules' }
  | { kind: 'dispatcherRulesList' }
  | { kind: 'dispatcherRuleEdit'; ruleName: string }
  | { kind: 'inbox' }
  | { kind: 'calendar' }
  | { kind: 'myCalendar' }
  | { kind: 'schedule' }
  | { kind: 'exec' }
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

export function parseRoute(pathname: string): Route {
  let raw = pathname.replace(/^\/dashboard/, '').replace(/\/$/, '') || '/';
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
  if (raw === '/it' || raw.startsWith('/it/')) {
    const p = raw.slice('/it'.length) || '/';
    // 1. The landing is the yard — delivery truth first.
    if (p === '/') return { kind: 'systemYard' };
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
    const jkDesignM = p.match(/^\/registry\/authoring\/(.+)$/);
    if (jkDesignM) return { kind: 'workflowDesign', jobId: decodeURIComponent(jkDesignM[1]!) };
    if (p === '/registry/step-plugins') return { kind: 'systemStepPlugins' };
    const spM = p.match(/^\/registry\/step-plugins\/(.+)$/);
    if (spM) return { kind: 'systemStepPluginDetail', pluginSlug: decodeURIComponent(spM[1]!) };
    if (p === '/registry/rules') return { kind: 'dispatcherRulesList' };
    const drM = p.match(/^\/registry\/rules\/(.+)$/);
    if (drM) return { kind: 'dispatcherRuleEdit', ruleName: decodeURIComponent(drM[1]!) };
    if (p === '/registry/dispatcher') return { kind: 'dispatcherRules' };
    if (p === '/registry/policy') return { kind: 'policy' };
    if (p === '/registry/subjects') return { kind: 'systemSubjects' };
    // 4. Design — reviews lead; experiments and feedback tabs.
    if (p === '/design') return { kind: 'systemDesign' };
    if (p === '/design/experiments') return { kind: 'experiments' };
    if (p === '/design/feedback') return { kind: 'systemFeedback' };
    if (p === '/design/backlog') return { kind: 'systemBacklog' };
    // 5. Estate. 6. KB. Plus the unlisted auth door.
    if (p === '/estate') return { kind: 'systemEstate' };
    if (p === '/kb') return { kind: 'systemKb' };
    if (p === '/auth-admin') return { kind: 'authAdmin' };
    // Workflow detail LAST — its wildcard would eclipse the
    // specific /registry/* cases above.
    const jkM = p.match(/^\/registry\/(.+)$/);
    if (jkM) return { kind: 'workflowDetail', kindSlug: decodeURIComponent(jkM[1]!) };
    // Unknown /it path: the department's own landing, not Home.
    return { kind: 'systemYard' };
  }

  // ===== User Experiences perspective — /ux/* (canonical); bare / is the public alias for the UX home.
  // Unprefixed legacy paths still resolve here (defensive). =====
  const p = raw === '/' || raw === '/ux' ? '/' : raw.startsWith('/ux/') ? raw.slice('/ux'.length) : raw;
  // User Experiences lands on My Day by default — the actor's personal
  // work view, not a marketing landing. (The landing page stays the
  // catch-all fallback for unknown paths, at the bottom of this fn.)
  if (p === '/') return { kind: 'me' };
  if (p === '/me') return { kind: 'me' };
  if (p === '/inbox') return { kind: 'inbox' };
  if (p === '/views') return { kind: 'views' };
  if (p === '/accounts') return { kind: 'accounts' };
  const cm = p.match(/^\/accounts\/(.+)$/);
  if (cm) return { kind: 'account', accountId: cm[1]! };

  if (p === '/vendors') return { kind: 'vendors' };
  const vm = p.match(/^\/vendors\/(.+)$/);
  if (vm) return { kind: 'vendor', vendorLookup: decodeURIComponent(vm[1]!) };

  if (p === '/people') return { kind: 'people' };
  const em = p.match(/^\/people\/(.+)$/);
  if (em) return { kind: 'employee', empId: em[1]! };

  if (p === '/parts') return { kind: 'parts' };
  const partM = p.match(/^\/parts\/(.+)$/);
  if (partM) return { kind: 'part', partSku: decodeURIComponent(partM[1]!) };

  if (p === '/products') return { kind: 'products' };
  const prodM = p.match(/^\/products\/(.+)$/);
  if (prodM) return { kind: 'product', productSku: decodeURIComponent(prodM[1]!) };

  if (p === '/finance') return { kind: 'finance' };
  if (p === '/finance/new') return { kind: 'newInvoice' };
  if (p === '/finance/journal-entries/new') return { kind: 'newJournalEntry' };
  // Wildcard MUST come after every specific `/finance/X` case above —
  // it eagerly matches any tail and would otherwise eclipse them.
  const invM = p.match(/^\/finance\/(.+)$/);
  if (invM) return { kind: 'invoice', invoiceId: decodeURIComponent(invM[1]!) };

  if (p === '/shipping') return { kind: 'shipping' };
  const shipM = p.match(/^\/shipments\/(.+)$/);
  if (shipM) return { kind: 'shipmentDetail', shipmentId: decodeURIComponent(shipM[1]!) };

  if (p === '/support') return { kind: 'support' };

  if (p === '/calendar/me') return { kind: 'myCalendar' };
  if (p === '/calendar') return { kind: 'calendar' };
  if (p === '/service/schedule') return { kind: 'schedule' };
  if (p === '/exec') return { kind: 'exec' };
  if (p === '/warehouse') return { kind: 'warehouse' };
  if (p === '/catalog') return { kind: 'catalog' };
  const catM = p.match(/^\/catalog\/(.+)$/);
  if (catM) return { kind: 'device', sku: decodeURIComponent(catM[1]!) };
  if (p === '/assets') return { kind: 'assets' };
  const assetM = p.match(/^\/assets\/(.+)$/);
  if (assetM) return { kind: 'asset', assetId: decodeURIComponent(assetM[1]!) };
  if (p === '/marketing-assets') return { kind: 'marketingAssets' };
  const mktM = p.match(/^\/marketing-assets\/(.+)$/);
  if (mktM) return { kind: 'marketingAsset', assetId: decodeURIComponent(mktM[1]!) };
  if (p === '/manual') return { kind: 'manual' };
  const mManual = p.match(/^\/manual\/(.+)$/);
  if (mManual) return { kind: 'manualSection', slug: decodeURIComponent(mManual[1]!) };
  const poM = p.match(/^\/purchase-orders\/(.+)$/);
  if (poM) return { kind: 'po', poId: decodeURIComponent(poM[1]!) };
  const viM = p.match(/^\/vendor-invoices\/(.+)$/);
  if (viM) return { kind: 'vendorInvoice', vendorInvoiceId: decodeURIComponent(viM[1]!) };
  if (p === '/watchlist') return { kind: 'watchlist' };
  if (p === '/shop') return { kind: 'shop' };
  const shopM = p.match(/^\/shop\/(.+)$/);
  if (shopM) return { kind: 'shopProduct', sku: decodeURIComponent(shopM[1]!) };

  if (p === '/search') {
    const sp = new URLSearchParams(window.location.search);
    return { kind: 'search', q: sp.get('q') ?? '' };
  }

  if (p === '/hr') return { kind: 'hr' };
  if (p === '/qa') return { kind: 'qa' };

  if (p === '/service') return { kind: 'service' };
  const tm = p.match(/^\/service\/(.+)$/);
  if (tm) return { kind: 'jobDetail', jobId: tm[1]! };

  if (p === '/sales') return { kind: 'sales' };
  const sm = p.match(/^\/sales\/(.+)$/);
  if (sm) return { kind: 'jobDetail', jobId: sm[1]! };

  if (p === '/jobs') {
    const sp = new URLSearchParams(window.location.search);
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
    const filterSubjectKind = sp.get('filter_subject_kind');
    const filterSubjectId = sp.get('subject_id');
    const r: Route = { kind: 'jobs' };
    if (jk) (r as { workflow?: string }).workflow = jk;
    if (jkp) (r as { workflowPrefix?: string }).workflowPrefix = jkp;
    if (js) (r as { jobStatus?: string }).jobStatus = js;
    if (ownerId) (r as { jobOwnerId?: string }).jobOwnerId = ownerId;
    if (filterSubjectId) (r as { jobSubjectId?: string }).jobSubjectId = filterSubjectId;
    if (filterSubjectKind) (r as { jobSubjectKind?: string }).jobSubjectKind = filterSubjectKind;
    if (newJob === '1') (r as { newJobOpen?: boolean }).newJobOpen = true;
    if (sk) (r as { newJobSubjectKind?: string }).newJobSubjectKind = sk;
    if (sid) (r as { newJobSubjectId?: string }).newJobSubjectId = sid;
    return r;
  }
  // Before the greedy /jobs/(.+) below, which would otherwise swallow
  // the whole `{id}/steps/{stepId}` tail as a job id.
  const sfm = p.match(/^\/jobs\/([^/]+)\/steps\/([^/]+)$/);
  if (sfm) {
    const sp = new URLSearchParams(window.location.search);
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

  const jm = p.match(/^\/jobs\/(.+)$/);
  if (jm) return { kind: 'jobDetail', jobId: jm[1]! };

  return { kind: 'home' };
}

// `href` (honors the /dashboard mount) + `navigate` (pushState SPA nav)
// now live in the shared @boss/web-kit/nav module. Re-exported here so
// the ~55 files importing them from '../router' need no change.
export { href, navigate } from '@boss/web-kit/nav';
