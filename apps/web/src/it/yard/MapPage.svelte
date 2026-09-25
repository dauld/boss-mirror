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
  // A REGION OWNS ITS PAGE (design 62de32ae, decision 7). The view
  // swapped and the page around it did not: the heading still read "The
  // IT world" inside every region, and the world's summary line sat
  // between the region map and its floor (review 2026-09-24, finding
  // 8). On a region the heading is the region's — "IT · Dock", under a
  // breadcrumb back to the world — and the summary line gives way to
  // the region's own rails in and out (region-page.ts), from the same
  // borders read. The deck scopes its board and alerts the same way.
  //
  // THE DEPARTMENT MAP (design e765b3fc, car N1; David 2026-09-25):
  // "the operating map sits at the top always and clicking stations or
  // lines pulls up detail below". At /it a click SELECTS — `/it?at=<name>`
  // (regions.ts `regionHref`) — and the selection's panel opens under
  // the map, which is not torn down: the route is the same one with a
  // query, and App.svelte mounts this page once for both. Car N1 opened
  // the panel as a shell (the station's one number, its state and the
  // door to its floor page). Car N2 fills it: a station's or a section's
  // readings (`?at=dock->track` selects a section) and a station's floor
  // cards, while the map marks what is selected and stops writing the
  // detail itself. Car N3 retires the floor pages, the `region` view
  // below with them.
  //
  // NO NEW STYLING (the visual redesign reskins): the world is drawn in
  // the yard's own strokes and tokens.
  //
  // Polling stays as the yard's: a 10s tick. Reads go through
  // fetchRemote, so an outage renders a failure line (`load-failed`),
  // never an empty map that reads as a calm one.
  import { onMount, untrack } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import { navigate } from '@boss/web-kit/nav';
  import { railLines, regionTitle } from './region-page';
  import type { Remote } from '../../data/remote';
  import { countText, fetchRegions, floorHref, lampOf, stateText, type Regions } from './regions';
  import { markOf, selectionOf, type MapSelection } from './selection';
  import { sectionCells, stationCells, type PanelCell } from './panel';
  import { territoryOf } from './world';
  import { hasInterior } from './region-contents';
  import RegionMap from './RegionMap.svelte';
  import { hasPlatforms, type Deck } from './world-interior';
  import type { FloorSelection, Scene } from './yard-floor';
  import { fetchBorders, type Borders } from './borders';
  import type { LastGood } from './hud';
  import HudFrame from './HudFrame.svelte';
  import WorldMap from './WorldMap.svelte';
  import TransitMap from './TransitMap.svelte';
  import { flightOn } from '@boss/web-kit/session/flights.svelte';
  import PlantStrip from './PlantStrip.svelte';
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
    /** The Department Map's selection — `/it?at=<name>`. Read only on
     *  the map itself; a region's own view selects nothing. */
    at?: string;
  }>;
  let { region = null, at = undefined }: Props = $props();

  /** A region the layout does not know leaves the WORLD on screen
   *  rather than swapping to a map of nothing. */
  const shown = $derived(region !== null && territoryOf(region) !== undefined ? region : null);
  /** The regions whose floor is a queue board — receiving, marshalling
   *  and the shop floor (car 4). Their map draws PLATFORMS rather than wagons in transit,
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
  /** THE TRANSIT MONITOR (design 16091dfb, Q2 decided 2026-09-25):
   *  behind its flight, on for David first. Off — and for every viewer
   *  the flight does not list — the world map is the map; the phone
   *  keeps its strip either way. */
  const transit = $derived(flightOn('it-map-transit'));

  let regions = $state<Remote<Regions>>({ kind: 'loading' });
  let borders = $state<Remote<Borders>>({ kind: 'loading' });
  let readAt = $state<number | null>(null);
  /** When the rails were last read WELL — the moving map's held clocks
   *  count on from it, and it greys past three missed reads (design
   *  31bade8f decision 6). */
  let bordersAt = $state<number | null>(null);
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
      if (b.kind === 'ready') bordersAt = readAt;
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

  /** What `?at=` selects, against the regions the server served — none
   *  while the read is out, and none on a region's own view. */
  const picked = $derived<MapSelection>(
    shown === null && regions.kind === 'ready'
      ? selectionOf(at, regions.data, borders.kind === 'ready' ? borders.data : null)
      : { kind: 'none' },
  );
  /** The panel's cells (design e765b3fc, car N2): every reading the map
   *  no longer writes, for the one station or section selected. */
  const cells = $derived<ReadonlyArray<PanelCell>>(
    picked.kind === 'station'
      ? stationCells(picked.region, borders.kind === 'ready' ? borders.data : null, regions.kind === 'ready' ? regions.data.now : '')
      : picked.kind === 'section'
        ? sectionCells(picked.border, borders.kind === 'ready' ? borders.data.now : '')
        : [],
  );

  /** The panel sits under the map, so on a short screen it can open
   *  below the fold and a click would look like it did nothing. Brought
   *  into view when the SELECTION changes — keyed on `at`, not on the
   *  10 s poll, which must never pull the page back down. */
  let panel = $state<HTMLElement | null>(null);
  $effect(() => {
    const key = at;
    const el = panel;
    if (key !== undefined && el !== null) untrack(() => el.scrollIntoView({ block: 'nearest' }));
  });

  function go(e: MouseEvent, href: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(href);
  }
</script>

<div class="theme-exec yard-root">
  {#if shown !== null}
    <!-- The region's own heading, under the way back to the world it
         was opened from — the page says where you are. -->
    <nav class="crumbs" aria-label="breadcrumb" data-region={shown}>
      <Breadcrumb to="/it">Department Map</Breadcrumb>
      <span class="crumb-here" aria-current="page">› {regionTitle(shown)}</span>
    </nav>
    <PageHeader title={`IT · ${regionTitle(shown)}`} />
  {:else}
    <!-- Named what its sidebar row is named (design e765b3fc, car N1):
         it answered to "Train Yard", "The IT world" and "IT · Forge
         line" at once (page audit gap f9850601). -->
    <PageHeader
      eyebrow="IT"
      title="Department Map"
      subtitle={transit
        ? 'The network as a transit monitor: each region a station on its line, each border a section of track with what waits on it, and the alarms board beside it. Select a station or a section to open its detail below the map.'
        : 'The territories along the packet flow, and the borders between them carrying what crosses, what waits and the machine that moves it. Select a territory to open its detail below the map.'}
    />
  {/if}

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
    {:else if transit}
      <!-- The same two reads, drawn as a transit map (design 16091dfb):
           stations, sections and the alarms board, with the selection
           marked on it (design e765b3fc, car N2). -->
      <TransitMap
        regions={regions.data}
        borders={borders.kind === 'ready' ? borders.data : null}
        selected={markOf(picked)} />
      <PlantStrip machines={regions.data.plant} />
    {:else}
      <WorldMap
        regions={regions.data}
        borders={borders.kind === 'ready' ? borders.data : null}
        {bordersAt} />
      <!-- THE PLANT along the world's edge (design 62de32ae, decision
           11): the host runners serve every region, so they stand under
           the territories rather than in one of them. -->
      <PlantStrip machines={regions.data.plant} />
    {/if}
    {#if picked.kind !== 'none'}
      <!-- THE SELECTION'S PANEL, under the map (design e765b3fc). Car N1
           opened it as a shell; car N2 fills it with what the map no
           longer writes — a station's or a section's rate, what waits
           and on whom, what is stuck, its trend, the band and the why
           that decided its state, its machines and its crossings
           (panel.ts) — and, for a station, what stands in it: its
           floor's cards, which used to be a page of their own. A name
           the read did not carry is said, never drawn as a quiet empty
           panel. -->
      {@const name = picked.kind === 'station' ? picked.name : picked.kind === 'section' ? picked.key : picked.at}
      {@const state = picked.kind === 'station' ? picked.region.state : picked.kind === 'section' ? (picked.border?.state ?? 'unknown') : 'unknown'}
      <section
        class="map-panel"
        bind:this={panel}
        data-map-panel
        data-selection={name}
        data-kind={picked.kind}
        data-state={state}
        aria-label="the {name} selection">
        <header class="panel-head">
          {#if state !== 'unknown'}
            <span class="panel-lamp {lampOf(state)}" aria-hidden="true"></span>
          {/if}
          <span class="panel-kind">{picked.kind === 'station' ? 'Station' : picked.kind === 'section' ? 'Section' : 'Selection'}</span>
          <h2 class="panel-title">{picked.kind === 'unknown' ? name : picked.title}</h2>
          <a class="panel-close" data-close href="/it" aria-label="close the {name} selection"
            onclick={(e) => go(e, '/it')}>close</a>
        </header>
        {#if picked.kind === 'unknown'}
          <p class="panel-none">Nothing on this map is named “{name}”.</p>
        {:else}
          {#if picked.kind === 'station'}
            <div class="panel-reading">
              <span class="panel-figure" data-figure>{countText(picked.region)}</span>
              <span class="panel-state">{stateText(picked.region)}</span>
            </div>
          {/if}
          <div class="panel-cells">
            {#each cells as c (c.field)}
              <div class="panel-cell" data-field={c.field}>
                <h3 class="cell-label">{c.label}</h3>
                <ul class="cell-lines">
                  {#each c.lines as line, i (i)}
                    <li>{line}</li>
                  {/each}
                </ul>
              </div>
            {/each}
          </div>
          {#if picked.kind === 'station'}
            <!-- The floor page stays reachable until car N3 retires it;
                 what it held is below. -->
            <a class="panel-floor" data-floor href={floorHref(picked.name)}
              onclick={(e) => go(e, floorHref(picked.name))}>open the page for {picked.title} →</a>
            <div class="panel-contents" data-contents={picked.name}>
              {@render contents(picked.name)}
            </div>
          {/if}
        {/if}
      </section>
    {/if}
    <!-- A failed rails read is SAID — the territories are still drawn,
         but a map whose rails could not be read must not look like a
         quiet one. The one-line activity summary that stood here summed
         the border rows on the client and double-counted about 260
         backlog items; the HUD frame above replaced it (design 00774ca8
         decision 10). -->
    {#if borders.kind === 'ready' && shown !== null}
      <!-- ON A REGION, ITS OWN RAILS (design 62de32ae decision 7): what
           comes in, what goes out, what waits at each and the machine
           that moves it — scoped to the borders this region has. -->
      <div class="yard-flow region-rails" data-rails={shown}>
        {#each railLines(borders.data, shown) as line, i (i)}
          <div class="rail-line">{line}</div>
        {/each}
      </div>
    {:else if borders.kind === 'failed'}
      <div class="yard-empty load-failed">The borders cannot be read — {borders.error}</div>
    {/if}
    <div class="yard-flow">
      window {regions.data.window_hours}h against the {regions.data.window_hours}h before{readAt !== null ? ` · read ${clock(readAt)}` : ''}
    </div>
  {/if}

  {#if shown !== null}
    {@render contents(shown)}
  {/if}
</div>

<!-- WHAT STANDS IN A REGION — its floor's cards. Under the region's own
     map at /it/yard/<region>, and inside a station's panel on the
     Department Map (design e765b3fc, car N2: "region floor cards move
     into the station panel"), until car N3 retires the region pages and
     the panel is the one place they are drawn. -->
{#snippet contents(region: string)}
  {#if hasInterior(region)}
    <!-- THE FLOOR'S DECK: the Train Yard's panels, a component of this
         page rather than a page of their own (design fe77a1d2, car 3).
         Keyed on the region so a move from one to another opens the new
         region's panel rather than keeping the old selection. -->
    {#key region}
      <FloorDeck
        focus={region}
        onfloor={(s) => (floor = s)}
        onselection={(s) => (selection = s)} />
    {/key}
  {:else if hasPlatforms(region)}
    <!-- THE QUEUE BOARD (car 4). The two /it/operate pages this replaced
         are the SAME components, mounted here with their page header
         dropped: the territory above draws the platforms from the very
         reads these make, so nothing is read twice and nothing is
         derived twice. -->
    {#key region}
      {#if region === 'receiving'}
        <ReceivingYardPage embedded ondeck={(d) => (held = { region: 'receiving', deck: d })} />
      {:else if region === 'shop-floor'}
        <CrewBoardPage embedded ondeck={(d) => (held = { region: 'shop-floor', deck: d })} />
      {:else}
        <MarshallingYardPage embedded ondeck={(d) => (held = { region: 'marshalling', deck: d })} />
      {/if}
    {/key}
  {/if}
{/snippet}

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
  .region-rails .rail-line + .rail-line { margin-top: 4px; }
  .crumbs { font-size: 13px; color: var(--map-muted); padding-top: 16px; }
  .crumb-here { margin-left: 4px; color: var(--map-ink); }
  /* The selection's panel — a departure-board plate under the map, in the
     crossing panel's grammar (WorldMap.svelte): a hairline frame, a mono
     uppercase head with the state's lamp, the figure large beneath. */
  .map-panel { margin-top: var(--s3); border: 1px solid var(--map-rule-strong);
    border-top-width: 3px; background: var(--map-surface); padding: var(--s2) var(--s3) var(--s3);
    color: var(--map-ink); }
  .map-panel[data-state='attention'] { border-top-color: var(--map-warn-edge); }
  .map-panel[data-state='troubled'], .map-panel[data-state='unknown'] { border-top-color: var(--map-bad-edge); }
  .panel-head { display: flex; align-items: center; gap: var(--s2); font-family: var(--font-mono);
    font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; }
  .panel-kind { color: var(--map-muted); }
  .panel-title { margin: 0; font: inherit; font-weight: 600; color: var(--map-ink); }
  .panel-lamp { width: 8px; height: 8px; border-radius: 50%; background: var(--map-rule-strong); }
  .panel-lamp.ok { background: var(--map-ok-edge); }
  .panel-lamp.warn { background: var(--map-warn-edge); }
  .panel-lamp.err { background: var(--map-bad-edge); }
  .panel-close { margin-left: auto; color: var(--map-muted); border: 1px solid var(--map-rule);
    padding: 2px 8px; text-decoration: none; }
  .panel-close:hover, .panel-close:focus-visible { color: var(--map-ink); border-color: var(--map-ink); }
  .panel-reading { display: flex; align-items: baseline; flex-wrap: wrap; gap: var(--s2) var(--s3);
    margin: var(--s2) 0; }
  .panel-figure { font-family: var(--font-mono); font-size: 22px; font-variant-numeric: tabular-nums; }
  .panel-state { font-family: var(--font-mono); font-size: 12px; color: var(--map-muted); }
  .map-panel[data-state='attention'] .panel-state { color: var(--map-warn-ink); }
  .map-panel[data-state='troubled'] .panel-state { color: var(--map-bad-ink); }
  .panel-floor { font-size: 13px; color: var(--map-link); }
  /* The readings (car N2): a board of cells, each a mono label over the
     server's own sentences. The long lists — what waits, and the
     crossings — take the whole row, so a hold sentence is never
     squeezed into a column; `dense` lets the short cells close up the
     row a long one leaves behind. On a phone every cell is a row. */
  .panel-cells { display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); grid-auto-flow: dense;
    gap: var(--s2); margin: var(--s2) 0 var(--s3); }
  .panel-cell { background: var(--map-bg); border: 1px solid var(--map-rule); padding: var(--s2); min-width: 0; }
  .panel-cell[data-field='waiting'], .panel-cell[data-field='crossings'] { grid-column: 1 / -1; }
  .cell-label { margin: 0 0 4px; font-family: var(--font-mono); font-size: 11px; font-weight: 400;
    letter-spacing: 0.08em; text-transform: uppercase; color: var(--map-muted); }
  .cell-lines { margin: 0; padding: 0; list-style: none; font-size: 13px; line-height: 1.45; color: var(--map-ink);
    overflow-wrap: anywhere; }
  .cell-lines li + li { margin-top: 2px; }
  .panel-contents { margin-top: var(--s3); border-top: 1px solid var(--map-rule); }
  .panel-none { margin: var(--s2) 0 0; font-size: 13px; color: var(--map-bad-ink); overflow-wrap: anywhere; }
</style>
