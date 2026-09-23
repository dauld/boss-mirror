<script lang="ts">
  // A REGION IS ITS OWN MAP (David, 2026-09-20; backlog ca37478f).
  //
  // "I just wanted the world map view to get replaced with the more
  // detailed region map view on click but not literally increase the
  // size of content on the world map."
  //
  // So this is not the world drawn nearer. It is the region's own
  // picture on its own canvas: the head that says what the region IS
  // (its count over its bound, its state, its why), what is standing
  // inside it, and the machinery that moves it. The world map draws
  // the world and nothing else; the two never share a viewBox, and
  // there is no camera between them — `/it/yard/<region>` renders
  // this, `/it` renders the world, and the route is the state.
  //
  // WHAT IT DRAWS IS UNCHANGED, and deliberately so. The wagon plates,
  // the platform tracks with their bounds and flags, and the machinery
  // glyphs are the same marks cars 3–5 built, off the same reads, with
  // the same ids and lamps the Train Yard uses. Only the rect they lay
  // out inside changed — from a slot on the world line to
  // `regionCanvas`, which every region gets equally.
  import { navigate } from '@boss/web-kit/nav';
  import {
    REGION_NAMES,
    countText,
    lampOf,
    trendText,
    type Region,
    type RegionName,
    type Regions,
  } from './regions';
  import { wrapWords } from './world';
  import { REGION_CANVAS, regionCanvas } from './region-canvas';
  import { hasInterior, interiorLayout, interiorWagons } from './region-contents';
  import { hasPlatforms, platformLayout, type Deck, type Platform } from './world-interior';
  import { machineTitle, machineryLabel, machineryStrip } from './world-machines';
  import type { Scene } from './yard-floor';

  type Props = Readonly<{
    /** The region this map is OF. The route's own value. */
    region: string;
    regions: Regions;
    /** The floor, for the wagons. Null until its read lands — the map
     *  then says it is reading rather than drawing an empty region,
     *  which would read as a calm one. */
    floor?: Scene | null;
    /** The queues standing in receiving or marshalling. Handed up by
     *  the page that owns the read, so this derives nothing. */
    deck?: Deck | null;
    /** Back to the world: Escape, or the control in the corner. */
    onleave?: () => void;
  }>;
  let { region, regions, floor = null, deck = null, onleave = () => navigate('/it') }: Props = $props();

  /** The region's rect IS its canvas. One definition of "lay your
   *  contents out in here", shared with the world's territories.
   *  The name is NARROWED against the registry rather than asserted:
   *  the page only mounts this for a region the layout knows, and a
   *  cast would make that guarantee invisible here. */
  const known = $derived(REGION_NAMES.find((n): n is RegionName => n === region) ?? null);
  const rect = $derived(regionCanvas(known ?? REGION_NAMES[0]));
  const r = $derived<Region | undefined>(regions.regions.find((x) => x.name === region));
  const state = $derived(r?.state ?? 'troubled');
  const troubled = $derived(state === 'troubled');
  const why = $derived(r?.why ?? 'the server answered no reading for this region');

  /** What is moving inside, placed in the region's own rect. */
  const interior = $derived.by(() => {
    if (floor === null || !hasInterior(region)) return null;
    return interiorLayout(rect, interiorWagons(floor, region));
  });
  const machinery = $derived(machineryStrip(rect, r?.machines ?? []));

  // The head is a region-scale block now, not a compact one squeezed
  // into a slot: the count, the state, the trend and the why all fit,
  // so nothing the world showed is lost on the way in.
  const HEAD_X = 16;
  const chars = Math.floor((REGION_CANVAS.width - 2 * HEAD_X) / 5.8);

  const standingText = (p: Platform): string =>
    p.standing === null ? '?' : p.bound === null ? String(p.standing) : `${p.standing} / ${p.bound}`;
  const rateText = (p: Platform): string => (p.rate === null ? '?' : `${p.rate}↓`);

  // Escape goes back to the world. Bound with addEventListener, NOT a
  // svelte:window tag: the bun+svelte bundler crashes on that tag's
  // event lookup and takes the whole app down (a lint refuses it,
  // src/shell/no-svelte-window.test.ts).
  $effect(() => {
    function onKeyDown(e: KeyboardEvent): void {
      if (e.key === 'Escape') onleave();
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  });
</script>

<section class="yard region-view" aria-label="the {region} region map" data-region={region} data-state={state}>
  <div class="region-head">
    <button class="leave" onclick={onleave} aria-label="back to the IT world">← the world</button>
    <span class="region-name">{region}</span>
    <span class="region-count">{r ? countText(r) : 'no reading'}</span>
    <span class="lamp-dot lamp {lampOf(state)}"></span>
    <span class="region-state" class:err={troubled}>{state}</span>
    {#if r}<span class="region-trend">{r.trend.metric} · {trendText(r.trend)}</span>{/if}
  </div>
  <div class="region-why" class:err={troubled}>{why}</div>

  <svg
    viewBox="0 0 {REGION_CANVAS.width} {REGION_CANVAS.height}"
    role="img"
    aria-label="{region} · {state} — {why}">
    <!-- The region's own outline: its whole canvas, not a slot. -->
    <rect
      x="0.5"
      y="0.5"
      width={REGION_CANVAS.width - 1}
      height={REGION_CANVAS.height - 1}
      class="shed"
      class:warn={state === 'busy'}
      class:err={troubled} />

    {#if hasPlatforms(region)}
      <!-- A REGION THAT HOLDS QUEUES. Receiving and marshalling stand
           no rolling stock in transit, so the interior is a platform
           per queue, with its packets on the track, its bound drawn
           where the bound falls, and the packets past it flagged. -->
      {#if deck === null || deck.kind === 'reading'}
        <text x={HEAD_X} y="40" class="tiny">reading what is standing here…</text>
      {:else if deck.kind === 'unavailable'}
        <!-- a read that failed is a failure, never an empty region -->
        <text x={HEAD_X} y="40" class="tiny err why">
          {#each wrapWords(`the queues cannot be read — ${deck.why}`, chars, 3) as line, i (i)}
            <tspan x={HEAD_X} dy={i === 0 ? 0 : 12}>{line}</tspan>
          {/each}
        </text>
      {:else if deck.platforms.length === 0}
        <text x={HEAD_X} y="40" class="tiny">no queue is declared here</text>
      {:else}
        {@const laid = platformLayout(rect, deck.platforms)}
        <g class="interior" data-interior={region}>
          {#each laid.placed as p (p.platform.name)}
            <g class="platform" data-platform={p.platform.name}>
              <title
                >{p.platform.name} — {p.platform.note}{p.perMark > 1
                  ? ` · the track is the whole queue: one mark is ${Math.round(p.perMark)} packets`
                  : ''}</title>
              <text x={p.x} y={p.y + 8} class="tiny plate-tag">{p.platform.name}</text>
              <text
                x={p.x + p.w}
                y={p.y + 8}
                text-anchor="end"
                class="tiny standing"
                class:err={p.platform.flag.n > 0}
                class:unknown={p.platform.standing === null}>{standingText(p.platform)}</text>
              <line class="track" x1={p.x} y1={p.trackY + 9} x2={p.x + p.w} y2={p.trackY + 9} />
              {#each p.marks as m, i (i)}
                <rect class="mark" class:flagged={m.flagged} x={m.x} y={m.y} width={m.w} height={m.h} />
              {/each}
              {#if p.boundX !== null}
                <line class="bound" x1={p.boundX} y1={p.trackY - 2} x2={p.boundX} y2={p.trackY + 10} />
              {/if}
              <text
                x={p.x + p.w}
                y={p.trackY + 7}
                text-anchor="end"
                class="tiny rate"
                class:unknown={p.platform.rate === null}>{rateText(p.platform)}</text>
            </g>
          {/each}
          {#if laid.hidden > 0}
            <!-- what did not fit is COUNTED, never quietly dropped -->
            <text x={REGION_CANVAS.width - HEAD_X} y={REGION_CANVAS.height - 6} text-anchor="end" class="tiny"
              >+{laid.hidden} more</text>
          {/if}
        </g>
      {/if}
    {:else if interior === null}
      <text x={HEAD_X} y="40" class="tiny">reading what is inside…</text>
    {:else if interior.placed.length === 0}
      <text x={HEAD_X} y="40" class="tiny">nothing is standing here</text>
    {:else}
      <!-- WHAT IS MOVING INSIDE. One plate per wagon standing at a
           station this region covers, off the floor's own Scene — same
           ids, same tags, same lamps the Train Yard draws, because they
           are the same wagons. Keyed on the wagon id, so a wagon that
           moves between polls moves rather than being rebuilt. -->
      <g class="interior" data-interior={region}>
        {#each interior.placed as p (p.wagon.id)}
          <g class="plate" data-car={p.wagon.id} data-station={p.wagon.station}>
            <title>{p.wagon.title} — {p.wagon.status}</title>
            <rect x={p.x} y={p.y} width={p.w} height={p.h} class="wagon {p.wagon.tone}" />
            <circle cx={p.x + 7} cy={p.y + p.h / 2} r="3" class="lamp {p.wagon.lamp}" />
            <text x={p.x + 14} y={p.y + p.h / 2 + 3} class="tiny plate-tag">{p.wagon.tag}</text>
          </g>
        {/each}
        {#if interior.hidden > 0}
          <text x={REGION_CANVAS.width - HEAD_X} y={REGION_CANVAS.height - 6} text-anchor="end" class="tiny"
            >+{interior.hidden} more</text>
        {/if}
      </g>
    {/if}

    <!-- THE MACHINERY (car 5). The region's actors, read from the
         registries and judged on the SERVER. The glyph says the state
         without a legend: running MOVES, idle is still and solid,
         failed blinks red, unknown is a broken outline with a mark.
         Idle and unknown are told apart by SHAPE, not shade. -->
    {#if machinery.placed.length > 0}
      <g class="machinery" data-machinery={region} aria-label={machineryLabel(r?.machines ?? [])}>
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
      </g>
    {/if}
  </svg>
</section>

<style>
  /* The yard's own classes — the same names WorldMap and YardMap use,
     because Svelte scopes styles per component and a region map must
     look like the world it came out of. Every colour is the same
     --map-* token WorldMap reads, with no fallback, so no state or
     surface appears as a hex (42f66fb3, map-palette.test.ts). */
  .yard {
    --rail: var(--map-rule-strong);
    --tie: var(--map-rule);
    background: var(--map-bg);
    border: 1px solid var(--map-rule);
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
    fill: var(--map-muted);
    font-size: 10px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .yard text.tiny { font-size: 9px; letter-spacing: 0.04em; text-transform: none; }
  .yard text.err { fill: var(--map-bad-ink); }
  .shed { fill: var(--map-surface); stroke: var(--map-rule-strong); }
  .shed.err { stroke: var(--map-bad-edge); }
  .shed.warn { stroke: var(--map-warn-edge); }

  /* THE HEAD is HTML, not SVG. On the world line it had to be text
     inside an outline, wrapped by hand to the slot's width; a region
     map has a page to put it on, so the name, the count, the state and
     the trend are a real row that wraps itself. */
  .region-head {
    display: flex;
    align-items: baseline;
    gap: var(--s3, 12px);
    flex-wrap: wrap;
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .leave {
    background: none;
    border: 1px solid var(--map-rule);
    color: var(--map-muted);
    font: inherit;
    font-size: 11px;
    letter-spacing: var(--ls-nav, 0.14em);
    padding: 4px 10px;
    cursor: pointer;
  }
  .leave:hover, .leave:focus-visible { color: var(--map-ink); border-color: var(--map-rule-strong); }
  .region-name { font-size: 15px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--map-ink); }
  .region-count { font-size: 18px; font-weight: 600; color: var(--map-ink); }
  .region-state { font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--map-muted); }
  .region-state.err { color: var(--map-bad-ink); }
  .region-trend { font-size: 11px; color: var(--map-muted); }
  .region-why { font-family: var(--font-mono, ui-monospace, monospace); font-size: 12px;
    color: var(--map-muted); margin-top: var(--s2, 8px); }
  .region-why.err { color: var(--map-bad-ink); }
  .lamp-dot { width: 8px; height: 8px; border-radius: 50%; display: inline-block;
    background: var(--map-rule-strong); }
  .lamp-dot.ok { background: var(--map-ok-edge); }
  .lamp-dot.warn { background: var(--map-warn-edge); }
  .lamp-dot.err { background: var(--map-bad-edge); animation: blink 1s steps(2) infinite; }

  .plate .wagon { fill: var(--map-surface); stroke: var(--map-rule-strong); }
  .plate .wagon.ok { stroke: var(--map-ok-edge); }
  .plate .wagon.warn { stroke: var(--map-warn-edge); }
  .plate .wagon.red { stroke: var(--map-bad-edge); }
  .yard text.plate-tag { fill: var(--map-ink); letter-spacing: 0; }
  .platform .track { stroke: var(--tie); stroke-width: 1; }
  .platform .mark { fill: var(--map-muted); }
  .platform .mark.flagged { fill: var(--map-bad-edge); }
  .platform .bound { stroke: var(--map-warn-edge); stroke-width: 1; }
  .yard text.standing { fill: var(--map-ink); letter-spacing: 0; }
  .yard text.rate { fill: var(--map-muted); letter-spacing: 0; }
  .yard text.unknown { fill: var(--map-muted); opacity: 0.6; font-style: italic; }
  .lamp { fill: var(--map-rule-strong); }
  .lamp.ok { fill: var(--map-ok-edge); }
  .lamp.warn { fill: var(--map-warn-edge); }
  .lamp.err { fill: var(--map-bad-edge); animation: blink 1s steps(2) infinite; }
  @keyframes blink { 50% { opacity: 0.25; } }
  /* The machine glyphs, as car 5 declared them: the housing is the
     map's own `.shed`, the parts are its `.lamp` tones, and what tells
     the four states apart without a legend is how the glyph BEHAVES. */
  .glyph.running .piston { animation: piston 1.1s ease-in-out infinite alternate; }
  .glyph.idle .shed { opacity: 0.55; }
  .glyph.failed .shed { stroke-width: 1.5; }
  .glyph.unknown .shed { stroke-dasharray: 2 2; }
  .yard .glyph text.mark { font-size: 9px; letter-spacing: 0; }
  @keyframes piston { to { transform: translateX(4px); } }
  @media (prefers-reduced-motion: reduce) {
    .lamp, .lamp-dot, .glyph { animation: none !important; }
    /* A still piston is still a FILLED housing, which idle never is. */
    .glyph .piston { animation: none !important; }
  }
</style>
