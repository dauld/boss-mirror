<script lang="ts">
  // THE IT MAP AS A TRANSIT MONITOR (design 16091dfb, answered by David
  // 2026-09-25; backlog ced4ca8b) — behind the flight `it-map-transit`.
  // MapPage mounts this in place of the world map for a viewer the
  // flight lists; for everyone else the world map stays the map until
  // the flight is promoted (Q2).
  //
  // Each region is a STATION on a schematic line at fixed angles, each
  // route the server serves a SECTION of track in its line's colour, the
  // packets waiting to cross stand as blocks on the approach, a block
  // moves at the section's real crossing rate replayed ×60, a section the
  // server judges not flowing is drawn red on the track itself, and a
  // station's ring is its state — pulsing when troubled. The verdicts the
  // world map writes inside a territory stand on an ALARMS BOARD beside
  // the map instead: every non-clear station with the server's own why,
  // troubled first. A station, and an alarm, selects that station — the
  // same route a territory selects.
  //
  // ONLY SERVED ROUTES ARE DRAWN (design e765b3fc, car R3). The sections
  // are `GET /api/yard/routes`, laid out by route-layout.ts and drawn by
  // RouteLayer.svelte: the train's own line out of the dock, back to the
  // gates and over to the track, the garage's sidings both ways, every
  // packet's EXIT as an off-ramp ending at a buffer stop, every ENTRY as
  // a stub into its station — and a route only the moves record
  // supports, observed but declared by no protocol or hand-off, dashed
  // red. While the routes are
  // unread the stations still stand and no section is drawn: a guessed
  // track would be the hand-drawn map this car deleted.
  //
  // THE DETAIL LEAVES THE MAP (design e765b3fc, car N2; David
  // 2026-09-25: "put more of that data behind a map selection for
  // display at the bottom ... we don't need to keep it all on the main
  // map"). The map carries a station's name, its one number and its
  // state, and the piles; the headway written on every section, and the
  // why and flowing rule its hover titles carried, are the selection
  // panel's now (panel.ts). A section is a door like a station: a click
  // selects it (`/it?at=dock->track`), and whatever is selected is MARKED
  // on the map — a casing under a section, a ring round a station — drawn
  // apart from the station's own state ring, which is the state's alone.
  //
  // The layout and every word are transit.ts, unit-pinned; this owns the
  // strokes and the motion. Colours are --map-* tokens only — the lines
  // are the Design department's --map-line-* (styles.css), the rings the
  // map's own states — so map-palette.test.ts and the a-colour-is-a-token
  // lint hold this file to the one palette. Reduced motion is honoured:
  // no block moves and no ring pulses, and a section's panel carries the
  // rate.
  import { navigate } from '@boss/web-kit/nav';
  import { MediaQuery } from 'svelte/reactivity';
  import type { Border, Borders } from './borders';
  import { countText, regionHref, type Region, type Regions } from './regions';
  import type { Routes } from './routes';
  import { linesOf, sectionKey, sectionsOf } from './route-layout';
  import RouteLayer from './RouteLayer.svelte';
  import {
    LINE_LABEL,
    NAME_ABOVE,
    REPLAY_TEXT,
    STATIONS,
    TRANSIT_VIEW,
    alarmsOf,
    ringOf,
    stationCount,
    stationLabel,
  } from './transit';

  type Props = Readonly<{
    regions: Regions;
    /** The routes read (design e765b3fc, car R3), or null while unread or
     *  unreadable: the stations still stand, and no section is drawn —
     *  the page says the read failed. */
    routes?: Routes | null;
    /** The borders read, or null while unread or unreadable: a section
     *  then reads "no reading" and nothing waits or moves on it. */
    borders?: Borders | null;
    /** What the page has selected, in the map's own keys — a station's
     *  name or a section's `from→to` (selection.ts `markOf`); null for
     *  nothing. Marked, never re-read: the page decides what is selected. */
    selected?: string | null;
  }>;
  let { regions, routes = null, borders = null, selected = null }: Props = $props();

  const prefersReduced = new MediaQuery('(prefers-reduced-motion: reduce)');
  const reduced = $derived(prefersReduced.current);

  const byName = $derived(new Map(regions.regions.map((r) => [r.name, r] as const)));
  const byKey = $derived(new Map<string, Border>((borders?.borders ?? []).map((b) => [sectionKey(b.from, b.to), b])));
  const alarms = $derived(alarmsOf(regions));

  /** The served routes, laid out — and any this layout has no station
   *  for, said at the foot rather than dropped. */
  const laid = $derived(routes === null ? { sections: [], unplaced: [] } : sectionsOf(routes));
  const lines = $derived(linesOf(laid.sections));
  const anyExit = $derived(laid.sections.some((s) => s.kind === 'exit'));
  const anyUndeclared = $derived(laid.sections.some((s) => !s.declared));

  /** A station's name, its one number and its state — what the map
   *  carries. The why is the panel's. */
  const stationTitle = (name: string, r: Region | undefined): string =>
    r === undefined
      ? `${stationLabel(name)} · troubled · no reading`
      : `${stationLabel(name)} · ${countText(r)} · ${r.state}`;

  function open(e: MouseEvent, href: string): void {
    e.preventDefault();
    navigate(href);
  }
</script>

<section class="transit" data-transit data-motion={reduced ? 'reduced' : 'moving'}>
  <div class="grid">
    <div class="board">
      <svg
        viewBox="0 0 {TRANSIT_VIEW.width} {TRANSIT_VIEW.height}"
        role="group"
        aria-label="the IT network as a transit map: stations on their lines, the traffic on every section">
        <!-- THE ROUTES (design e765b3fc, car R3): every section, exit and
             entry the routes read serves, drawn by RouteLayer.svelte from
             route-layout.ts — the layer the moves of car M2 travel. -->
        <RouteLayer sections={laid.sections} borders={byKey} {reduced} {selected} />

        <!-- THE STATIONS: a ring each in the region's state, pulsing when
             troubled, and a door to its panel. The selected one wears a
             second, ink ring outside its own — the mark is the
             selection's, the inner ring stays the state's. -->
        {#each STATIONS as st (st.name)}
          {@const r = byName.get(st.name)}
          {@const state = ringOf(r)}
          {@const isSelected = selected === st.name}
          <a class="station" href={regionHref(st.name)} data-station={st.name} data-state={state}
            data-selected={isSelected ? 'true' : undefined} aria-current={isSelected ? 'true' : undefined}
            aria-label={stationTitle(st.name, r)} onclick={(e) => open(e, regionHref(st.name))}>
            <title>{stationTitle(st.name, r)}</title>
            {#if isSelected}
              <circle cx={st.x} cy={st.y} r="20" class="sel-ring" data-selected-mark={st.name} />
            {/if}
            {#if state === 'troubled' && !reduced}
              <circle cx={st.x} cy={st.y} r="15" class="pulse" data-pulse={st.name} />
            {/if}
            <circle cx={st.x} cy={st.y} r="12" class="ring {state}" />
            <text x={st.x} y={st.below ? st.y + 32 : st.y - NAME_ABOVE} text-anchor="middle" class="stn">{stationLabel(st.name)}</text>
            <text x={st.x} y={st.below ? st.y + 46 : st.y + 40} text-anchor="middle" class="sub">{stationCount(r)}</text>
          </a>
        {/each}
      </svg>
    </div>

    <!-- THE ALARMS BOARD: every station that is not clear, with the
         server's own why, troubled first. The why is printed whole —
         clamped on screen, never cut in the record. -->
    <aside class="alarms" aria-label="alarms">
      <h2>Alarms</h2>
      {#each alarms as a (a.name)}
        <a class="alarm" href={regionHref(a.name)} data-alarm={a.name} data-state={a.state} title={a.why}
          onclick={(e) => open(e, regionHref(a.name))}>
          <span class="alarm-head"><span class="alarm-name">{a.label}</span><span class="st {a.state}">{a.state}</span></span>
          <span class="why">{a.why}</span>
        </a>
      {:else}
        <p class="none" data-alarms-clear>No alarms — every station is clear or full.</p>
      {/each}
    </aside>
  </div>

  <div class="key" aria-label="the lines">
    {#each lines as l (l)}
      <span class="key-item"><svg class="swatch" viewBox="0 0 20 4" aria-hidden="true"><line x1="0" y1="2" x2="20" y2="2" class="line-{l}" /></svg>{LINE_LABEL[l]}</span>
    {/each}
    <span class="key-item"><svg class="swatch" viewBox="0 0 20 4" aria-hidden="true"><line x1="0" y1="2" x2="20" y2="2" class="held" /></svg>a held section: the server judges nothing is crossing</span>
    {#if anyExit}
      <span class="key-item"><svg class="swatch tall" viewBox="0 0 20 12" aria-hidden="true"><line x1="10" y1="0" x2="10" y2="10" class="line-delivery" /><line x1="4" y1="10" x2="16" y2="10" class="line-delivery" /></svg>an exit: packets leave the map there, by the terminals it names</span>
    {/if}
    {#if anyUndeclared}
      <span class="key-item" data-key-undeclared><svg class="swatch" viewBox="0 0 20 4" aria-hidden="true"><line x1="0" y1="2" x2="20" y2="2" class="undeclared" /></svg>dashed red: packets moved this way, and no protocol or hand-off declares it</span>
    {/if}
  </div>
  {#if laid.unplaced.length > 0}
    <p class="unplaced" data-unplaced>served, with no station on this map to draw it at: {laid.unplaced.join(', ')}</p>
  {/if}
  <p class="replay" data-replay>
    {REPLAY_TEXT}{reduced ? '. Reduced motion is on: nothing moves, and a section\'s panel carries its rate.' : ''}
  </p>
</section>

<style>
  /* Every colour a --map-* token from styles.css with no fallback: the
     lines the Design department's --map-line-*, the rings the map's own
     ok / warn / bad edges, the words its ink and muted. */
  .transit { margin-top: var(--s3); }
  .grid { display: grid; grid-template-columns: minmax(0, 1fr) 300px; gap: var(--s3); align-items: start; }
  @media (max-width: 1100px) { .grid { grid-template-columns: minmax(0, 1fr); } }
  .board { background: var(--map-bg); border: 1px solid var(--map-rule); border-radius: var(--radius); overflow-x: auto; }
  .board svg { display: block; width: 100%; min-width: 720px; height: auto; font-family: var(--font-body); }
  .board text { fill: var(--map-ink); }
  .stn { font-size: 12px; font-weight: 600; }
  .sub { font-size: 10.5px; fill: var(--map-muted); }
  .board text.sub { fill: var(--map-muted); }

  .line-delivery { stroke: var(--map-line-delivery); }
  .line-publish { stroke: var(--map-line-publish); }
  .line-siding { stroke: var(--map-line-siding); }
  .line-tenant { stroke: var(--map-line-tenant); }
  .swatch line.held { stroke: var(--map-bad-edge); }

  /* THE SELECTION'S MARK (car N2), in the map's ink: a ring outside the
     selected station's own (a section's casing is RouteLayer's). Not a
     state colour, so the mark never reads as a verdict. */
  .sel-ring { fill: none; stroke: var(--map-ink); stroke-width: 3; }
  /* The key's swatch for an observed, undeclared route — drawn as
     RouteLayer draws the route itself. */
  .swatch line.undeclared { stroke: var(--map-bad-edge); stroke-dasharray: 9 6; stroke-linecap: butt; }

  .station { cursor: pointer; }
  .station:focus-visible { outline: none; }
  .station:focus-visible .ring { stroke-width: 6; }
  .ring { fill: var(--map-surface); stroke-width: 5; }
  .ring.clear { stroke: var(--map-ok-edge); stroke-width: 3; }
  /* FULL (design e765b3fc Q2, David 2026-09-25): at capacity and moving
     is a SOLID disk with a heavier ring — clear stays hollow — so "the
     gates are full" reads by fill alone, without the small text. */
  .ring.full { fill: var(--map-full); stroke: var(--map-full); stroke-width: 6; }
  .ring.attention { stroke: var(--map-warn-edge); }
  .ring.troubled { stroke: var(--map-bad-edge); }
  .pulse { fill: none; stroke: var(--map-bad-edge); stroke-width: 3; transform-box: fill-box; transform-origin: center;
    animation: pulse 1.6s ease-out infinite; }
  @keyframes pulse { from { transform: scale(1); opacity: 0.9; } to { transform: scale(1.75); opacity: 0; } }
  /* Belt and braces: the pulse is not rendered under reduced motion,
     and a stylesheet that outlives that decision still does not move. */
  @media (prefers-reduced-motion: reduce) { .pulse { animation: none; } }

  .alarms { background: var(--map-surface); border: 1px solid var(--map-rule); border-top: 3px solid var(--map-ink);
    border-radius: var(--radius); padding: var(--s2) var(--s3); }
  .alarms h2 { font-family: var(--font-mono); font-size: 11px; letter-spacing: var(--ls-nav); text-transform: uppercase;
    margin: 0 0 var(--s2); color: var(--map-ink); }
  .alarm { display: block; border-top: 1px solid var(--map-rule); padding: var(--s2) 0; color: var(--map-ink); text-decoration: none; }
  .alarm:first-of-type { border-top: 0; }
  .alarm:hover .alarm-name, .alarm:focus-visible .alarm-name { text-decoration: underline; }
  .alarm-head { display: flex; justify-content: space-between; gap: var(--s2); font-size: 13px; font-weight: 600; }
  .st { font-family: var(--font-mono); font-size: 11px; letter-spacing: var(--ls-label); text-transform: uppercase; }
  .st.troubled { color: var(--map-bad-ink); }
  .st.attention { color: var(--map-warn-ink); }
  .why { display: -webkit-box; -webkit-line-clamp: 4; line-clamp: 4; -webkit-box-orient: vertical; overflow: hidden;
    margin-top: 3px; font-size: 12px; color: var(--map-muted); overflow-wrap: anywhere; }
  .none { margin: 0; font-size: 12px; color: var(--map-muted); }

  .key { display: flex; flex-wrap: wrap; gap: 6px 16px; margin-top: var(--s2); font-size: 12px; color: var(--map-muted); }
  .key-item { display: inline-flex; align-items: center; gap: 6px; }
  .swatch { width: 20px; height: 4px; }
  .swatch line { stroke-width: 4; }
  .swatch.tall { height: 12px; }
  .unplaced { margin: var(--s1) 0 0; font-size: 12px; color: var(--map-bad-ink); overflow-wrap: anywhere; }
  .replay { margin: var(--s1) 0 0; font-size: 12px; color: var(--map-muted); }
</style>
