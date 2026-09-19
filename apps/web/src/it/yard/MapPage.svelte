<script lang="ts">
  // THE IT WORLD — the /it landing. Since design d2154293 (decided by
  // David 2026-09-19; this is car 1) it is ONE SVG world: world.ts lays
  // the eight regions out as territories along the packet flow and
  // WorldMap.svelte draws them, each with its count, state and trend
  // inside its outline and the why on a troubled one. The 4x2 card
  // grid that stood here (0524fc95 car 2) had no shared coordinate
  // space, no adjacency and no flow — "a world map in a video game
  // where the physical layout and connections make sense" (feedback
  // c3105b2a) is the ask, and the world is becoming THE IT surface
  // (Q2): the floors zoom in on car 3, receiving and marshalling grow
  // interiors on car 4.
  //
  // Every number is read from ONE endpoint, /api/yard/regions, and the
  // page derives nothing — it used to be the other way: yard.ts read
  // six endpoints and derived every number the Train Yard showed, boss
  // orient derived the same numbers again, and nothing held the two
  // equal. Clicking a territory still opens /it/yard/<region>, the
  // floor that already exists, one click deeper.
  //
  // NO NEW STYLING (the visual redesign reskins): the world is drawn in
  // the yard's own strokes and tokens.
  //
  // Polling stays as the yard's: a 10s tick. Reads go through
  // fetchRemote, so an outage renders a failure line (`load-failed`),
  // never an empty map that reads as a calm one.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import type { Remote } from '../../data/remote';
  import { fetchRegions, type Regions } from './regions';
  import WorldMap from './WorldMap.svelte';

  let regions = $state<Remote<Regions>>({ kind: 'loading' });
  let readAt = $state<number | null>(null);

  onMount(() => {
    let cancelled = false;
    async function tick() {
      const r = await fetchRegions();
      if (cancelled) return;
      regions = r;
      readAt = Date.now();
    }
    void tick();
    const t = setInterval(tick, 10_000);
    return () => {
      cancelled = true;
      clearInterval(t);
    };
  });

  const clock = (ms: number) =>
    new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
</script>

<div class="theme-exec yard-root">
  <PageHeader
    eyebrow="IT · Forge line"
    title="The IT world"
    subtitle="Eight territories along the packet flow, each a door to its floor — the count, whether it is clear, busy or troubled, and this window against the last"
  />

  {#if regions.kind === 'loading'}
    <div class="yard-empty">Reading the regions…</div>
  {:else if regions.kind === 'failed'}
    <!-- A failed read is said, in the one class the outage crawl reads:
         a map that could not be read must not look like a clear one. -->
    <div class="yard-empty load-failed">The regions cannot be read — {regions.error}</div>
  {:else}
    <WorldMap regions={regions.data} />
    <div class="yard-flow">
      window {regions.data.window_hours}h against the {regions.data.window_hours}h before{readAt !== null ? ` · read ${clock(readAt)}` : ''}
    </div>
  {/if}
</div>

<style>
  /* The yard's classes, as YardPage.svelte declares them (Svelte scopes
     a component's styles, so the page carries its own copy of the ones
     it uses — same names, same tokens, nothing new). */
  .yard-root { padding: 0 32px 32px; }
  .yard-empty { color: var(--static, #78716c); padding: 12px 0; font-size: 14px; }
  .yard-flow { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: var(--ls-nav, 0.14em); color: var(--static, #7A838C);
    border-top: 1px solid var(--hairline, #2A3138); margin-top: 28px; padding-top: 12px; }
</style>
