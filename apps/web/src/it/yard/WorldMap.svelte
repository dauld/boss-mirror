<script lang="ts">
  // THE IT WORLD MAP (design d2154293, car 1). One SVG in one
  // coordinate space: world.ts declares the eight regions as
  // territories laid out along the packet flow and the borders between
  // them; this draws each territory as an outline in the yard's own
  // strokes with the region's count, state and trend from
  // /api/yard/regions rendered INSIDE it, and the why on a troubled
  // one — troubled where it is on the map, not in a card somewhere
  // else. The map draws; every number is the server's (0524fc95 Q3).
  //
  // A territory is a door, and since car 3 the door is a ZOOM, not a
  // departure: clicking one walks the viewBox into that territory's
  // rect and NEVER LEAVES THE WORLD (David, feedback c3105b2a: "zoom
  // into the region by clicking to see" what is moving inside it).
  // Zoomed, the territory shows its INTERIOR — the wagons standing at
  // the stations it covers, off the floor's own Scene — under a
  // compact header carrying the region's count, state and why; the
  // full trend block the world draws at rest gives up its room to
  // them. Two territories hold queues rather than rolling stock —
  // receiving and marshalling — and since car 4 they draw a PLATFORM
  // PER QUEUE instead (world-interior.ts), which is what "this
  // region's floor is a page of its own" used to stand in for.
  // The arithmetic of all three is region-contents.ts and world-interior.ts, unit-pinned;
  // this owns the animation frames and the strokes. NO NEW STYLING —
  // the classes here are YardMap's, by name and by token, so the
  // visual redesign reskins one grammar (Q1, decided 2026-09-19).
  import { untrack } from 'svelte';
  import { navigate } from '@boss/web-kit/nav';
  import { countText, floorHref, lampOf, trendText, type Region, type Regions } from './regions';
  import {
    densityOf,
    machineText,
    rateText as borderRateText,
    waitingText,
    type Border,
    type Borders,
  } from './borders';
  import { BORDERS, TERRITORIES, WORLD, borderPath, territoryOf, wrapWords, type Territory } from './world';
  import { machineTitle, machineryLabel, machineryStrip } from './world-machines';

  type Props = Readonly<{
    regions: Regions;
    /** The rails' readings, or null while unread or unreadable: the
     *  rails still DRAW — the layout is the map — but every number on
     *  them then reads unknown rather than zero (car 2). */
    borders?: Borders | null;
  }>;
  let { regions, borders = null }: Props = $props();

  const key = (from: string, to: string): string => `${from}→${to}`;
  const byBorder = $derived(new Map((borders?.borders ?? []).map((b) => [key(b.from, b.to), b] as const)));
  /** A border the server answered that the layout does not draw — said
   *  at the foot of the map beside an unmapped region, never dropped. */
  const unmappedBorders = $derived(
    (borders?.borders ?? [])
      .filter((b) => !BORDERS.some((d) => d.from === b.from && d.to === b.to))
      .map((b) => key(b.from, b.to)),
  );

  /** The rail's midpoint — where the traffic token stands. A cubic whose
   *  control points share the endpoints' axes passes through the mean of
   *  its endpoints at t=0.5, so this IS the curve's middle. */
  const mid = (p: { x1: number; y1: number; x2: number; y2: number }) => ({
    x: (p.x1 + p.x2) / 2,
    y: (p.y1 + p.y2) / 2,
  });

  /** Everything the rail knows, for the hover — a border must not need
   *  a second surface to explain what it is showing. */
  function borderTitle(b: Border | undefined, from: string, to: string): string {
    if (b === undefined) return `${from} → ${to} — the borders read answered nothing for this rail`;
    const holds = b.holds.map((h) => `  ${h.what} — ${h.why}`).join('\n');
    return [
      `${from} → ${to} · ${b.state} — ${b.why}`,
      `one crossing = ${b.crossing}`,
      `rate: ${borderRateText(b.rate)}`,
      `${waitingText(b)}${holds === '' ? '' : `:\n${holds}`}`,
      `machine: ${machineText(b.machine)} (${b.machine.why})`,
    ].join('\n');
  }

  /** What the token prints: the queue depth, or `?` for a count the
   *  server could not take. Never 0 for "I do not know". */
  const tokenText = (b: Border | undefined): string =>
    b === undefined || b.waiting === null ? '?' : String(b.waiting);

  /** `3/d`, or `?` — the rate beside the rail, short enough for the gap
   *  between two territories. The full sentence rides the title. */
  const railRate = (b: Border | undefined): string =>
    b === undefined || b.rate.current === null ? '?' : `${Math.round(b.rate.current * 10) / 10}/d`;

  const densityFor = (b: Border | undefined) => densityOf(b === undefined ? null : b.rate.current);
  const stateOfBorder = (b: Border | undefined) => b?.state ?? 'troubled';
  /** The machine's lamp: its own silence when it declares a cadence,
   *  otherwise unlit — an unlit lamp is "cannot tell", not "fine". */
  const machineLamp = (b: Border | undefined): string =>
    b === undefined || b.machine.silent === null ? 'unknown' : b.machine.silent ? 'err' : 'ok';

  const byName = $derived(new Map(regions.regions.map((r) => [r.name, r] as const)));
  /** A region the server answered that the layout has no territory
   *  for is said at the foot of the map, never dropped: the layout is
   *  pinned to the server's list, but a newer server is not a blank. */
  const unmapped = $derived(regions.regions.filter((r) => territoryOf(r.name) === undefined).map((r) => r.name));

  // The text inside an outline: 9px mono is ~5.8px a character, and a
  // why gets the lines the outline has room for below the trend.
  const chars = (t: Territory): number => Math.floor((t.w - 16) / 5.8);
  const whyLines = (t: Territory): number => Math.max(1, Math.min(4, Math.floor((t.h - 116) / 12)));
  const stateOf = (r: Region | undefined) => r?.state ?? 'troubled';
  const whyOf = (r: Region | undefined) => r?.why ?? 'the server answered no reading for this region';

  function open(e: MouseEvent, href: string): void {
    e.preventDefault();
    navigate(href);
  }
</script>

<section class="yard" aria-label="the IT world map">
  <svg
    viewBox="0 0 {WORLD.width} {WORLD.height}"
    role="img"
    aria-label="the IT world: eight territories along the packet flow">
    <!-- the borders: a track segment per hop of the flow, under the
         territories so an outline sits on the rail -->
    {#each BORDERS as b (`${b.from}→${b.to}`)}
      {@const from = territoryOf(b.from)}
      {@const to = territoryOf(b.to)}
      {#if from && to}
        {@const p = borderPath(from, to)}
        {@const row = byBorder.get(`${b.from}→${b.to}`)}
        <path d={p.d} class="tie" data-border="{b.from}→{b.to}" />
        <path d={p.d} class="rail" data-state={stateOfBorder(row)} />
        <!-- the traffic itself: dashes running along the rail, their
             weight from the crossing rate the server measured. An
             UNMEASURED rate gets its own band (a dotted, unlit rail),
             never the empty-rail one. -->
        <path d={p.d} class="traffic" data-traffic="{b.from}→{b.to}" data-density={densityFor(row)} />
      {/if}
    {/each}

    <!-- the territories: an outline each, the region's numbers inside -->
    {#each TERRITORIES as t (t.name)}
      {@const r = byName.get(t.name)}
      {@const state = stateOf(r)}
      {@const troubled = state === 'troubled'}
      {@const machinery = machineryStrip(t, r?.machines ?? [])}
      <a
        class="territory machine"
        data-region={t.name}
        data-state={state}
        href={floorHref(t.name)}
        aria-label="{t.name} · {state} — {whyOf(r)}"
        onclick={(e) => open(e, floorHref(t.name))}>
        <title>{t.name} · {state} — {whyOf(r)}</title>
        <rect x={t.x} y={t.y} width={t.w} height={t.h} class="shed" class:warn={state === 'busy'} class:err={troubled} />
        <text x={t.x + 8} y={t.y + 18}>{t.name}</text>
        <text x={t.x + 8} y={t.y + 44} class="count">{r ? countText(r) : 'no reading'}</text>
        <circle cx={t.x + 12} cy={t.y + 58} r="4" class="lamp {lampOf(state)}" />
        <text x={t.x + 22} y={t.y + 62} class:err={troubled}>{state}</text>
        {#if r}
          <!-- the trend as the card printed it, one part per line so
               the samples do not run past the outline -->
          <text x={t.x + 8} y={t.y + 80} class="tiny">{r.trend.metric}</text>
          <text x={t.x + 8} y={t.y + 92} class="tiny trend">
            {#each trendText(r.trend).split(' · ') as part, i (i)}
              <tspan x={t.x + 8} dy={i === 0 ? 0 : 12}>{part}</tspan>
            {/each}
          </text>
        {/if}
        {#if troubled}
          <!-- a verdict must name what failed: the why, where the
               trouble is, wrapped to the outline (whole in the title) -->
          <text x={t.x + 8} y={t.y + 122} class="tiny err why">
            {#each wrapWords(whyOf(r), chars(t), whyLines(t)) as line, i (i)}
              <tspan x={t.x + 8} dy={i === 0 ? 0 : 12}>{line}</tspan>
            {/each}
          </text>
        {/if}
        <!-- THE MACHINERY (car 5). The region's actors, read from the
             registries and judged on the SERVER — gate bays, the
             conductor, the stations, the runners — drawn along the
             territory's bottom edge, because "running machinery" is
             what a region is for. The glyph
             says the state without a legend: running MOVES, idle is
             still and solid, failed blinks red, unknown is a broken
             outline with a mark. Idle and unknown are told apart by
             SHAPE, not shade: a machine nobody measured must never
             draw as a calm one. -->
        {#if machinery.placed.length > 0}
          <g class="machinery" data-machinery={t.name} aria-label={machineryLabel(r?.machines ?? [])}>
            {#each machinery.placed as p (p.machine.id)}
              <g class="glyph {p.machine.state}" data-machine={p.machine.id} data-machine-state={p.machine.state}>
                <title>{machineTitle(p.machine)}</title>
                <rect x={p.x} y={p.y} width={p.w} height={p.h} class="shed" class:err={p.machine.state === 'failed'} />
                {#if p.machine.state === 'running'}
                  <rect class="lamp ok piston" x={p.x + 2} y={p.y + 3} width="4" height={p.h - 6} />
                {:else if p.machine.state === 'failed'}
                  <rect class="lamp err" x={p.x + 3} y={p.y + 3} width={p.w - 6} height={p.h - 6} />
                {:else if p.machine.state === 'unknown'}
                  <text class="tiny mark" x={p.x + p.w / 2} y={p.y + p.h - 3} text-anchor="middle">?</text>
                {/if}
              </g>
            {/each}
            {#if machinery.hidden > 0}
              <!-- what did not fit is COUNTED; the strip is ordered so
                   a failure or an unknown is never what gets cut -->
              <text x={t.x + t.w - 4} y={t.y + t.h - 6} text-anchor="end" class="tiny">+{machinery.hidden}</text>
            {/if}
          </g>
        {/if}
      </a>
    {/each}

    <!-- THE BORDER TOKENS, drawn ON TOP of the territories: what stands
         at each border now, the machine that moves it, and the crossing
         rate. Every state is the server's (design d2154293) — the map
         only decides how thick to draw the rail. -->
    {#each BORDERS as b (`t:${b.from}→${b.to}`)}
      {@const from = territoryOf(b.from)}
      {@const to = territoryOf(b.to)}
      {#if from && to}
        {@const p = borderPath(from, to)}
        {@const m = mid(p)}
        {@const row = byBorder.get(`${b.from}→${b.to}`)}
        {@const state = stateOfBorder(row)}
        <g class="crossing" data-crossing="{b.from}→{b.to}" data-state={state}
           data-waiting={row === undefined || row.waiting === null ? 'unknown' : row.waiting}>
          <title>{borderTitle(row, b.from, b.to)}</title>
          <!-- the machine's lamp, above the rail: lit red once it has
               been silent past its OWN declared cadence, unlit when
               nothing records it -->
          <rect x={m.x - 5} y={m.y - 22} width="10" height="10" class="glyph {machineLamp(row)}" />
          <!-- what waits to cross, on the rail -->
          <circle cx={m.x} cy={m.y} r="9" class="token {state}" />
          <text x={m.x} y={m.y + 3.5} text-anchor="middle" class="token-count">{tokenText(row)}</text>
          <!-- and the rate, under it -->
          <text x={m.x} y={m.y + 22} text-anchor="middle" class="tiny rate">{railRate(row)}</text>
        </g>
      {/if}
    {/each}

    {#if unmapped.length > 0}
      <text x={WORLD.width - 20} y={WORLD.height - 8} text-anchor="end" class="tiny err"
        >not on the map: {unmapped.join(', ')}</text>
    {/if}
    {#if unmappedBorders.length > 0}
      <text x={WORLD.width - 20} y={WORLD.height - 20} text-anchor="end" class="tiny err"
        >no rail drawn for: {unmappedBorders.join(', ')}</text>
    {/if}
  </svg>
</section>

<style>
  /* YardMap's classes, by name and by token — the rails are the strong
     border, the ties the hairline, the ground the void, the outlines
     the sheds, the lamps the lamps. Nothing new enters the system
     here; the count is the deck's entity title (18px, 600). */
  .yard {
    --rail: var(--border-strong, #3a434d);
    --tie: var(--hairline, #2a3138);
    background: var(--void, #0d1014);
    border: 1px solid var(--hairline, #2a3138);
    padding: var(--s2, 8px);
    overflow-x: auto;
    margin-top: var(--s3, 12px);
  }
  .yard svg {
    display: block;
    width: 100%;
    min-width: 900px;
    height: auto;
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .yard text {
    fill: var(--static, #7a838c);
    font-size: 10px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .yard text.tiny { font-size: 9px; letter-spacing: 0.04em; text-transform: none; }
  .yard text.count { font-size: 18px; font-weight: 600; fill: var(--fog, #e8ecef); letter-spacing: 0; text-transform: none; }
  .yard text.err { fill: var(--err, #e2685c); }
  .rail { stroke: var(--rail); stroke-width: 2; fill: none; }
  .rail[data-state='busy'] { stroke: var(--warn, #d9a441); }
  .rail[data-state='troubled'] { stroke: var(--err, #e2685c); }
  /* The traffic: dashes running the rail from A to B. Weight and speed
     come from the measured rate; `unknown` is deliberately a sparse,
     unlit dotting so an unmeasured rail cannot read as an empty one. */
  .traffic { fill: none; stroke: var(--ok, #4fb98a); stroke-linecap: round; opacity: 0.85;
    stroke-width: 2; stroke-dasharray: 2 10; animation: flow 3s linear infinite; }
  .traffic[data-density='unknown'] { stroke: var(--static, #7a838c); stroke-dasharray: 1 7;
    opacity: 0.4; animation: none; }
  .traffic[data-density='none'] { stroke: none; animation: none; }
  .traffic[data-density='light'] { stroke-width: 2; stroke-dasharray: 2 14; animation-duration: 4s; }
  .traffic[data-density='steady'] { stroke-width: 3; stroke-dasharray: 4 10; animation-duration: 2.4s; }
  .traffic[data-density='heavy'] { stroke-width: 4; stroke-dasharray: 6 6; animation-duration: 1.4s; }
  @keyframes flow { to { stroke-dashoffset: -48; } }
  /* The token standing on the rail: what waits to cross, right now. */
  .token { fill: var(--ink, #12161c); stroke: var(--border-strong, #3a434d); stroke-width: 1.5; }
  .token.busy { stroke: var(--warn, #d9a441); }
  .token.troubled { stroke: var(--err, #e2685c); }
  .yard text.token-count { font-size: 9px; letter-spacing: 0; text-transform: none;
    fill: var(--fog, #e8ecef); }
  .yard text.rate { fill: var(--static, #7a838c); }
  /* The machine's lamp above the rail. Unlit = nothing records it. */
  .glyph { fill: var(--ink, #12161c); stroke: var(--border-strong, #3a434d); }
  .glyph.ok { fill: var(--ok, #4fb98a); stroke: var(--ok, #4fb98a); }
  .glyph.err { fill: var(--err, #e2685c); stroke: var(--err, #e2685c); animation: blink 1s steps(2) infinite; }
  .crossing { cursor: help; }
  .tie { stroke: var(--tie); stroke-width: 6; stroke-dasharray: 3 9; fill: none; }
  .shed { fill: var(--ink, #12161c); stroke: var(--border-strong, #3a434d); }
  .shed.err { stroke: var(--err, #e2685c); }
  .shed.warn { stroke: var(--warn, #d9a441); }
  .machine { cursor: pointer; outline: none; }
  /* THE ZOOM. The camera is the viewBox (region-contents.ts); these are only
     what the zoom LOOKS like: the territories the camera left fade
     back, the one it is in does not, and the ground around it is the
     way out. No new tokens — the plates are YardMap's wagon body and
     its lamps. */
  .frame { fill: var(--void, #0d1014); cursor: zoom-out; }
  .territory.away { opacity: 0.35; }
  .territory.here .shed { stroke-width: 2; }
  .leave { cursor: zoom-out; fill: var(--static, #7a838c); font-size: 9px; letter-spacing: 0.08em; }
  .leave:hover, .leave:focus-visible { fill: var(--fog, #e8ecef); }
  .plate .wagon { fill: var(--ink, #12161c); stroke: var(--border-strong, #3a434d); }
  .plate .wagon.ok { stroke: var(--ok, #4fb98a); }
  .plate .wagon.warn { stroke: var(--warn, #d9a441); }
  .plate .wagon.red { stroke: var(--err, #e2685c); }
  .yard text.plate-tag { fill: var(--fog, #e8ecef); letter-spacing: 0; }
  /* THE PLATFORMS (car 4), in the same grammar: the track is a tie, a
     mark is a wagon body, the bound is a signal on the track. Every
     colour is a named token — a state or a surface never enters this
     file as a hex (42f66fb3). An UNKNOWN number gets its own band:
     dimmer than a figure and never the same ink as a 0, because the
     two are different facts. */
  .platform .track { stroke: var(--tie); stroke-width: 1; }
  .platform .mark { fill: var(--static); }
  .platform .mark.flagged { fill: var(--err); }
  .platform .bound { stroke: var(--warn); stroke-width: 1; }
  .yard text.standing { fill: var(--fog); letter-spacing: 0; }
  .yard text.rate { fill: var(--static); letter-spacing: 0; }
  .yard text.unknown { fill: var(--static); opacity: 0.6; font-style: italic; }
  .lamp.working { fill: var(--warn, #d9a441); }
  .lamp.off { fill: var(--border-strong, #3a434d); }
  .machine:hover .shed, .machine:focus-visible .shed { stroke: var(--fog, #e8ecef); }
  .lamp { fill: var(--border-strong, #3a434d); }
  .lamp.ok { fill: var(--ok, #4fb98a); }
  .lamp.warn { fill: var(--warn, #d9a441); }
  .lamp.err { fill: var(--err, #e2685c); animation: blink 1s steps(2) infinite; }
  @keyframes blink { 50% { opacity: 0.25; } }
  /* THE MACHINE GLYPHS (car 5). No colour is declared here: the
     housing is the map's own `.shed` and the parts inside it are its
     `.lamp` tones, so a state can only ever wear a token this surface
     already names. What IS declared here is the one thing that tells
     the four states apart without a legend — how the glyph BEHAVES. */
  .glyph.running .piston { animation: piston 1.1s ease-in-out infinite alternate; }
  /* Idle: solid, still, quiet. The housing is drawn, nothing is in it. */
  .glyph.idle .shed { opacity: 0.55; }
  .glyph.failed .shed { stroke-width: 1.5; }
  /* Unknown: a BROKEN outline and a mark — a different shape from
     idle, not a different shade of it, because shade alone does not
     survive world scale. */
  .glyph.unknown .shed { stroke-dasharray: 2 2; }
  .yard .glyph text.mark { font-size: 9px; letter-spacing: 0; }
  @keyframes piston { to { transform: translateX(4px); } }
  @media (prefers-reduced-motion: reduce) {
    .lamp, .glyph, .traffic { animation: none !important; }
    /* A still piston is still a FILLED housing, which idle never is —
       motion is the cue, the fill is the fallback. */
    .glyph .piston { animation: none !important; }
  }
</style>
