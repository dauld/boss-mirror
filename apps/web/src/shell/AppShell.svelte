<script lang="ts">
  // App shell — persistent sidebar + content slot.
  //
  // Sidebar layout: a Work section (operator-tier surfaces tied to
  // the user's role + assignments) + a flat list of Browse/Know
  // surfaces. The legacy Admin tier was removed 2026-05-03 — admin-
  // shaped pages live in the regular sidebar gated by the policy
  // role check.

  import { session } from '@boss/web-kit/session/session.svelte';
  import { moduleEnabled, getLabel } from '@boss/web-kit/session/manifest.svelte';
  import { canSeeRoute, type RouteName, type Role } from '@boss/web-kit/session/permissions';
  import { workForRole } from '@boss/web-kit/session/work-by-role';
  import { departmentLabel } from '@boss/web-kit/nav';
  import { navigate } from '../router';
  import {
    ROUTE_CATALOG,
    type AppId,
    type NavItem,
    type NavGroup,
  } from './nav-catalog';

  // NavItem / NavGroup / ROUTE_CATALOG live in ./nav-catalog so both
  // this shell and App.svelte read the same registry — and so the
  // consistency test can import it instead of mirroring it by hand.
  // `app` on each entry is the single answer to "which tab owns this
  // surface"; it replaced this file's MODEL_ROUTES and App.svelte's
  // MODEL_KINDS, which had to agree and could silently stop agreeing.

  // Surfaces — one entry per department-rooted dashboard, in the
  // order an operator would scan them. Rendered as-is; the visible()
  // filter then drops anything the role/manifest blocks. A
  // service-only persona simply sees Service + Inventory + Shipments.
  let { activeSection, perspective = 'home', children } = $props<{
    activeSection: string;
    // Which app tab this shell renders under. Drives which surfaces
    // appear in the sidebar. Typed as the full AppId — the shell
    // speaks the same vocabulary as the catalog, so adding an app is
    // a catalog change rather than a widening here.
    perspective?: AppId;
    children: () => any;
  }>();

  // Svelte's generated props type widens `perspective` to `any` through
  // the default, which silently un-types every Record lookup below.
  // Pin it once.
  let activeApp = $derived(perspective as AppId);

  let user = $derived(
    session.value.kind === 'ready' ? session.value.user : null,
  );
  let role = $derived((user?.role ?? null) as Role | null);

  // Unread badge on Inbox (David, feedback 8c020e6d: "I can't see new
  // inbox messages").
  //
  // Counts `kind=direct` only, which is the same set the inbox itself
  // opens on. The unfiltered count for the platform admin is ~1,980
  // against 3 directs, so a badge wired to every kind would render the
  // noise as a number and be ignored within a day — the exact failure
  // the needs-you filter already exists to avoid. A number here has to
  // mean "somebody asked you something".
  //
  // Polls rather than subscribes: the SSE marker stream is not wired
  // to this shell, and 30s is well inside the latency that matters for
  // a question waiting in a queue. `?kind=direct` is counted
  // server-side, so this is one small JSON response, not the inbox.
  let unreadDirect = $state(0);

  $effect(() => {
    const id = user?.id;
    if (!id) return;
    // Bound to a local so the narrowing survives into the closure —
    // `user?.id` is `string | undefined` and TS re-widens it there.
    const uid: string = id;
    let cancelled = false;
    async function poll() {
      try {
        const r = await fetch(
          `/api/messages/unread/${encodeURIComponent(uid)}?kind=direct`,
        );
        if (!r.ok) return;
        const body = (await r.json()) as { count?: number };
        if (!cancelled) unreadDirect = body.count ?? 0;
      } catch {
        // A failed poll leaves the last known count rather than
        // zeroing it: showing "nothing waiting" because the network
        // blinked is the one wrong answer this badge can give.
      }
    }
    void poll();
    const t = setInterval(poll, 30_000);
    return () => {
      cancelled = true;
      clearInterval(t);
    };
  });

  // Per-department surface order. Each department app owns one group;
  // the order is how someone in that department would scan it, not
  // alphabetical. `visible()` then drops whatever the role or the
  // tenant manifest blocks.
  //
  // The keys are department codes because apps ARE departments now.
  // The previous version keyed off invented apps — `crm` held
  // accounts + sales + support + shop + marketing assets, and
  // `supply-chain` held six surfaces spanning four real departments —
  // so a marketer and a salesperson shared one list and neither list
  // matched an org chart anybody recognised.
  //
  // Only the ORDER lives here. Which department owns a surface is
  // answered once, by `app` in the nav catalog, and the test beside
  // this asserts these two agree — an entry here for a surface the
  // catalog assigns elsewhere is exactly the drift that put pages
  // under the wrong tab before.
  const APP_SURFACES: Readonly<Partial<Record<AppId, ReadonlyArray<RouteName>>>> = {
    sales: ['accounts', 'sales', 'shop'],
    marketing: ['marketing-assets'],
    support: ['support'],
    service: ['service'],
    qa: ['qa'],
    executive: ['exec'],
    finance: ['finance', 'vendors'],
    warehouse: ['warehouse', 'parts'],
    distribution: ['shipping'],
    production: ['products', 'calendar'],
    maintenance: ['catalog', 'assets'],
    people: ['people'],
  };

  // The group header is the department's own label — derived, because
  // a second spelling of "Finance" is a second thing to keep in step.
  function appGroupLabel(app: AppId): string {
    return app === 'home' || app === 'simulator' ? '' : departmentLabel(app);
  }

  // Work group is role-keyed: each role gets a tailored 3-5 item
  // list of the surfaces they personally operate from. The same
  // visible() filter still applies, so a brewery manifest that turns
  // off a module hides it from Work too.
  const WORK = $derived<NavGroup>({
    label: 'Work',
    items: workForRole(role).map((r) => ROUTE_CATALOG[r]),
  });

  // System Model perspective — surfaces grouped by the aspects of
  // operating the model: Run (observe the live machine), Define
  // (configure the model), Evolve (controlled change + experiments),
  // Platform (reference + admin). The User Experiences perspective
  // keeps Work / Surfaces / Knowledge Bases (below). Selected via the
  // `perspective` prop.
  const IT_GROUPS: ReadonlyArray<NavGroup> = [
    {
      label: 'Run',
      items: [
        // Flow first: the team's own dashboard. System Monitoring
        // below it answers the other question — what the machine is
        // doing, rather than what the people are getting through.
        ROUTE_CATALOG['system-flow'],
        // Fleet beside Flow: Flow is the team's throughput, Fleet is
        // where a kind's work is piling up on its Workflow.
        ROUTE_CATALOG['system-fleet'],
        ROUTE_CATALOG['system-model'],
        ROUTE_CATALOG['system-monitoring'],
        // Incidents beside Monitoring: monitoring is what the machine
        // is doing, incidents are what went wrong and what we learned.
        // Active packets to respond to + the post-mortem archive.
        ROUTE_CATALOG['system-incidents'],
        // Audit Log + Atlas are sub-pages of monitoring with no
        // distinct permKey — plain NavItems (permKey-less ⇒ always
        // visible + always in-perspective; see visible()/inPerspective()).
        { id: 'system-audit', label: 'Audit Log', path: '/system/monitoring/events' },
        { id: 'system-atlas', label: 'Atlas', path: '/system/monitoring/atlas' },
        // The executor network — who moves work and where it goes.
        // Belongs with the other live instruments rather than under
        // Define: it shows the system RUNNING, not how it is authored.
        // The yard is one batch station rendered deep (the pipeline's
        // queues); the map is every registry station rendered wide.
        // The yard row was missing since its car landed — repaired
        // here alongside the map's addition.
        ROUTE_CATALOG['system-yard'],
        ROUTE_CATALOG['system-map'],
      ],
    },
    {
      label: 'Define',
      items: [
        // Workflows is the single UI surface for Workflows: the
        // read-only catalog that also links into the authoring
        // routes (/system/workflows*). The separate "Job kinds"
        // sidebar entry was dropped — authoring is reached FROM
        // Workflows, not its own sidebar row.
        ROUTE_CATALOG.workflows,
        ROUTE_CATALOG['system-subjects'],
        ROUTE_CATALOG['system-step-plugins'],
        ROUTE_CATALOG['system-dispatcher'],
        ROUTE_CATALOG.policy,
      ],
    },
    {
      label: 'Evolve',
      items: [
        ROUTE_CATALOG['system-experiments'],
        ROUTE_CATALOG['system-design'],
        ROUTE_CATALOG['system-feedback'],
      ],
    },
    {
      label: 'Platform',
      items: [ROUTE_CATALOG['system-kb'], ROUTE_CATALOG['auth-admin']],
    },
  ];

  // Home — personal work, whichever domain it belongs to. The
  // role-keyed Work list lives here rather than being repeated in
  // every app: "what am I meant to be doing" is one question, and its
  // answer crosses CRM, Operations and Finance freely.
  const HOME_GROUPS = $derived<ReadonlyArray<NavGroup>>([
    WORK,
    {
      label: 'Mine',
      items: [
        // Permkey-less: /` is App.svelte's `me` route, which has no
        // catalog entry (it is everyone's, ungated).
        { id: 'my-day', label: 'My Day', path: '/' },
        ROUTE_CATALOG.inbox,
        ROUTE_CATALOG.views,
        ROUTE_CATALOG.schedule,
        ROUTE_CATALOG.exec,
      ],
    },
  ]);

  let MAIN = $derived<ReadonlyArray<NavGroup>>(
    activeApp === 'it'
      ? IT_GROUPS
      : activeApp === 'home'
        ? HOME_GROUPS
        : [
            {
              label: appGroupLabel(activeApp),
              items: (APP_SURFACES[activeApp] ?? []).map(
                (r: RouteName) => ROUTE_CATALOG[r],
              ),
            },
          ],
  );

  // A surface is in-perspective when its catalog `app` matches the
  // app this shell is rendering. One comparison against one field —
  // where this used to be a MODEL_ROUTES set here that had to agree
  // with a MODEL_KINDS set in App.svelte, keyed off a different
  // vocabulary (RouteName vs Route['kind']).
  function inPerspective(i: NavItem): boolean {
    // A permKey-less NavItem (e.g. a plain sub-page link like Audit
    // Log / Atlas) carries no app of its own — it belongs to whatever
    // group it's placed in, so it's always in-perspective.
    if (i.permKey === undefined) return true;
    return (ROUTE_CATALOG[i.permKey]?.app ?? 'home') === activeApp;
  }

  function visible(items: ReadonlyArray<NavItem>): ReadonlyArray<NavItem> {
    if (!role) return [];
    return items.filter((i) => {
      const policyOk = i.permKey === undefined || canSeeRoute(role, i.permKey);
      const moduleOk = i.module === undefined || moduleEnabled(i.module);
      return policyOk && moduleOk && inPerspective(i);
    });
  }

  function onLinkClick(e: MouseEvent, path: string) {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(path);
  }
</script>

<div class="app-shell">
  <aside class="shell-sidebar">
    <nav class="shell-nav">
      {#if perspective === 'user'}
        <div class="shell-nav-personal">
          <a
            href="/ux/me"
            class="shell-nav-item shell-nav-home {activeSection === 'me' ? 'shell-nav-item-active' : ''}"
            onclick={(e) => onLinkClick(e, '/ux/me')}
          >
            My Day
          </a>
          <a
            href="/ux/inbox"
            class="shell-nav-item {activeSection === 'inbox' ? 'shell-nav-item-active' : ''}"
            onclick={(e) => onLinkClick(e, '/ux/inbox')}
          >
            Inbox
            {#if unreadDirect > 0}
              <span
                class="shell-nav-badge"
                title="{unreadDirect} message{unreadDirect === 1 ? '' : 's'} addressed to you"
                aria-label="{unreadDirect} unread message{unreadDirect === 1 ? '' : 's'} addressed to you"
              >{unreadDirect}</span>
            {/if}
          </a>
          <a
            href="/ux/shop"
            class="shell-nav-item {activeSection === 'shop' ? 'shell-nav-item-active' : ''}"
            onclick={(e) => onLinkClick(e, '/ux/shop')}
          >
            Shop
          </a>
        </div>
      {/if}

      {#each MAIN as group (group.label)}
        {@const items = visible(group.items)}
        {#if items.length > 0}
          <div class="shell-nav-group">
            <div class="shell-nav-group-label">
              <span class="shell-nav-group-chevron">▾</span>
              {group.label}
            </div>
            {#each items as item (item.id)}
              <a
                href={item.path}
                class="shell-nav-item {activeSection === item.id ? 'shell-nav-item-active' : ''}"
                onclick={(e) => onLinkClick(e, item.path)}
              >
                {getLabel(`nav.${item.id}_label`, item.label)}
              </a>
            {/each}
          </div>
        {/if}
      {/each}
    </nav>


    <div class="shell-sidebar-footer">
      {#if user}
        <div class="shell-user">
          <div class="shell-user-name">{user.name}</div>
          <div class="shell-user-role">{user.role}</div>
        </div>
      {/if}
    </div>
  </aside>

  <div class="shell-main">
    <!-- Demo-mode persona switcher — fixed-positioned (bottom-left),
         so it renders here but floats independently of the layout.
         The system-time + sign-in chrome moved up to the perspective
         tab bar; the old topbar is gone. -->
    <div class="shell-content">
      {@render children()}
    </div>
  </div>
</div>
