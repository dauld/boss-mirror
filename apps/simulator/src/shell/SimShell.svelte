<script lang="ts">
  // App shell for the Simulator UX. It IS the apps/web shell — the same
  // .app-shell / .shell-sidebar / .shell-nav-item rules, from the web
  // stylesheet the Simulator imports (backlog 6f471ff6, car 4) — so the
  // Simulator tab is the same product as the others. It used to mirror
  // that shell's shape with a warm-stone copy of its old dark sidebar,
  // which stayed dark and stone after the web shell went Enamel. The
  // sidebar nav is the simulator's own (Cockpit | Controls); it
  // intentionally does NOT mirror the User Experiences Work / Surfaces /
  // Knowledge-Bases grouping. New sim surfaces slot in as more NAV entries.
  import PerspectiveTabs from '@boss/web-kit/PerspectiveTabs.svelte';
  import { loadManifest } from '@boss/web-kit/session/manifest.svelte';
  import { onMount } from 'svelte';
  import { href, navigate, type Route } from '../router';

  // The Simulator is a separate SPA and never loaded the manifest —
  // it did not need to while its chrome carried a hardcoded brand.
  // Now that the brand is tenant data, this app has to fetch it too or
  // its bar would read "BOSS" while every other app reads the tenant's
  // name. Same endpoint, same store; the gateway serves it to both.
  onMount(loadManifest);

  let { route, children } = $props<{
    route: Route;
    children: () => unknown;
  }>();

  type NavItem = Readonly<{ label: string; rel: string; kind: Route['kind'] }>;
  const NAV: ReadonlyArray<NavItem> = [
    { label: 'Cockpit', rel: '/', kind: 'cockpit' },
    { label: 'Controls', rel: '/controls', kind: 'controls' },
  ];

  function go(e: MouseEvent, rel: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(href(rel));
  }
</script>

<div class="app-shell">
  <!-- Brand comes from the tenant manifest; the Simulator app has no
       Subject kinds of its own, so search here is unscoped by design
       rather than by omission. -->
  <PerspectiveTabs active="simulator" searchAppKinds={[]} />
  <aside class="shell-sidebar">
    <nav class="shell-nav" aria-label="Simulator sections">
      <div class="shell-nav-group">
        <div class="shell-nav-group-label">Simulator</div>
        {#each NAV as item (item.kind)}
          <a
            class="shell-nav-item"
            class:shell-nav-item-active={route.kind === item.kind}
            href={href(item.rel)}
            aria-current={route.kind === item.kind ? 'page' : undefined}
            onclick={(e) => go(e, item.rel)}
          >{item.label}</a>
        {/each}
      </div>
    </nav>
  </aside>

  <main class="shell-main sim-main">
    {@render children()}
  </main>
</div>

<style>
  .sim-main {
    padding: 28px max(24px, calc((100vw - 200px - 1180px) / 2)) 64px;
  }
</style>
