<script lang="ts">
  // THE IT WORLD — the /it landing. Since design d2154293 (decided by
  // David 2026-09-19; this is car 1) it is ONE SVG world: world.ts lays
  // the regions out as territories along the packet flow and
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
  // CLICKING SWAPS THE VIEW (David, 2026-09-20; backlog ca37478f).
  // /it/yard/<region> is this same page with `region` set, and it
  // renders the REGION's map in place of the world's — not the world
  // drawn nearer. Car 3 read "zoom into the region by clicking to see"
  // (feedback c3105b2a) as a camera walking the viewBox into a
  // territory's rect; David's correction: "I just wanted the world map
  // view to get replaced with the more detailed region map view on
  // click but not literally increase the size of content on the world
  // map." The route was already right — only what it drew was wrong,
  // and the camera is gone. The floor's
  // panels — the alerts, the departure board, the entity deck and its
  // verbs, production and signals — mount UNDER the region map as
  // FloorDeck. The region map is fed by the scene FloorDeck ALREADY
  // reads, handed up through `onfloor`: one read of the floor on the
  // page, not two — the same rule that made the regions endpoint the
  // only reading of a region.
  //
  // ONE MAP PER FLOOR (design fe77a1d2). Car 2: the region map draws
  // its region's slice of the floor and the deck draws no map at all,
  // so the selection that drives the entity panel crosses between the
  // two: the deck hands up what is selected and the function that
  // selects (`onselection`), and a click on the region map goes
  // through that function — one selection, whichever surface took it.
  // Car 3: the deck was still the Train Yard page (YardPage), mounted
  // with its header switched off — a page in name only, with this as
  // its one mount site — so it became the FloorDeck component and the
  // page was deleted. Every route is unchanged.
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
  import { hasInterior } from './region-contents';
  import RegionMap from './RegionMap.svelte';
  import { hasPlatforms, type Deck } from './world-interior';
  import type { FloorSelection, Scene } from './yard-floor';
  import { fetchBorders, type Borders } from './borders';
  import type { LastGood } from './hud';
  import HudFrame from './HudFrame.svelte';
  import WorldMap from './WorldMap.svelte';
  import FloorDeck from './FloorDeck.svelte';
  import CrewBoardPage from '../crew/CrewBoardPage.svelte';
  import ReceivingYardPage from '../receiving/ReceivingYardPage.svelte';
  import MarshallingYardPage from '../marshalling/MarshallingYardPage.svelte';
  import { MediaQuery } from 'svelte/reactivity';
  import PhoneStrip from './PhoneStrip.svelte';
  import { PHONE_QUERY } from './phone-strip';

  type Props = Readonly<{
    /** The territory the camera is in — the `/it/yard/<region>` route.
     *  Absent at `/it`, which is the whole world. */
    region?: string | null;
  }>;
  let { region = null }: Props = $props();

  /** A region the layout does not know leaves the WORLD on screen
   *  rather than swapping to a map of nothing. */
  const shown = $derived(region !== null && territoryOf(region) !== undefined ? region : null);
  /** The six regions whose floor is the yard's own. */
  const floorRegion = $derived(shown !== null && hasInterior(shown) ? shown : null);
  /** The two whose floor is a queue board — receiving and marshalling
   *  (car 4). Their map draws PLATFORMS rather than wagons in transit,
   *  and the board itself mounts under it the way the yard's floor
   *  does. */
  const platformRegion = $derived(shown !== null && hasPlatforms(shown) ? shown : null);
  /** The floor, handed up by the yard page below — the scene its own
   *  reads already built. Null until the first read lands. */
  let floor = $state<Scene | null>(null);
  /** The floor's selection, handed up by the same page — null until it
   *  has mounted, when the map selects nothing and a click is dropped
   *  rather than aimed at a panel that is not there. */
  let selection = $state<FloorSelection | null>(null);
  /** The platform deck, handed up by whichever queue board is mounted,
   *  WITH the region it was read for: a deck left over from the region
   *  just left would draw the wrong queues for a moment. */
  let held = $state<Readonly<{ region: string; deck: Deck }> | null>(null);
  const deck = $derived<Deck | null>(
    platformRegion !== null && held !== null && held.region === platformRegion ? held.deck : null,
  );
  /** A phone: the world is drawn as the strip map rather than the SVG
   *  shrunk (design 62de32ae decision 12, car G). ONE of the two is
   *  mounted, never both hidden by CSS — a hidden world would still be
   *  read, counted and crawled as if it were on the screen. */
  const phone = new MediaQuery(PHONE_QUERY);

  let regions = $state<Remote<Regions>>({ kind: 'loading' });
  let borders = $state<Remote<Borders>>({ kind: 'loading' });
  let readAt = $state<number | null>(null);
  /** The newest regions read that succeeded, and when — the HUD's
   *  "last good HH:MMZ" when a later read fails (design 00774ca8
   *  decision 5). Its VALUES are never drawn once a newer read failed. */
  let lastGood = $state<LastGood | null>(null);

  onMount(() => {
    let cancelled = false;
    async function tick() {
      // Two reads, concurrently — they are independent, and the map
      // draws its territories even while the rails are still coming.
      const [r, b] = await Promise.all([fetchRegions(), fetchBorders()]);
      if (cancelled) return;
      regions = r;
      borders = b;
      readAt = Date.now();
      if (r.kind === 'ready') lastGood = { at: readAt, data: r.data };
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
    subtitle="The territories along the packet flow, each a door to its floor, and the borders between them carrying what crosses, what waits and the machine that moves it"
  />

  <!-- THE HUD FRAME (design 00774ca8): the whole system, one row per
       third, above the map — and above every region's map too, because
       it does not follow the zoom. It stands whatever the read did: a
       failed read turns its cells to `?` rather than taking the frame
       away. -->
  <HudFrame read={regions} {readAt} {lastGood} />

  {#if regions.kind === 'loading'}
    <div class="yard-empty">Reading the regions…</div>
  {:else if regions.kind === 'failed'}
    <!-- A failed read is said, in the one class the outage crawl reads:
         a map that could not be read must not look like a clear one. -->
    <div class="yard-empty load-failed">The regions cannot be read — {regions.error}</div>
  {:else}
    {#if shown !== null}
      <!-- THE REGION'S OWN MAP, in place of the world's. Its canvas is
           its own, so what it shows is laid out for the region rather
           than for the slot its rectangle occupies on the world line. -->
      <!-- A MAP THAT CANNOT BE DRAWN SAYS SO (backlog 846ab934). On
           2026-09-24 the shop floor threw each_key_duplicate on live
           data and, with nothing to catch it, the whole page stayed on
           "Reading the regions…" — a loading line over a crash, the
           answer-instead-of-an-error class. The boundary turns any
           render failure of the region map into the failure line the
           outage crawl reads, naming the error. -->
      <svelte:boundary>
        <RegionMap
          region={shown}
          regions={regions.data}
          {floor}
          {deck}
          selected={selection?.selected ?? ''}
          onselect={(key) => selection?.select(key)}
          onleave={() => navigate('/it')} />
        {#snippet failed(error)}
          <div class="yard-empty load-failed" data-region={shown}>
            The {shown} region cannot be drawn — {error instanceof Error ? error.message : String(error)}
          </div>
        {/snippet}
      </svelte:boundary>
    {:else if phone.current}
      <PhoneStrip
        regions={regions.data}
        borders={borders.kind === 'ready' ? borders.data : null} />
    {:else}
      <WorldMap
        regions={regions.data}
        borders={borders.kind === 'ready' ? borders.data : null} />
    {/if}
    <!-- A failed rails read is SAID — the territories are still drawn,
         but a map whose rails could not be read must not look like a
         quiet one. The one-line activity summary that stood here summed
         the border rows on the client and double-counted about 260
         backlog items; the HUD frame above replaced it (design 00774ca8
         decision 10). -->
    {#if borders.kind === 'failed'}
      <div class="yard-empty load-failed">The borders cannot be read — {borders.error}</div>
    {/if}
    <div class="yard-flow">
      window {regions.data.window_hours}h against the {regions.data.window_hours}h before{readAt !== null ? ` · read ${clock(readAt)}` : ''}
    </div>
  {/if}

  {#if floorRegion !== null}
    <!-- THE FLOOR'S DECK, under the region's map: the Train Yard's
         panels, a component of this page rather than a page of their
         own (design fe77a1d2, car 3). Keyed on the region so a move
         from one to another opens the new region's panel rather than
         keeping the old selection. -->
    {#key floorRegion}
      <FloorDeck
        focus={floorRegion}
        onfloor={(s) => (floor = s)}
        onselection={(s) => (selection = s)} />
    {/key}
  {/if}

  {#if platformRegion !== null}
    <!-- THE QUEUE BOARD, under the region's map (car 4).
         The two /it/operate pages this replaced are the SAME
         components, mounted here with their page header dropped: the
         territory above draws the platforms from the very reads these
         make, so nothing is read twice and nothing is derived twice. -->
    {#key platformRegion}
      {#if platformRegion === 'receiving'}
        <ReceivingYardPage embedded ondeck={(d) => (held = { region: 'receiving', deck: d })} />
      {:else if platformRegion === 'shop-floor'}
        <CrewBoardPage embedded ondeck={(d) => (held = { region: 'shop-floor', deck: d })} />
      {:else}
        <MarshallingYardPage embedded ondeck={(d) => (held = { region: 'marshalling', deck: d })} />
      {/if}
    {/key}
  {/if}
</div>

<style>
  /* The yard's classes, as FloorDeck.svelte declares them (Svelte scopes
     a component's styles, so the page carries its own copy of the ones
     it uses — same names, nothing new). The colours are the map's own
     --map-* tokens, with no fallback (42f66fb3, map-palette.test.ts). */
  .yard-root { padding: 0 32px 32px; }
  /* On a phone the shell's own 16px gutter is the page's (styles.css,
     car G); this page's 32px on top of it left the strip 294px of 390. */
  @media (max-width: 720px) { .yard-root { padding: 0 0 24px; } }
  .yard-empty { color: var(--map-muted); padding: 12px 0; font-size: 14px; }
  .yard-flow { font-family: var(--font-mono); font-size: 11px;
    letter-spacing: var(--ls-nav); color: var(--map-muted);
    border-top: 1px solid var(--map-rule); margin-top: 28px; padding-top: 12px; }
</style>
