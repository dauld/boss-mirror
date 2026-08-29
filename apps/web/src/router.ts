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
  | { kind: 'systemKb' }
  | { kind: 'systemMonitoring' }
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
  /// The network map — every registry station as a node (stations.md).
  | { kind: 'systemMap' }
  | { kind: 'systemFlow' }
  | { kind: 'systemFleet' }
  /// The IT incidents surface — active incident-post-mortem packets +
  /// the closed ones rendered as a durable archive.
  | { kind: 'incidents' }
  | { kind: 'systemSubjects' }
  | { kind: 'systemModel' }
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

  // IT surfaces answer on /it/* as well as /system/*.
  //
  // Feedback 0fc8b216: "We should not be under /system route because I
  // am working on a Job in the context of the IT department. So I
  // should be in the /it route IT app." He is right — nav-catalog
  // already assigns every one of these to `app: 'it'`, so the URL was
  // the last place still calling them System surfaces.
  //
  // /system/* is kept, permanently and deliberately. Those paths are in
  // bookmarks, in the station registry's `upstream` hrefs, and in docs;
  // an alias costs one line and a redirect that breaks a saved link
  // costs an operator their place. Canonical moves, old paths keep
  // answering.
  if (raw === '/it' || raw.startsWith('/it/')) {
    raw = `/system${raw.slice('/it'.length)}`;
  }

  // ===== System Model perspective — /system/* =====
  if (raw === '/system' || raw.startsWith('/system/')) {
    const p = raw.slice('/system'.length) || '/';
    if (p === '/') return { kind: 'systemModel' };
    if (p === '/monitoring') return { kind: 'systemMonitoring' };
    if (p === '/monitoring/perf') return { kind: 'systemMonitoringPerf' };
    if (p === '/monitoring/events') return { kind: 'systemMonitoringEvents' };
    if (p === '/monitoring/atlas') return { kind: 'systemMonitoringAtlas' };
    if (p === '/kb') return { kind: 'systemKb' };
    if (p === '/design') return { kind: 'systemDesign' };
    if (p === '/yard') return { kind: 'systemYard' };
    if (p === '/map') return { kind: 'systemMap' };
    if (p === '/flow') return { kind: 'systemFlow' };
    if (p === '/fleet') return { kind: 'systemFleet' };
    if (p === '/incidents') return { kind: 'incidents' };
    if (p === '/feedback') return { kind: 'systemFeedback' };
    if (p === '/experiments') return { kind: 'experiments' };
    if (p === '/subjects') return { kind: 'systemSubjects' };
    if (p === '/policy') return { kind: 'policy' };
    if (p === '/workflows') return { kind: 'workflows' };
    if (p === '/auth-admin') return { kind: 'authAdmin' };
    if (p === '/workflows/new') return { kind: 'workflowNew' };
    // The admin index. Was `/system/workflows` before the rename;
    // it and the catalog collapsed onto one path when `workflows`
    // became `workflows`, and they are two different surfaces.
    if (p === '/workflows/authoring') return { kind: 'workflowsAdmin' };
    const jkDesignM = p.match(/^\/workflows\/authoring\/(.+)$/);
    if (jkDesignM) return { kind: 'workflowDesign', jobId: decodeURIComponent(jkDesignM[1]!) };
    const jkM = p.match(/^\/workflows\/(.+)$/);
    if (jkM) return { kind: 'workflowDetail', kindSlug: decodeURIComponent(jkM[1]!) };
    if (p === '/step-plugins') return { kind: 'systemStepPlugins' };
    const spM = p.match(/^\/step-plugins\/(.+)$/);
    if (spM) return { kind: 'systemStepPluginDetail', pluginSlug: decodeURIComponent(spM[1]!) };
    if (p === '/dispatcher/rules') return { kind: 'dispatcherRulesList' };
    const drM = p.match(/^\/dispatcher\/rules\/(.+)$/);
    if (drM) return { kind: 'dispatcherRuleEdit', ruleName: decodeURIComponent(drM[1]!) };
    if (p === '/dispatcher') return { kind: 'dispatcherRules' };
    return { kind: 'systemModel' };
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
