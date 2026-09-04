<script lang="ts">
  // Root component — parses the URL, dispatches to the matched
  // page inside AppShell.
  //
  // Phase 1 wires /me, /jobs, /jobs/:id, /service, /sales,
  // /assets, /assets/:id. Unmatched URLs fall back to My Day
  // (same as the React app's default).

  import { onMount } from 'svelte';
  import { parseRoute, type Route } from './router';
  import { loadSession } from '@boss/web-kit/session/session.svelte';
  import { loadManifest } from '@boss/web-kit/session/manifest.svelte';
  import { loadStepTypeRegistry } from './steps/surfaceRegistry.svelte';
  import { loadClasses } from '@boss/web-kit/session/classes.svelte';
  import AppShell from './shell/AppShell.svelte';
  import UpdateBar from './shell/UpdateBar.svelte';
  import { APPS, appForSection, APP_SUBJECT_KINDS, type AppId } from './shell/nav-catalog';
  import { SECTION_FOR_ROUTE } from './shell/sections';
  import StepFocusPage from './steps/StepFocusPage.svelte';
  import PerspectiveTabs from '@boss/web-kit/PerspectiveTabs.svelte';
  import DebugGear from './debug/DebugGear.svelte';
  import MePage from './me/MePage.svelte';
  import JobsListPage from './jobs/JobsListPage.svelte';
  import JobDetailPage from './jobs/JobDetailPage.svelte';
  import MarketingAssetsList from './marketing-assets/MarketingAssetsList.svelte';
  import MarketingAssetPage from './marketing-assets/MarketingAssetPage.svelte';
  import AccountsList from './accounts/AccountsList.svelte';
  import AccountPage from './accounts/AccountPage.svelte';
  import VendorsList from './vendors/VendorsList.svelte';
  import VendorPage from './vendors/VendorPage.svelte';
  import PeopleList from './people/PeopleList.svelte';
  import EmployeePage from './people/EmployeePage.svelte';
  import PartsList from './parts/PartsList.svelte';
  import PartPage from './parts/PartPage.svelte';
  import ProductsList from './products/ProductsList.svelte';
  import ProductPage from './products/ProductPage.svelte';
  import ShippingPage from './shipping/ShippingPage.svelte';
  import ShipmentPage from './shipping/ShipmentPage.svelte';
  import SupportPage from './support/SupportPage.svelte';
  import FinancePage from './finance/FinancePage.svelte';
  import InvoicePage from './finance/InvoicePage.svelte';
  import NewInvoicePage from './finance/NewInvoicePage.svelte';
  import NewJournalEntryPage from './finance/NewJournalEntryPage.svelte';
  import HrPage from './hr/HrPage.svelte';
  import QaPage from './qa/QaPage.svelte';
  // SimPage retired 2026-05-03 — boss-sim-api is gone (HumanWorker
  // generator retirement step 9b). Tenant runners are CLI tools now.
  import ItKnowledgeBasePage from './it/ItKnowledgeBasePage.svelte';
  import AtlasPage from './it/monitoring/AtlasPage.svelte';
  import PolicyPage from './policy/PolicyPage.svelte';
  import WorkflowsAdminPage from './workflows/WorkflowsAdminPage.svelte';
  import WorkflowNewPage from './workflows/WorkflowNewPage.svelte';
  import WorkflowDesignWorkspace from './workflows/WorkflowDesignWorkspace.svelte';
  import WorkflowDetailPage from './workflows/WorkflowDetailPage.svelte';
  import StepPluginsPage from './it/step-plugins/StepPluginsPage.svelte';
  import StepPluginDetailPage from './it/step-plugins/StepPluginDetailPage.svelte';
  import DispatcherCascadePage from './dispatcher/DispatcherCascadePage.svelte';
  import DispatcherRulesPage from './dispatcher/DispatcherRulesPage.svelte';
  import DispatcherRuleEditPage from './dispatcher/DispatcherRuleEditPage.svelte';
  import SubjectsClassesPage from './it/subjects/SubjectsClassesPage.svelte';
  import YardPage from './it/yard/YardPage.svelte';
  import YardStatusPage from './it/yard/YardStatusPage.svelte';
  import EstatePage from './it/estate/EstatePage.svelte';
  import FleetPage from './it/monitoring/FleetPage.svelte';
  import ItTabs from './it/ItTabs.svelte';
  import DesignReviewPage from './it/design/DesignReviewPage.svelte';
  import ExperimentsPage from './it/experiments/ExperimentsPage.svelte';
  import InboxPage from './inbox/InboxPage.svelte';
  import CalendarPage from './calendar/CalendarPage.svelte';
  import MyCalendarPage from './calendar/MyCalendarPage.svelte';
  import SchedulePage from './schedule/SchedulePage.svelte';
  import ExecPage from './exec/ExecPage.svelte';
  import WarehousePage from './warehouse/WarehousePage.svelte';
  import CatalogBrowser from './catalog/CatalogBrowser.svelte';
  import DevicePage from './catalog/DevicePage.svelte';
  import AssetsList from './assets/AssetsList.svelte';
  import AssetPage from './assets/AssetPage.svelte';
  import ManualPage from './content/ManualPage.svelte';
  import WorkflowsPage from './kb/WorkflowsPage.svelte';
  import PerfPage from './it/monitoring/PerfPage.svelte';
  import EventsPage from './it/monitoring/EventsPage.svelte';
  import PoPage from './po/PoPage.svelte';
  import VendorInvoicePage from './po/VendorInvoicePage.svelte';
  import WatchlistPage from './accounts/WatchlistPage.svelte';
  import ShopHome from './shop/ShopHome.svelte';
  import ShopProductPage from './shop/ShopProductPage.svelte';
  import LandingPage from './landing/LandingPage.svelte';
  import SearchResultsPage from './search/SearchResultsPage.svelte';
  import ViewsPage from './views/ViewsPage.svelte';
  import FeedbackTriagePage from './it/feedback/FeedbackTriagePage.svelte';
  import BacklogBoardPage from './it/backlog/BacklogBoardPage.svelte';
  import IncidentsPage from './it/incidents/IncidentsPage.svelte';
  import LoginPage from './auth/LoginPage.svelte';
  import AuthAdminPage from './auth/AuthAdminPage.svelte';
  import ModuleDisabled from './shell/ModuleDisabled.svelte';
  import { moduleEnabled } from '@boss/web-kit/session/manifest.svelte';

  let route = $state<Route>(parseRoute(window.location.pathname));

  // Map route.kind → tenant module-id. Routes whose module is
  // flagged false in tenant.toml render a "not enabled" notice
  // instead of an empty/broken page. Routes not listed here are
  // always-on (jobs, people, finance, etc. — never gated).
  function routeRequiredModule(kind: Route['kind']): { id: string; label: string } | null {
    switch (kind) {
      case 'support':
      case 'service':
      case 'shipping':
      case 'shipmentDetail':  return { id: 'shipping',  label: 'Shipments' };
      case 'calendar':        return { id: 'calendar',  label: 'Release calendar' };
      case 'marketingAssets':
      case 'marketingAsset':  return { id: 'marketing-assets', label: 'Marketing assets' };
      case 'catalog':
      case 'device':
      case 'assets':
      case 'asset':           return { id: 'equipment', label: 'Equipment' };
      case 'shop':
      case 'shopProduct':     return { id: 'shop',      label: 'Shop' };
      case 'exec':            return { id: 'exec',      label: 'Exec' };
      default:                return null;
    }
  }

  let blockedModule = $derived.by(() => {
    const req = routeRequiredModule(route.kind);
    if (req && !moduleEnabled(req.id)) return req;
    return null;
  });

  // 401-redirect interceptor. Wraps window.fetch so any
  // /api/* response that comes back unauthenticated kicks the
  // operator to /login with the current path captured as ?next=.
  //
  // This used to be skipped in demo mode, because a middleware
  // minted a session for anyone arriving without one and several
  // 401s were then routine rather than a problem. Nothing mints a
  // session any more: a visitor is signed in as an employee, or as
  // a guest, or not at all. So a 401 means what it says, and the
  // interceptor runs for everyone.
  //
  // It is what a guest whose session expired needs most — without
  // it they keep browsing a shell that looks signed in while every
  // request fails, which is precisely how the expiry that killed
  // demo mode presented.
  //
  // Identity PROBES are exempt. `/api/session` and `/api/auth/*` are
  // the questions "who am I" — a 401 there is the ANSWER (nobody),
  // consumed by the session state machine, not an expiry mid-browse.
  // Redirecting on the probe made `session.value = unauthenticated`
  // unreachable: loadSession's own fetch bounced the visitor before
  // the state could ever render, so MePage's advice for that state
  // ("reload to log in") described a loop, not a way in. Data reads
  // and writes still redirect — that is the interceptor's real job.
  {
    const isIdentityProbe = (url: string): boolean =>
      url.startsWith('/api/session') || url.startsWith('/api/auth/');
    const _origFetch = window.fetch;
    window.fetch = (async (
      input: RequestInfo | URL,
      init?: RequestInit,
    ): Promise<Response> => {
      const resp = await _origFetch(input, init);
      if (resp.status === 401 && window.location.pathname !== '/login') {
        const url = typeof input === 'string'
          ? input
          : input instanceof URL ? input.href : input.url;
        // Only redirect on /api/* — let app-internal 401 handling
        // for non-API resources stay where the call was made.
        if (url.startsWith('/api/') && !isIdentityProbe(url)) {
          const next = encodeURIComponent(window.location.pathname + window.location.search);
          window.location.href = `/login?next=${next}`;
        }
      }
      return resp;
    }) as typeof window.fetch;
  }

  onMount(() => {
    loadSession();
    loadManifest();
    loadStepTypeRegistry();
    loadClasses('employee');
    const onPop = () => {
      route = parseRoute(window.location.pathname);
    };
    window.addEventListener('popstate', onPop);
    return () => window.removeEventListener('popstate', onPop);
  });

  // Which sidebar section highlights. The route->section map lives in
  // shell/sections.ts as a typed Record so a new route kind cannot fall
  // through silently, and sections.test.ts pins every section id to a
  // ROUTE_CATALOG key.
  let activeSection = $derived(SECTION_FOR_ROUTE[route.kind]);

  // Which app tab is active. Derived from `activeSection` via the
  // catalog's `app` field.
  //
  // This replaced a MODEL_KINDS set of Route['kind']s maintained here
  // alongside a MODEL_ROUTES set of RouteNames in AppShell.svelte.
  // Two lists in two vocabularies answering one question, which had
  // to agree for every routed surface: miss one and the page rendered
  // with the wrong tab highlighted and the wrong sidebar, silently.
  let perspective: AppId = $derived(appForSection(activeSection));

  // Both chrome render sites read this. They previously repeated the
  // prop list, and drifted: the step-focus bar shipped without
  // `searchAppKinds`, so global search silently lost its app scoping
  // on exactly the surface built for focused reading.
  let appKinds: ReadonlyArray<string> = $derived(APP_SUBJECT_KINDS[perspective] ?? []);
</script>

<!-- Every route, every state: a stale tab is stale regardless of
     where it is parked (72c7c36e). Renders nothing until a deploy
     actually lands. -->
<UpdateBar />

{#if route.kind === 'login'}
  <LoginPage />
{:else if route.kind === 'stepFocus'}
  <!-- Outside AppShell on purpose: a full-page step surface has no
       sidebar. The chrome bar stays — you can still switch apps —
       but everything below it belongs to the step. -->
  <PerspectiveTabs active={perspective} apps={APPS} searchAppKinds={appKinds} />
  <StepFocusPage jobId={route.jobId} stepId={route.stepId} from={route.from} fromLabel={route.fromLabel} />
{:else}
  <PerspectiveTabs active={perspective} apps={APPS} searchAppKinds={appKinds} />
<AppShell {activeSection} {perspective}>
  {#if blockedModule}
    <ModuleDisabled module={blockedModule.id} label={blockedModule.label} />
  {:else if route.kind === 'home'}
      <LandingPage />
    {:else if route.kind === 'search'}
      <SearchResultsPage q={route.q} />
    {:else if route.kind === 'views'}
      <ViewsPage />
    {:else if route.kind === 'systemFeedback'}
      <ItTabs group="design" active="/it/design/feedback" />
      <FeedbackTriagePage />
    {:else if route.kind === 'systemBacklog'}
      <ItTabs group="design" active="/it/design/backlog" />
      <BacklogBoardPage />
    {:else if route.kind === 'authAdmin'}
      <AuthAdminPage />
    {:else if route.kind === 'me'}
      <MePage />
    {:else if route.kind === 'jobs'}
      <JobsListPage
        initialKind={route.workflow ?? ''}
        initialKindPrefix={route.workflowPrefix ?? ''}
        initialStatus={route.jobStatus ?? 'open'}
        initialOwnerId={route.jobOwnerId ?? ''}
        initialSubjectKind={route.jobSubjectKind ?? ''}
        initialSubjectId={route.jobSubjectId ?? ''}
        initialNewJobOpen={route.newJobOpen ?? false}
        initialNewJobSubjectKind={route.newJobSubjectKind ?? ''}
        initialNewJobSubjectId={route.newJobSubjectId ?? ''}
      />
    {:else if route.kind === 'jobDetail'}
      <JobDetailPage jobId={route.jobId} />
    {:else if route.kind === 'service'}
      <JobsListPage
        initialKind="field-service"
        initialStatus="open"
        pageTitle="Service queue"
      />
    {:else if route.kind === 'sales'}
      <JobsListPage
        initialKind="sale"
        initialStatus="open"
        pageTitle="Sales pipeline"
      />
    {:else if route.kind === 'assets'}
      <AssetsList />
    {:else if route.kind === 'asset'}
      <AssetPage assetId={route.assetId} />
    {:else if route.kind === 'accounts'}
      <AccountsList />
    {:else if route.kind === 'account'}
      <AccountPage accountId={route.accountId} />
    {:else if route.kind === 'vendors'}
      <VendorsList />
    {:else if route.kind === 'vendor'}
      <VendorPage vendorLookup={route.vendorLookup} />
    {:else if route.kind === 'people'}
      <PeopleList />
    {:else if route.kind === 'employee'}
      <EmployeePage empId={route.empId} />
    {:else if route.kind === 'parts'}
      <PartsList />
    {:else if route.kind === 'part'}
      <PartPage partSku={route.partSku} />
    {:else if route.kind === 'products'}
      <ProductsList />
    {:else if route.kind === 'product'}
      <ProductPage sku={route.productSku} />
    {:else if route.kind === 'shipping'}
      <ShippingPage />
    {:else if route.kind === 'shipmentDetail'}
      <ShipmentPage shipmentId={route.shipmentId} />
    {:else if route.kind === 'support'}
      <SupportPage />
    {:else if route.kind === 'finance'}
      <FinancePage />
    {:else if route.kind === 'newInvoice'}
      <NewInvoicePage />
    {:else if route.kind === 'newJournalEntry'}
      <NewJournalEntryPage />
    {:else if route.kind === 'invoice'}
      <InvoicePage invoiceId={route.invoiceId} />
    {:else if route.kind === 'hr'}
      <HrPage />
    {:else if route.kind === 'qa'}
      <QaPage />
    {:else if route.kind === 'systemKb'}
      <ItKnowledgeBasePage />
    {:else if route.kind === 'policy'}
      <ItTabs group="registry" active="/it/registry/policy" />
      <PolicyPage />
    {:else if route.kind === 'workflowsAdmin'}
      <WorkflowsAdminPage />
    {:else if route.kind === 'workflowNew'}
      <WorkflowNewPage />
    {:else if route.kind === 'workflowDesign'}
      <WorkflowDesignWorkspace jobId={route.jobId} />
    {:else if route.kind === 'workflowDetail'}
      <WorkflowDetailPage kindSlug={route.kindSlug} />
    {:else if route.kind === 'systemStepPlugins'}
      <ItTabs group="registry" active="/it/registry/step-plugins" />
      <StepPluginsPage />
    {:else if route.kind === 'systemStepPluginDetail'}
      <StepPluginDetailPage pluginSlug={route.pluginSlug} />
    {:else if route.kind === 'systemDesign'}
      <ItTabs group="design" active="/it/design" />
      <DesignReviewPage />
    {:else if route.kind === 'systemYard'}
      <YardPage />
    {:else if route.kind === 'systemEstate'}
      <EstatePage />
    {:else if route.kind === 'systemFleet'}
      <ItTabs group="operate" active="/it/operate/bottlenecks" />
      <FleetPage />
    {:else if route.kind === 'systemYardStatus'}
      <ItTabs group="operate" active="/it/operate/yard-status" />
      <YardStatusPage />
    {:else if route.kind === 'experiments'}
      <ItTabs group="design" active="/it/design/experiments" />
      <ExperimentsPage />
    {:else if route.kind === 'dispatcherRules'}
      <ItTabs group="registry" active="/it/registry/dispatcher" />
      <DispatcherCascadePage />
    {:else if route.kind === 'dispatcherRulesList'}
      <DispatcherRulesPage />
    {:else if route.kind === 'dispatcherRuleEdit'}
      <DispatcherRuleEditPage ruleName={route.ruleName} />
    {:else if route.kind === 'systemSubjects'}
      <ItTabs group="registry" active="/it/registry/subjects" />
      <SubjectsClassesPage />
    {:else if route.kind === 'inbox'}
      <InboxPage />
    {:else if route.kind === 'calendar'}
      <CalendarPage />
    {:else if route.kind === 'myCalendar'}
      <MyCalendarPage />
    {:else if route.kind === 'schedule'}
      <SchedulePage />
    {:else if route.kind === 'exec'}
      <ExecPage />
    {:else if route.kind === 'warehouse'}
      <WarehousePage />
    {:else if route.kind === 'catalog'}
      <CatalogBrowser />
    {:else if route.kind === 'device'}
      <DevicePage sku={route.sku} />
    {:else if route.kind === 'marketingAssets'}
      <MarketingAssetsList />
    {:else if route.kind === 'marketingAsset'}
      <MarketingAssetPage assetId={route.assetId} />
    {:else if route.kind === 'manual'}
      <ManualPage slug={null} />
    {:else if route.kind === 'manualSection'}
      <ManualPage slug={route.slug} />
    {:else if route.kind === 'workflows'}
      <ItTabs group="registry" active="/it/registry" />
      <WorkflowsPage />
    {:else if route.kind === 'systemMonitoringPerf'}
      <ItTabs group="operate" active="/it/operate/perf" />
      <PerfPage />
    {:else if route.kind === 'systemMonitoringEvents'}
      <ItTabs group="operate" active="/it/operate/audit" />
      <EventsPage />
    {:else if route.kind === 'systemMonitoringAtlas'}
      <ItTabs group="operate" active="/it/operate/atlas" />
      <AtlasPage />
    {:else if route.kind === 'po'}
      <PoPage poId={route.poId} />
    {:else if route.kind === 'vendorInvoice'}
      <VendorInvoicePage vendorInvoiceId={route.vendorInvoiceId} />
    {:else if route.kind === 'watchlist'}
      <WatchlistPage />
    {:else if route.kind === 'incidents'}
      <ItTabs group="operate" active="/it/operate" />
      <IncidentsPage />
    {:else if route.kind === 'shop'}
      <ShopHome />
    {:else if route.kind === 'shopProduct'}
      <ShopProductPage sku={route.sku} />
    {:else}
      <MePage />
    {/if}
</AppShell>
{/if}

<DebugGear />
