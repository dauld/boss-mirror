<script lang="ts">
  // THE IT MAP AS A TRANSIT MONITOR (design 16091dfb, answered by David
  // 2026-09-25; backlog ced4ca8b) — behind the flight `it-map-transit`.
  // MapPage mounts this in place of the world map for a viewer the
  // flight lists; for everyone else the world map stays the map until
  // the flight is promoted (Q2).
  //
  // The same two reads the world map draws from, and no server change:
  // each region is a STATION on a schematic line at fixed angles, each
  // border a SECTION of track in its route's colour, the packets waiting
  // to cross stand as blocks on the approach, a block moves at the
  // section's real crossing rate replayed ×60, a section the server
  // judges not flowing is drawn red on the track itself, and a station's
  // ring is its state — pulsing when troubled. The verdicts the world map
  // writes inside a territory stand on an ALARMS BOARD beside the map
  // instead: every non-clear station with the server's own why, troubled
  // first. The planned tenant branch (design fd8b5143) is drawn dashed. A
  // station, and an alarm, selects that station — the same route a
  // territory selects.
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
  import { countText, regionHref, sectionHref, type Region, type Regions } from './regions';
  import {
    LINE_LABEL,
    REPLAY_TEXT,
    SECTIONS,
    STATIONS,
    TENANT_BRANCH,
    TRANSIT_VIEW,
    alarmsOf,
    ringOf,
    sectionGround,
    sectionKey,
    stationCount,
    stationLabel,
    trainsOf,
    waitingBlocks,
    type TransitLine,
  } from './transit';

  type Props = Readonly<{
    regions: Regions;
    /** The borders read, or null while unread or unreadable: the track
     *  still draws — the layout is the map — but every section then
     *  reads "no reading" and nothing waits or moves on it. */
    borders?: Borders | null;
    /** What the page has selected, in the map's own keys — a station's
     *  name or a section's `from→to` (selection.ts `markOf`); null for
     *  nothing. Marked, never re-read: the page decides what is selected. */
    selected?: string | null;
  }>;
  let { regions, borders = null, selected = null }: Props = $props();

  const prefersReduced = new MediaQuery('(prefers-reduced-motion: reduce)');
  const reduced = $derived(prefersReduced.current);

  const byName = $derived(new Map(regions.regions.map((r) => [r.name, r] as const)));
  const byKey = $derived(new Map<string, Border>((borders?.borders ?? []).map((b) => [sectionKey(b.from, b.to), b])));
  const alarms = $derived(alarmsOf(regions));

  const LINES: ReadonlyArray<TransitLine> = ['delivery', 'publish', 'siding', 'tenant'];

  /** A section's name and nothing more: its rate, its queue and its
   *  verdict are the panel's (car N2). */
  const sectionTitle = (from: string, to: string): string => `the ${stationLabel(from)} → ${stationLabel(to)} section`;

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
        <!-- THE PLANNED TENANT BRANCH (design fd8b5143): dashed, and read
             from nothing — a plan, not a reading. -->
        <g class="tenant-branch" data-planned="tenant">
          <path d={TENANT_BRANCH.d} class="branch line-tenant" />
          {#each TENANT_BRANCH.stations as s (s.name)}
            <circle cx={s.x} cy={s.y} r="9" class="branch-stop" class:owned={s.owner !== null}
              data-owner={s.owner ?? undefined} />
            <text x={s.x} y={s.y - 22} text-anchor="middle" class="stn">{s.name}</text>
          {/each}
          <text x={TENANT_BRANCH.note.x} y={TENANT_BRANCH.note.y} text-anchor="middle" class="sub">{TENANT_BRANCH.note.text}</text>
        </g>

        <!-- THE SECTIONS: a border each, in its route's colour — red where
             the server says it is not flowing, dotted where it cannot
             tell — with its waiting blocks and its trains. Each is a door
             to its own panel: a wide unpainted stroke takes the click, so
             an 8-unit line is not a needle to aim at. The selected one
             stands in an ink casing. -->
        {#each SECTIONS as s (s.key)}
          {@const b = byKey.get(s.key)}
          {@const ground = sectionGround(b)}
          {@const waiting = waitingBlocks(s, b)}
          {@const trains = trainsOf(b, reduced)}
          {@const href = sectionHref(s.from, s.to)}
          {@const isSelected = selected === s.key}
          <a class="section-link" {href} data-section-link={s.key} data-selected={isSelected ? 'true' : undefined}
            aria-current={isSelected ? 'true' : undefined} aria-label={sectionTitle(s.from, s.to)}
            onclick={(e) => open(e, href)}>
            {#if isSelected}
              <path d={s.d} class="casing" data-selected-mark={s.key} />
            {/if}
            <path d={s.d} class="section line-{s.line}" class:held={ground === 'held'} class:unknown={ground === 'unknown'}
              data-section={s.key} data-line={s.line} data-ground={ground}>
              <title>{sectionTitle(s.from, s.to)}</title>
            </path>
            <path d={s.d} class="hit" aria-hidden="true" />
          </a>
          {#each waiting.blocks as p, i (i)}
            <rect x={p.x - 4} y={p.y - 13} width="8" height="7" rx="1.5" class="waiting" data-waiting={s.key} />
          {/each}
          {#if waiting.more !== null}
            <text x={waiting.more.at.x - 8} y={waiting.more.at.y - 7} text-anchor="end" class="more" data-more={s.key}>+{waiting.more.n}</text>
          {/if}
          {#if trains !== null}
            {#each trains.begins as begin, i (i)}
              <rect x="-7" y="-4" width="14" height="8" rx="2" class="train line-{s.line}" data-train={s.key}>
                <animateMotion dur="{trains.dur.toFixed(3)}s" begin="{begin.toFixed(3)}s" repeatCount="indefinite"
                  path={s.d} rotate="auto" />
              </rect>
            {/each}
          {/if}
        {/each}

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
            <text x={st.x} y={st.below ? st.y + 32 : st.y - 24} text-anchor="middle" class="stn">{stationLabel(st.name)}</text>
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
    {#each LINES as l (l)}
      <span class="key-item"><svg class="swatch" viewBox="0 0 20 4" aria-hidden="true"><line x1="0" y1="2" x2="20" y2="2" class="line-{l}" class:dashed={l === 'tenant'} /></svg>{LINE_LABEL[l]}</span>
    {/each}
    <span class="key-item"><svg class="swatch" viewBox="0 0 20 4" aria-hidden="true"><line x1="0" y1="2" x2="20" y2="2" class="held" /></svg>a held section: the server judges nothing is crossing</span>
  </div>
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
  .board text.more { font-family: var(--font-mono); font-size: 9.5px; fill: var(--map-muted); font-variant-numeric: tabular-nums; }

  .line-delivery { stroke: var(--map-line-delivery); }
  .line-publish { stroke: var(--map-line-publish); }
  .line-siding { stroke: var(--map-line-siding); }
  .line-tenant { stroke: var(--map-line-tenant); }
  .swatch line.held { stroke: var(--map-bad-edge); }

  .section { fill: none; stroke-width: 8; stroke-linecap: round; stroke-linejoin: round; }
  .section.held { stroke: var(--map-bad-edge); }
  .section.unknown { stroke-dasharray: 2 6; opacity: 0.6; }
  /* The section's door: a wide stroke nobody sees takes the click. */
  .section-link { cursor: pointer; }
  .section-link:focus-visible { outline: none; }
  .hit { fill: none; stroke: transparent; stroke-width: 22; stroke-linecap: round; pointer-events: stroke; }
  /* THE SELECTION'S MARK (car N2), in the map's ink: a casing under the
     selected section, as a transit diagram draws an interchange, and a
     ring outside the selected station's own. Neither is a state colour,
     so the mark never reads as a verdict. */
  .casing { fill: none; stroke: var(--map-ink); stroke-width: 16; stroke-linecap: round; stroke-linejoin: round; }
  .section-link:focus-visible .section, .section-link:hover .section { stroke-width: 11; }
  .sel-ring { fill: none; stroke: var(--map-ink); stroke-width: 3; }
  .branch { fill: none; stroke-width: 6; stroke-dasharray: 10 7; stroke-linecap: round; stroke-linejoin: round; }
  .branch-stop { fill: var(--map-surface); stroke: var(--map-line-tenant); stroke-width: 3; }
  .branch-stop.owned { stroke: var(--map-ink); stroke-width: 4; }

  .waiting { fill: var(--map-ink); }
  .train { fill: var(--map-surface); stroke-width: 2; }

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
  .swatch line.dashed { stroke-dasharray: 5 3; }
  .replay { margin: var(--s1) 0 0; font-size: 12px; color: var(--map-muted); }
</style>
