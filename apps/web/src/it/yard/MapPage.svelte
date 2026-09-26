<script lang="ts" module>
  /** The window the map is PINNED in (design e765b3fc, car N3): wider
   *  than a phone's 720px and at least 900px tall. The map stands about
   *  430px (transit) to 470px (world) at a desktop width, under the 44px
   *  perspective bar, so 900px of window leaves the panel under it about
   *  400px — room for a board's card whole. Measured at 1280×800 with
   *  the map pinned: the panel kept 286px, and a feedback card's Route
   *  button could be scrolled to nowhere visible (Playwright: "element is
   *  outside of the viewport"), so a shorter window scrolls the map with
   *  the page, as a phone does. The one number to move if the maps grow
   *  or shrink. */
  export const PIN_QUERY = '(min-width: 721px) and (min-height: 900px)';
</script>

<script lang="ts">
  // THE DEPARTMENT MAP — the /it landing (design e765b3fc; David,
  // 2026-09-25: "the operating map sits at the top always and clicking
  // stations or lines pulls up detail below"). The map on top — the
  // transit map under its flight, the world map without it, the strip on
  // a phone — and, below it, the panel of whatever `/it?at=<name>`
  // selects (regions.ts `regionHref`): a station or a section of track.
  // A selection is a query on this one route, so the map is mounted once
  // and a click never tears it down.
  //
  // THE MAP IS PINNED (car N3). "Pinned at the top, always visible": the
  // map holds under the perspective bar while the panel scrolls beneath
  // it, so the thing selected and the detail it opened are on the screen
  // together. Where a pinned map would leave the panel no room — a
  // phone, or a window too short (PIN_QUERY) — it scrolls with the page
  // instead, and the panel is brought into view on a selection either
  // way.
  //
  // WHAT A PAGE WAS IS A PANEL NOW (car N3). A region's floor was its own
  // page at /it/yard/<region>, with a second map of its own (design
  // d2154293 car 3, fe77a1d2, 62de32ae decision 7); car N2 drew the
  // floor's cards in the station's panel, and car N3 retired the page,
  // its map and its route. The Crew Board is the shop floor's panel, and
  // four more pages MOVED here before they retired (panel.ts
  // `STATION_BOARDS` is the table): yard status is the track's, dock's
  // and garage's, the conductor's feed the track's, and the feedback and
  // backlog boards receiving's (the backlog marshalling's too). No alias
  // and no redirect: each old path is not found, and its one door is
  // this map.
  //
  // NO NEW STYLING (the visual redesign reskins): the map is drawn in the
  // yard's own strokes and tokens.
  //
  // Every number is read from ONE endpoint, /api/yard/regions (the rails
  // from /api/yard/borders), on the yard's 10s tick, through fetchRemote
  // — so an outage renders a failure line (`load-failed`), never an empty
  // map that reads as a calm one.
  import { onMount, untrack } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { navigate } from '@boss/web-kit/nav';
  import type { Remote } from '../../data/remote';
  import { countText, fetchRegions, lampOf, stateText, type Regions } from './regions';
  import { markOf, selectionOf, unreadStationOf, type MapSelection } from './selection';
  import { boardsOf, sectionCells, stationCells, type PanelCell } from './panel';
  import { hasInterior } from './region-contents';
  import { hasPlatforms } from './world-interior';
  import { fetchBorders, type Borders } from './borders';
  import { fetchRoutes, type Routes } from './routes';
  import type { LastGood } from './hud';
  import HudFrame from './HudFrame.svelte';
  import WorldMap from './WorldMap.svelte';
  import TransitMap from './TransitMap.svelte';
  import { flightOn } from '@boss/web-kit/session/flights.svelte';
  import PlantStrip from './PlantStrip.svelte';
  import FloorDeck from './FloorDeck.svelte';
  import YardStatusPanel from './YardStatusPanel.svelte';
  import ConductorFeed from '../monitoring/ConductorFeed.svelte';
  import CrewBoardPage from '../crew/CrewBoardPage.svelte';
  import ReceivingYardPage from '../receiving/ReceivingYardPage.svelte';
  import MarshallingYardPage from '../marshalling/MarshallingYardPage.svelte';
  import FeedbackTriagePage from '../feedback/FeedbackTriagePage.svelte';
  import BacklogBoardPage from '../backlog/BacklogBoardPage.svelte';
  import { MediaQuery } from 'svelte/reactivity';
  import PhoneStrip from './PhoneStrip.svelte';
  import { PHONE_QUERY } from './phone-strip';

  type Props = Readonly<{
    /** The Department Map's selection — `/it?at=<name>`. Absent, nothing
     *  is selected and no panel opens. */
    at?: string;
  }>;
  let { at = undefined }: Props = $props();

  /** A phone: the world is drawn as the strip map rather than the SVG
   *  shrunk (design 62de32ae decision 12, car G). ONE of the two is
   *  mounted, never both hidden by CSS — a hidden world would still be
   *  read, counted and crawled as if it were on the screen. */
  const phone = new MediaQuery(PHONE_QUERY);
  const pinnable = new MediaQuery(PIN_QUERY);
  /** THE TRANSIT MONITOR (design 16091dfb, Q2 decided 2026-09-25):
   *  behind its flight, on for David first. Off — and for every viewer
   *  the flight does not list — the world map is the map; the phone
   *  keeps its strip either way. */
  const transit = $derived(flightOn('it-map-transit'));

  let regions = $state<Remote<Regions>>({ kind: 'loading' });
  let borders = $state<Remote<Borders>>({ kind: 'loading' });
  /** The routes the map draws (design e765b3fc, car R3): every section,
   *  exit and entry is one the server serves, derived from the
   *  protocols — the page holds no edge of its own. */
  let routes = $state<Remote<Routes>>({ kind: 'loading' });
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
      // Three reads, concurrently — they are independent, and the map
      // draws its stations even while the rails and routes are coming.
      const [r, b, w] = await Promise.all([fetchRegions(), fetchBorders(), fetchRoutes()]);
      if (cancelled) return;
      regions = r;
      borders = b;
      routes = w;
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
   *  while the read is out. */
  const picked = $derived<MapSelection>(
    regions.kind === 'ready'
      ? selectionOf(
          at,
          regions.data,
          borders.kind === 'ready' ? borders.data : null,
          routes.kind === 'ready' ? routes.data : null,
        )
      : { kind: 'none' },
  );
  /** The panel's cells (design e765b3fc, car N2): every reading the map
   *  no longer writes, for the one station or section selected. */
  const cells = $derived<ReadonlyArray<PanelCell>>(
    picked.kind === 'station'
      ? stationCells(picked.region, borders.kind === 'ready' ? borders.data : null, regions.kind === 'ready' ? regions.data.now : '')
      : picked.kind === 'section'
        ? sectionCells(
            picked.border,
            borders.kind === 'ready' ? borders.data.now : '',
            picked.route === null
              ? null
              : {
                  route: picked.route,
                  windowHours: routes.kind === 'ready' ? routes.data.window_hours : 24,
                  bordersRead: borders.kind === 'ready',
                },
          )
        : [],
  );

  /** Whether the map is pinned in this window, and how tall it stands. */
  const pinned = $derived(pinnable.current && !phone.current);
  let pin = $state<HTMLElement | null>(null);
  let pinHeight = $state(0);
  /** The perspective bar the shell fixes above every page
   *  (PerspectiveTabs.svelte: 44px), which the pinned map sits under. */
  const BAR = 44;

  /** WHAT THE PINNED MAP COVERS IS NOT "IN VIEW". While the map is
   *  pinned, the document's scroll padding is the bar and the map, so
   *  whatever brings a control of the panel into view — a keyboard focus
   *  moving down a board, a link to an anchor — lands it under the map
   *  rather than behind it, where it would be focused and hidden. Given
   *  back when the map stops being pinned, or the page goes. It sets a
   *  style and moves nothing, so it may follow the map's height. */
  $effect(() => {
    if (!pinned) return;
    const root = document.documentElement;
    root.style.scrollPaddingTop = `${BAR + pinHeight}px`;
    return () => {
      root.style.scrollPaddingTop = '';
    };
  });

  /** The panel sits under the map, so a click could open it below the
   *  fold — or, with the page scrolled, behind the pinned map — and look
   *  like it did nothing. Brought into view when the SELECTION changes —
   *  keyed on `at`, not on the 10 s poll, which must never move the
   *  page. Under a pinned map the panel's head goes right under the map:
   *  the map's height is MEASURED here, when the panel opens, because the
   *  bound height is still 0 on the first paint of a linked selection. */
  let panel = $state<HTMLElement | null>(null);
  $effect(() => {
    const key = at;
    const el = panel;
    const map = pinned ? pin : null;
    if (key === undefined || el === null) return;
    untrack(() => {
      if (map === null) {
        el.scrollIntoView({ block: 'nearest' });
        return;
      }
      const under = BAR + map.getBoundingClientRect().height;
      window.scrollTo({ top: window.scrollY + el.getBoundingClientRect().top - under });
    });
  });

  function go(e: MouseEvent, href: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(href);
  }

  /** Where the boards of the retired pages live now — said on the page,
   *  so a reader who knew the old tab finds it (David, 2026-09-25: a
   *  newcomer loses nothing). panel.ts `STATION_BOARDS` is the table. */
  const WHERE =
    'Feedback and the IT backlog are in the Receiving station\'s panel (the backlog in Marshalling\'s too), the Crew Board in the Shop floor\'s, and yard status and the conductor\'s activity in the Track\'s, with the Dock\'s and the Garage\'s lanes in theirs.';
</script>

<div class="theme-exec yard-root">
  <!-- Named what its sidebar row is named (design e765b3fc, car N1):
       it answered to "Train Yard", "The IT world" and "IT · Forge
       line" at once (page audit gap f9850601). -->
  <PageHeader
    eyebrow="IT"
    title="Department Map"
    subtitle={(transit
      ? 'The network as a transit monitor: each region a station on its line, each border a section of track with what waits on it, and the alarms board beside it. Select a station or a section to open its detail below the map. '
      : 'The territories along the packet flow, and the borders between them carrying what crosses, what waits and the machine that moves it. Select a territory to open its detail below the map. ') + WHERE}
  />

  <!-- THE HUD FRAME (design 00774ca8): the whole system, one row per
       third, above the map. It stands whatever the read did: a failed
       read turns its cells to `?` rather than taking the frame away. -->
  <HudFrame read={regions} {readAt} {lastGood} />

  {#if regions.kind === 'loading'}
    <div class="yard-empty">Reading the regions…</div>
  {:else if regions.kind === 'failed'}
    <!-- A failed read is said, in the one class the outage crawl reads:
         a map that could not be read must not look like a clear one. -->
    <div class="yard-empty load-failed">The regions cannot be read — {regions.error}</div>
    {@const unread = unreadStationOf(at)}
    {#if unread !== null}
      <!-- A STATION WHOSE READING FAILED STILL OPENS (car N3): its
           readings came with the regions read, so they are said to be
           unread — but what stands in it reads its own endpoints, as the
           floor page it replaced did, and a failed regions read must not
           take those boards down with it. -->
      <section
        class="map-panel"
        bind:this={panel}
        data-map-panel
        data-selection={unread.name}
        data-kind="station"
        data-state="unknown"
        aria-label="the {unread.name} selection">
        <header class="panel-head">
          <span class="panel-kind">Station</span>
          <h2 class="panel-title">{unread.title}</h2>
          <a class="panel-close" data-close href="/it" aria-label="close the {unread.name} selection"
            onclick={(e) => go(e, '/it')}>close</a>
        </header>
        <p class="panel-none">Its readings come with the regions read, which failed; what stands in it reads on its own, below.</p>
        {@render station(unread.name)}
      </section>
    {/if}
  {:else}
    <!-- THE MAP, pinned (car N3) where the window has room for it. -->
    <div class="map-pin" class:pinned data-map-pin={pinned ? 'pinned' : 'scrolls'} bind:this={pin} bind:clientHeight={pinHeight}>
      {#if phone.current}
        <PhoneStrip
          regions={regions.data}
          borders={borders.kind === 'ready' ? borders.data : null} />
      {:else if transit}
        <!-- The same two reads, drawn as a transit map (design 16091dfb):
             stations, sections and the alarms board, with the selection
             marked on it (design e765b3fc, car N2). -->
        <TransitMap
          regions={regions.data}
          routes={routes.kind === 'ready' ? routes.data : null}
          borders={borders.kind === 'ready' ? borders.data : null}
          selected={markOf(picked)} />
      {:else}
        <WorldMap
          regions={regions.data}
          borders={borders.kind === 'ready' ? borders.data : null}
          {bordersAt} />
      {/if}
    </div>
    {#if !phone.current}
      <!-- THE PLANT along the map's edge (design 62de32ae, decision 11):
           the host runners serve every region, so they stand under the
           map rather than in one of its stations. -->
      <PlantStrip machines={regions.data.plant} />
    {/if}
    {#if picked.kind !== 'none'}
      <!-- THE SELECTION'S PANEL, under the map (design e765b3fc). Car N1
           opened it as a shell; car N2 filled it with what the map no
           longer writes — a station's or a section's rate, what waits
           and on whom, what is stuck, its trend, the band and the why
           that decided its state, its machines and its crossings
           (panel.ts) — and, for a station, what stands in it: its
           floor's cards. Car N3 added the boards of the pages that
           retired into it. A name the read did not carry is said, never
           drawn as a quiet empty panel. -->
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
            {@render station(picked.name)}
          {/if}
        {/if}
      </section>
    {/if}
    <!-- A failed rails read is SAID — the territories are still drawn,
         but a map whose rails could not be read must not look like a
         quiet one. -->
    {#if borders.kind === 'failed'}
      <div class="yard-empty load-failed">The borders cannot be read — {borders.error}</div>
    {/if}
    <!-- A failed routes read is said where the routes are drawn (car R3):
         the transit map's stations still stand, and no section is guessed
         in its place. -->
    {#if routes.kind === 'failed' && transit && !phone.current}
      <div class="yard-empty load-failed" data-routes-failed>The routes cannot be read — {routes.error}</div>
    {/if}
    <div class="yard-flow">
      window {regions.data.window_hours}h against the {regions.data.window_hours}h before{readAt !== null ? ` · read ${clock(readAt)}` : ''}
    </div>
  {/if}
</div>

<!-- A STATION'S CONTENTS, under a net. A STATION THAT CANNOT BE DRAWN
     SAYS SO (backlog 846ab934): a board that throws on live data becomes
     the failure line the outage crawl reads, naming the error, and the
     map above it stands. -->
{#snippet station(name: string)}
  <div class="panel-contents" data-contents={name}>
    <svelte:boundary>
      {@render contents(name)}
      {#snippet failed(error)}
        <div class="yard-empty load-failed" data-region={name}>
          The {name} station cannot be drawn — {error instanceof Error ? error.message : String(error)}
        </div>
      {/snippet}
    </svelte:boundary>
  </div>
{/snippet}

<!-- WHAT STANDS IN A STATION — its floor's cards, and the boards of the
     pages that retired into it (design e765b3fc, cars N2 and N3). Keyed
     on the station, so a move from one to another opens the new
     station's boards rather than keeping the old one's selection. -->
{#snippet contents(region: string)}
  {#key region}
    {#if hasInterior(region)}
      <!-- THE FLOOR'S DECK: the Train Yard's panels (design fe77a1d2,
           car 3) — the departure board and the entity panel, whose
           selection the board's own rows drive. -->
      <FloorDeck focus={region} />
    {:else if hasPlatforms(region)}
      <!-- THE QUEUE BOARD (car 4 of d2154293), with its page header
           dropped: the station's panel is its heading. -->
      {#if region === 'receiving'}
        <ReceivingYardPage embedded />
      {:else if region === 'shop-floor'}
        <CrewBoardPage embedded />
      {:else}
        <MarshallingYardPage embedded />
      {/if}
    {/if}
    {#each boardsOf(region) as board (board)}
      <div class="panel-board" data-station-board={board}>
        {#if board === 'yard-status'}
          <YardStatusPanel part={region === 'track' ? 'track' : region === 'dock' ? 'dock' : 'garage'} />
        {:else if board === 'conductor'}
          <ConductorFeed />
        {:else if board === 'feedback'}
          <FeedbackTriagePage />
        {:else}
          <BacklogBoardPage />
        {/if}
      </div>
    {/each}
  {/key}
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
  /* THE PIN (car N3): the map holds under the 44px perspective bar, on
     the page's own ground so the panel scrolling beneath it does not
     show through, above the panel's boards and below the shell's
     sidebar (20) and bar (60). */
  .map-pin.pinned { position: sticky; top: 44px; z-index: 10; background: var(--map-bg);
    padding-bottom: var(--s2); border-bottom: 1px solid var(--map-rule); }
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
  /* The readings (car N2): a board of cells, each a mono label over the
     server's own sentences. The long lists — what waits, and the
     crossings — take the whole row, so a hold sentence is never
     squeezed into a column; `dense` lets the short cells close up the
     row a long one leaves behind. On a phone every cell is a row. */
  .panel-cells { display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); grid-auto-flow: dense;
    gap: var(--s2); margin: var(--s2) 0 var(--s3); }
  .panel-cell { background: var(--map-bg); border: 1px solid var(--map-rule); padding: var(--s2); min-width: 0; }
  .panel-cell[data-field='route'], .panel-cell[data-field='waiting'], .panel-cell[data-field='crossings'] { grid-column: 1 / -1; }
  .cell-label { margin: 0 0 4px; font-family: var(--font-mono); font-size: 11px; font-weight: 400;
    letter-spacing: 0.08em; text-transform: uppercase; color: var(--map-muted); }
  .cell-lines { margin: 0; padding: 0; list-style: none; font-size: 13px; line-height: 1.45; color: var(--map-ink);
    overflow-wrap: anywhere; }
  .cell-lines li + li { margin-top: 2px; }
  .panel-contents { margin-top: var(--s3); border-top: 1px solid var(--map-rule); }
  .panel-board { margin-top: var(--s3); }
  .panel-none { margin: var(--s2) 0 0; font-size: 13px; color: var(--map-bad-ink); overflow-wrap: anywhere; }
</style>
