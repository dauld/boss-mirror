# Playwright coverage — controls across the SPA

This is the working index for the "every button gets a test"
effort. Goal: every interactive control in the Svelte SPA has a
passing assertion in `tests/smoke/*.spec.ts`. Pages get checked
off (`[x]`) once their spec covers every `<button>`, `onclick`
handler, form submission, and significant input affordance.

**Two layers.**

- **`tests/smoke/*.spec.ts` — live-stack** (this suite). Runs
  against the dev-server proxying to the real backend
  (`playwright.config.ts`, scratch-isolated). High fidelity but
  needs the stack up; **not yet a CI gate** (Phase-2 work) — so
  treat a stale spec here as a real bug, not noise.
- **`tests/mocked/*.spec.ts` — mocked-backend** (CI-gated). Every
  `/api/**` call is intercepted in-browser
  (`tests/mocked/_mockApi.ts`), so it needs only the SPA shell —
  fast, deterministic, and run in the `web` CI job
  (`bun run test:mocked`, `playwright.mocked.config.ts`). This is
  where interactive behavior we want to *maintain* (e.g. the
  Workflow authoring graph/inspector/workflow rail) is guarded
  against regression.

**Conventions.**

- Test file naming: `<area>-<kind>.spec.ts`. One file per page is
  the default; split when a page has separable flows (e.g.
  `jobs-list.spec.ts` + `jobs-create.spec.ts`).
- Use `tests/smoke/_helpers.ts` for shared utilities — persona
  pinning, navigation, expect-toast, response-mocking. Keep
  per-page specs short and prose-light; the helper is where
  reusable behavior lives.
- Selectors prefer `getByRole({ name: /…/i })` over CSS classes
  so refactors don't break tests. Use class selectors only when
  no role/name fits.
- Each spec MUST exercise the buttons that mutate state, not just
  the page that renders. A button with no assertion against its
  effect doesn't count toward coverage.
- The dev server `bun src/dev-server.ts` is the test target.
  `playwright.config.ts` auto-spawns it via the `webServer` block.

**Counts as of the audit (2026-04-28):** 55 page-level surfaces;
~140 `onclick=` handlers; ~100 explicit `<button>` elements; 1
`<form>`. Plus component-internal controls inside pages (step
plugins, modals, table-row links, etc.) — realistic total is
~300 distinct interactive affordances.

---

## Pages — coverage status

Sorted by area. Each entry: `[ ]` = uncovered, `[~]` = partial,
`[x]` = every control under test. The `controls` count is a
quick scan (`<button` + `onclick=` deduped); spec authors verify
the actual surface.

### Work
- [x] `me/MePage.svelte` (1 control) —
      `tests/smoke/me-calendar.spec.ts` covers shell-topbar
      mount + (when seeded) job-card → detail navigation.
- [x] `jobs/JobsListPage.svelte` (8) —
      `tests/smoke/jobs-create.spec.ts` covers Start a new Job /
      Create Ad Hoc Job entry points + the inline form;
      `tests/smoke/jobs-list.spec.ts` covers status filter
      buttons + subtitle rename + row → detail navigation.
- [x] `jobs/JobDetailPage.svelte` (1) — step interactions live in
      child plugin components. `tests/smoke/detail-pages.spec.ts`
      covers a real seeded job id discovered at runtime.
- [x] `inbox/InboxPage.svelte` (12) — `tests/smoke/inbox.spec.ts`
      covers Compose modal (open / cancel / ✕) + Send enabled
      gate + 4 filter buttons + search input persistence.
- [x] `schedule/SchedulePage.svelte` (3) —
      `tests/smoke/ops-tabs.spec.ts` covers prev / today / next
      week navigation with header round-trip assertion.
- [x] `calendar/CalendarPage.svelte` (1) —
      `tests/smoke/me-calendar.spec.ts` covers 30d / 90d / 180d
      window-preset buttons via inline-style assertion.
- [x] `calendar/MyCalendarPage.svelte` (3) — same spec file
      covers prev / this / next week + the round-trip.

### Browse
- [x] `accounts/AccountsList.svelte` (5) —
      `tests/smoke/accounts-list.spec.ts` covers the filter
      sidebar (tier + state buttons + search input) and row →
      detail navigation. Tests skip the row-navigation case
      gracefully when audit_log has no `accounts.*` events
      (current state — see backlog note).
- [~] `accounts/AccountPage.svelte` (1) —
      `tests/smoke/detail-pages.spec.ts` covers the render
      contract; skips when no accounts seeded.
- [x] `accounts/WatchlistPage.svelte` (13) —
      `tests/smoke/browse-watchlist-parts.spec.ts` covers Risk
      bucket buttons (4) + Tier buttons (4) + Search input + the
      sortable column-header click.
- [~] `vendors/VendorsList.svelte` (4) —
      `tests/smoke/list-pages.spec.ts` covers filter sidebar
      mount + category-filter All button. Per-category buttons +
      row nav skip on empty seed (no `vendors.*` events).
- [~] `vendors/VendorPage.svelte` (0) — same spec covers the
      render contract; skips when no vendors seeded.
- [x] `people/PeopleList.svelte` (7) — same spec covers View
      mode (List / Hierarchy) + Status filter buttons + dept
      filter when populated.
- [x] `people/EmployeePage.svelte` (0) — same spec covers the
      render contract (mounts for any employee in the roster).
- [x] `parts/PartsList.svelte` (10) — same spec file covers the
      stock-status filter buttons + category filter buttons +
      row-link navigation.
- [x] `parts/PartPage.svelte` (0) — same spec covers a known-
      good brewery SKU.
- [x] `assets/AssetsList.svelte` (4) —
      `tests/smoke/list-pages.spec.ts` covers Phase filter All
      button + search-input persistence + row-link navigation.
- [~] `assets/AssetPage.svelte` (1) — same spec covers the
      render contract; skips on empty assets.
- [~] `catalog/CatalogBrowser.svelte` (4) — same spec covers
      Category filter + product-card click; skips on empty
      `/api/catalog/models`.
- [~] `catalog/DevicePage.svelte` (0) — same spec covers the
      render contract; skips when catalog/models empty.

### Finance
- [x] `finance/FinancePage.svelte` (1) — tabs.
      `tests/smoke/admin-finance-ops-tail.spec.ts` covers all 8
      tabs (Overview / Invoices / PO Approvals / Income statement
      / Balance sheet / Cash flow / Trial Balance / Tax liability)
      via aria-selected toggle.
- [x] `finance/InvoicePage.svelte` (0) —
      `tests/smoke/detail-pages.spec.ts` covers a real seeded
      invoice id discovered at runtime.
- [x] `finance/NewInvoicePage.svelte` (4) —
      `tests/smoke/finance-forms.spec.ts` covers Create-invoice
      disabled gate, Add line / Remove line, Cancel → /finance.
- [x] `finance/NewJournalEntryPage.svelte` (5) — same spec
      file covers Add debit / Add credit line + Cancel.
- [~] `po/PoPage.svelte` (0) — same spec covers the render
      contract; skips when no purchase orders seeded.

### Operations
- [x] `shipping/ShippingPage.svelte` (3) —
      `tests/smoke/manual-shipping.spec.ts` covers direction
      tabs (All / Inbound / Outbound) + search input + status
      filter buttons (All toggle).
- [x] `shipping/ShipmentPage.svelte` (0) — same spec covers a
      real seeded shipment id discovered at runtime.
- [x] `warehouse/WarehousePage.svelte` (8) —
      `tests/smoke/ops-tabs.spec.ts` covers tab nav (Overview /
      Inventory / Receiving) + inventory filter buttons + the
      Receiving Create-PO toggle.
- [x] `support/SupportPage.svelte` (1) — same spec covers
      Overview / Active Cases / Account Health tabs.
- [x] `hr/HrPage.svelte` (4) — same spec file covers the 5 tabs
      (Overview / Workflows / Requisitions / Certifications /
      Headcount).
- [x] `qa/QaPage.svelte` (1) — same spec covers all 4 tabs
      (Overview / Batch QC / Compliance / Equipment preventive maintenance).
- [x] `assets/AssetsList.svelte` (5) —
      `tests/smoke/list-pages.spec.ts` covers Kind filter buttons
      + Include-retired toggle.
- [~] `assets/AssetPage.svelte` (0) —
      `tests/smoke/detail-pages.spec.ts` covers the render
      contract; skips when no assets seeded.
- [x] `ops/OpsDashboard.svelte` (0) — same spec covers the
      render contract.
- [x] `sim/SimPage.svelte` (7) — `tests/smoke/sim.spec.ts`
      covers Reset / Run / Steppable checkbox / scenario picker
      active-state flip.

### Shop
- [~] `shop/ShopHome.svelte` (6) — `tests/smoke/shop.spec.ts`
      covers hero + filter sidebar mount; product-specific
      assertions skip cleanly because catalog/models is empty in
      the current seed.
- [~] `shop/ShopProductPage.svelte` (4) — same spec file covers
      Request-quote → form → Submit-gate → Cancel; skips when no
      products to navigate into.

### Exec / CTO
- [x] `exec/ExecPage.svelte` (0) — `tests/smoke/cto-exec.spec.ts`
      asserts ≥4 exec cards render.
- [x] `cto/CtoPage.svelte` (3) — same spec file exercises the
      script-picker filter buttons.
- [x] `cto/PerfPage.svelte` (9) — same spec file covers
      Pause/Resume toggle + Reset + sortable column headers.
- [~] `cto/EventsPage.svelte` (1) — same spec file covers the
      row-expand → JSON-pane toggle; skips when audit_log empty.
- [x] `cto/AtlasPage.svelte` (2) — same spec file asserts the
      flow svg + ≥1 navigable node.
- [x] `architecture/ArchitecturePage.svelte` (0) — same spec
      covers the render contract.

### Admin
- [x] `admin/WorkflowsPage.svelte` (0) — list.
      `tests/smoke/admin-workflows.spec.ts` covers row → detail
      navigation.
- [x] `admin/WorkflowNewPage.svelte` (1) — DAG editor.
      `tests/smoke/admin-workflow-new.spec.ts` covers spec
      inputs (slug / label / category / description) + subject-
      kind checkboxes + Create-draft button + StepDagEditor's
      Add-tier / Add-step / Show-JSON toggles.
- [~] `admin/WorkflowDetailPage.svelte` (6) —
      `tests/smoke/admin-workflows.spec.ts` covers Publish/Retire
      disabled-state + Fork navigation. Add-tier / add-step
      controls inside the DAG editor flyout still uncovered.
- [~] `admin/PolicyPage.svelte` (2) —
      `tests/smoke/admin-finance-ops-tail.spec.ts` covers the
      Refresh button mount; per-row Edit → flyout flow skips
      until policy rules seed.
- [x] `admin/StepPluginsPage.svelte` (0) — same spec covers
      list page mount + row → detail navigation.
- [x] `admin/StepPluginDetailPage.svelte` (2) — same spec covers
      Publish + Retire button visibility on the detail page.

### Knowledge
- [x] `content/ManualPage.svelte` (2) —
      `tests/smoke/manual-shipping.spec.ts` covers tree mount +
      label-link navigation + the collapsible toggle button.
- [—] `design/DesignIndexPage.svelte` and `design/DesignDocPage.svelte`
      are gone, and so is `tests/smoke/design-docs.spec.ts` that
      covered them. Both rendered the markdown corpus — the doc list,
      the Refresh-from-git button, the Flush-to-git button and the
      Accept/Override decision flow — and the corpus index, its
      service and the flush pipeline behind all of it were deleted on
      2026-09-10 (backlog f5da586c). `/it/design` renders the
      `design-review` station's queue now; its unit coverage is
      `src/it/design/designLens.test.ts`.

### Integrations / Landing
- [~] `it/ItPanel.svelte` (5) — same spec covers all 4 page
      tabs (Providers / Banking / Payroll / Tax). Provider-row
      expand → action buttons (Configure / Test / Sync) skip
      when no providers seeded.
- [x] `landing/LandingPage.svelte` (2) — Workflow picker on the
      unauth `/`. `tests/smoke/landing-shell.spec.ts` covers the
      hero + jobs-in-flight stat render and the kind-picker /
      recent-job button active-state toggles.

---

## Reusable / shared
- [x] `shell/AppShell.svelte` — sidebar nav links + persona
      switcher. `tests/smoke/landing-shell.spec.ts` covers the
      always-visible My Day + Inbox links + sidebar-group link
      navigation; `persona-switcher.spec.ts` covers the dropdown.
- [x] `debug/DebugGear.svelte` — `tests/smoke/debug-gear.spec.ts`
      asserts the role gate hides the gear when the session
      user's role is not `platform-admin` (the dev-server's
      default persona uses a tenant role like `cto`). The
      allowed-user path (sim buttons + log clear) requires a
      platform-admin session and isn't reachable from smoke.

---

## What "covered" means

A spec covers a page when, for every interactive control on that
page, the spec asserts at least one of:

1. **State mutation.** The click triggers a request whose response
   is observed (URL change, DOM change, toast, …).
2. **UI affordance.** The click toggles a visible UI state
   (dropdown opens, tab selects, modal mounts).
3. **Negative assertion.** The button is correctly disabled / hidden
   based on data state (e.g. "Cancel" disabled while submitting).

Read-only navigation links (table-row → detail-page) count once
per page, not once per row — the same handler runs for every row.

---

## Backlog notes

- Step plugins (`/var/lib/boss/step-plugins/*`) render inside
  JobDetailPage; their controls are out of scope for the
  page-level specs and tracked separately if/when they grow.
- Smoke tests run against the live-sim demo. The tenant seed
  populates accounts / messages / content /
  calendar at install, and the sim grows jobs / commerce / inventory
  / shipping live — so list pages fill in as the sim ticks. On a
  freshly-started demo some pages may still be sparse; list-page row
  tests skip on the "no rows" path until the sim has built them up.
- When dev-server reloads break a spec, prefer fixing the spec
  to `await page.waitForTimeout(…)` — use
  `expect.poll` / `expect(...).toBeVisible({ timeout })`.
- The `{#snippet children()}` wrapper around component bodies
  triggers a bun-plugin-svelte double-children bug (the empty
  auto-snippet shadows the explicit one). The codebase was
  swept of these in c2cf17a + the followup strip; new code
  should rely on Svelte 5's implicit children binding instead.
