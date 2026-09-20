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
  // equal.
  //
  // CAR 3 — CLICKING IS A ZOOM. /it/yard/<region> is this same page
  // with `region` set: the camera walks into that territory and the
  // territory shows what is moving inside it. Nothing navigates to a
  // separate surface, which is the whole of David's ask (feedback
  // c3105b2a: "zoom into the region by clicking to see"). The floor's
  // panels — the departure board, the entity deck and its verbs —
  // mount UNDER the zoomed world, and they are still the Train Yard's
  // own (YardPage, `embedded`: it drops its page header and its second
  // copy of the map, keeps everything else). The interiors are fed by
  // the scene YardPage ALREADY reads, handed up through `onfloor`: one
  // read of the floor on the page, not two — the same rule that made
  // the regions endpoint the only reading of a region.
  //
  // NO NEW STYLING (the visual redesign reskins): the world is drawn in
  // the yard's own strokes and tokens.
  //
  // Polling stays as the yard's: a 10s tick. Reads go through
  // fetchRemote, so an outage renders a failure line (`load-failed`),
  // never an empty map that reads as a calm one.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { navigate } from '@boss/web-kit/nav';
  import type { Remote } from '../../data/remote';
  import { fetchRegions, type Regions } from './regions';
  import { territoryOf } from './world';
  import { hasInterior } from './world-zoom';
  import { hasPlatforms, type Deck } from './world-interior';
  import type { Scene } from './yard-floor';
  import WorldMap from './WorldMap.svelte';
  import YardPage from './YardPage.svelte';
  import ReceivingYardPage from '../receiving/ReceivingYardPage.svelte';
  import MarshallingYardPage from '../marshalling/MarshallingYardPage.svelte';

  type Props = Readonly<{
    /** The territory the camera is in — the `/it/yard/<region>` route.
     *  Absent at `/it`, which is the whole world. */
    region?: string | null;
  }>;
  let { region = null }: Props = $props();

  /** A region the layout has no territory for leaves the camera at the
   *  world rather than flying to nowhere. */
  const zoomed = $derived(region !== null && territoryOf(region) !== undefined ? region : null);
  /** The six regions whose floor is the yard's own. */
  const floorRegion = $derived(zoomed !== null && hasInterior(zoomed) ? zoomed : null);
  /** The two whose floor is a queue board — receiving and marshalling
   *  (car 4). Their territory draws PLATFORMS rather than wagons in
   *  transit, and the board itself mounts under the zoomed world the
   *  way the yard's floor does. */
  const platformRegion = $derived(zoomed !== null && hasPlatforms(zoomed) ? zoomed : null);
  /** The floor, handed up by the yard page below — the scene its own
   *  reads already built. Null until the first read lands. */
  let floor = $state<Scene | null>(null);
  /** The platform deck, handed up by whichever queue board is mounted,
   *  WITH the region it was read for: a deck left over from the region
   *  the camera just left would draw the wrong queues for a moment. */
  let held = $state<Readonly<{ region: string; deck: Deck }> | null>(null);
  const deck = $derived<Deck | null>(
    platformRegion !== null && held !== null && held.region === platformRegion ? held.deck : null,
  );

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
    <WorldMap regions={regions.data} {zoomed} {floor} {deck} onleave={() => navigate('/it')} />
    <div class="yard-flow">
      window {regions.data.window_hours}h against the {regions.data.window_hours}h before{readAt !== null ? ` · read ${clock(readAt)}` : ''}
    </div>
  {/if}

  {#if floorRegion !== null}
    <!-- THE FLOOR, under the territory the camera is in: the Train
         Yard's own panels, mounted in place rather than on a page of
         their own. Keyed on the region so a move from one territory to
         another opens the new region's panel rather than keeping the
         old selection. -->
    {#key floorRegion}
      <YardPage focus={floorRegion} embedded onfloor={(s) => (floor = s)} />
    {/key}
  {/if}

  {#if platformRegion !== null}
    <!-- THE QUEUE BOARD, under the territory the camera is in (car 4).
         The two /it/operate pages this replaced are the SAME
         components, mounted here with their page header dropped: the
         territory above draws the platforms from the very reads these
         make, so nothing is read twice and nothing is derived twice. -->
    {#key platformRegion}
      {#if platformRegion === 'receiving'}
        <ReceivingYardPage embedded ondeck={(d) => (held = { region: 'receiving', deck: d })} />
      {:else}
        <MarshallingYardPage embedded ondeck={(d) => (held = { region: 'marshalling', deck: d })} />
      {/if}
    {/key}
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
