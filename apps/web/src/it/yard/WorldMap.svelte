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
  // them. The arithmetic of both halves is world-zoom.ts, unit-pinned;
  // this owns the animation frames and the strokes. NO NEW STYLING —
  // the classes here are YardMap's, by name and by token, so the
  // visual redesign reskins one grammar (Q1, decided 2026-09-19).
  import { untrack } from 'svelte';
  import { navigate } from '@boss/web-kit/nav';
  import { countText, floorHref, lampOf, trendText, type Region, type Regions } from './regions';
  import { BORDERS, TERRITORIES, WORLD, borderPath, territoryOf, wrapWords, type Territory } from './world';
  import {
    WORLD_BOX,
    ZOOM_MS,
    easeInOut,
    hasInterior,
    interiorLayout,
    interiorWagons,
    lerpBox,
    viewBoxText,
    zoomBoxOf,
    type Box,
  } from './world-zoom';
  import type { Scene } from './yard-floor';

  type Props = Readonly<{
    regions: Regions;
    /** The region the camera is in, or null for the whole world. */
    zoomed?: string | null;
    /** The floor, for the interiors. Null until the floor's reads land
     *  — a zoomed territory then says so rather than drawing an empty
     *  region, which would read as a calm one. */
    floor?: Scene | null;
    /** Back to the world: Escape, or the frame around the territory. */
    onleave?: () => void;
  }>;
  let { regions, zoomed = null, floor = null, onleave = () => {} }: Props = $props();

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

  // THE CAMERA. `view` is the viewBox the SVG is showing; it walks to
  // the box the route asks for over ZOOM_MS. It is SEEDED at that box,
  // so a /it/yard/<region> load lands zoomed rather than flying in
  // from a world nobody asked to see — every later change is the
  // effect's job, not the seed's, which is what `untrack` says here.
  // `untrack` on the effect's read of `view` is load-bearing for a
  // different reason: an effect that writes `view` must not depend on
  // it, or it re-runs itself forever.
  let view = $state<Box>(zoomBoxOf(untrack(() => zoomed)));
  const reducedMotion = (): boolean =>
    typeof window !== 'undefined' &&
    typeof window.matchMedia === 'function' &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  $effect(() => {
    const target = zoomBoxOf(zoomed);
    const from = untrack(() => view);
    if (viewBoxText(from) === viewBoxText(target)) return;
    if (reducedMotion() || typeof requestAnimationFrame !== 'function') {
      view = target;
      return;
    }
    let raf = 0;
    const t0 = performance.now();
    const step = (now: number): void => {
      const k = Math.min(1, (now - t0) / ZOOM_MS);
      view = lerpBox(from, target, easeInOut(k));
      if (k < 1) raf = requestAnimationFrame(step);
    };
    raf = requestAnimationFrame(step);
    return () => cancelAnimationFrame(raf);
  });

  const zoomedIn = $derived(zoomed !== null && territoryOf(zoomed) !== undefined);
  /** What is moving inside the zoomed territory, placed in its rect. */
  const interior = $derived.by(() => {
    if (zoomed === null || floor === null) return null;
    const t = territoryOf(zoomed);
    if (t === undefined || !hasInterior(zoomed)) return null;
    return interiorLayout(t, interiorWagons(floor, zoomed));
  });

  function open(e: MouseEvent, href: string, name: string): void {
    e.preventDefault();
    // Already here: a second click on the territory the camera is in
    // is not a navigation — the world would reload the same route and
    // the zoom would read as a flicker.
    if (name === zoomed) return;
    navigate(href);
  }

  // Escape zooms out. Bound with addEventListener, NOT a svelte:window
  // tag: the bun+svelte bundler crashes on that tag's event lookup and
  // takes the whole app down (a lint refuses it, src/shell/
  // no-svelte-window.test.ts).
  $effect(() => {
    function onKeyDown(e: KeyboardEvent): void {
      if (e.key === 'Escape' && zoomed !== null) onleave();
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  });
</script>

<section class="yard" aria-label="the IT world map" class:zoomed={zoomedIn}>
  <svg
    viewBox={viewBoxText(view)}
    data-zoomed={zoomed ?? ''}
    role="img"
    aria-label={zoomedIn
      ? `the IT world, zoomed into ${zoomed}`
      : 'the IT world: eight territories along the packet flow'}>
    <!-- THE FRAME. Zoomed, the ground around the territory is the way
         back out: clicking it (or Escape) returns to the world. Drawn
         first, so every outline and rail sits on top of it. -->
    {#if zoomedIn}
      <rect
        class="frame"
        x={WORLD_BOX.x - WORLD.width}
        y={WORLD_BOX.y - WORLD.height}
        width={WORLD.width * 3}
        height={WORLD.height * 3}
        role="button"
        tabindex="0"
        aria-label="back to the world"
        onclick={onleave}
        onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && onleave()} />
    {/if}
    <!-- the borders: a track segment per hop of the flow, under the
         territories so an outline sits on the rail -->
    {#each BORDERS as b (`${b.from}→${b.to}`)}
      {@const from = territoryOf(b.from)}
      {@const to = territoryOf(b.to)}
      {#if from && to}
        {@const p = borderPath(from, to)}
        <path d={p.d} class="tie" data-border="{b.from}→{b.to}" />
        <path d={p.d} class="rail" />
      {/if}
    {/each}

    <!-- the territories: an outline each, the region's numbers inside -->
    {#each TERRITORIES as t (t.name)}
      {@const r = byName.get(t.name)}
      {@const state = stateOf(r)}
      {@const troubled = state === 'troubled'}
      {@const here = t.name === zoomed}
      <a
        class="territory machine"
        class:here
        class:away={zoomedIn && !here}
        data-region={t.name}
        data-state={state}
        href={floorHref(t.name)}
        aria-label="{t.name} · {state} — {whyOf(r)}"
        onclick={(e) => open(e, floorHref(t.name), t.name)}>
        <title>{t.name} · {state} — {whyOf(r)}</title>
        <rect x={t.x} y={t.y} width={t.w} height={t.h} class="shed" class:warn={state === 'busy'} class:err={troubled} />
        <text x={t.x + 8} y={t.y + 18}>{t.name}</text>
        {#if here}
          <!-- ZOOMED: a compact head — the count, the state and the
               why on one block — and the rest of the rect goes to what
               is moving inside. The trend belongs to the world's
               reading of the region and is read at the world; in here
               the room is worth more as wagons. -->
          <text x={t.x + 8} y={t.y + 34} class="count">{r ? countText(r) : 'no reading'}</text>
          <circle cx={t.x + 12} cy={t.y + 46} r="4" class="lamp {lampOf(state)}" />
          <text x={t.x + 22} y={t.y + 50} class:err={troubled}>{state}</text>
          <text x={t.x + 8} y={t.y + 62} class="tiny why" class:err={troubled}>
            {#each wrapWords(whyOf(r), chars(t), 1) as line, i (i)}
              <tspan x={t.x + 8} dy={i === 0 ? 0 : 12}>{line}</tspan>
            {/each}
          </text>
          <!-- WHAT IS MOVING INSIDE. One plate per wagon standing at a
               station this territory covers, off the floor's own Scene
               — same ids, same tags, same lamps as the Train Yard
               draws, because they are the same wagons. A plate is keyed
               on the wagon id, so a wagon that moves between polls
               moves rather than being torn down and rebuilt. -->
          {#if !hasInterior(t.name)}
            <!-- receiving and marshalling keep pages of their own until
                 car 4 grows their interiors; say so rather than draw an
                 empty region, which would read as an idle one. -->
            <text x={t.x + 8} y={t.y + 84} class="tiny">this region's floor is a page of its own</text>
          {:else if interior === null}
            <text x={t.x + 8} y={t.y + 84} class="tiny">reading what is inside…</text>
          {:else if interior.placed.length === 0}
            <text x={t.x + 8} y={t.y + 84} class="tiny">nothing is standing here</text>
          {:else}
            <g class="interior" data-interior={t.name}>
              {#each interior.placed as p (p.wagon.id)}
                <g class="plate" data-car={p.wagon.id} data-station={p.wagon.station}>
                  <title>{p.wagon.title} — {p.wagon.status}</title>
                  <rect x={p.x} y={p.y} width={p.w} height={p.h} class="wagon {p.wagon.tone}" />
                  <circle cx={p.x + 7} cy={p.y + p.h / 2} r="3" class="lamp {p.wagon.lamp}" />
                  <text x={p.x + 14} y={p.y + p.h / 2 + 3} class="tiny plate-tag">{p.wagon.tag}</text>
                </g>
              {/each}
              {#if interior.hidden > 0}
                <!-- what did not fit is COUNTED, never quietly dropped -->
                <text x={t.x + t.w - 8} y={t.y + t.h - 6} text-anchor="end" class="tiny"
                  >+{interior.hidden} more</text>
              {/if}
            </g>
          {/if}
        {:else}
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
        {/if}
      </a>
    {/each}

    <!-- THE WAY OUT, said rather than guessed: an affordance at the
         head of the zoomed territory, because Escape is not
         discoverable and the frame is a thin ring at this zoom. -->
    {#if zoomedIn}
      {@const t = territoryOf(zoomed ?? '')}
      {#if t}
        <text
          class="leave"
          x={t.x}
          y={t.y - 6}
          role="button"
          tabindex="0"
          aria-label="back to the world"
          onclick={onleave}
          onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && onleave()}>← the world (Esc)</text>
      {/if}
    {/if}

    {#if unmapped.length > 0}
      <text x={WORLD.width - 20} y={WORLD.height - 8} text-anchor="end" class="tiny err"
        >not on the map: {unmapped.join(', ')}</text>
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
  .tie { stroke: var(--tie); stroke-width: 6; stroke-dasharray: 3 9; fill: none; }
  .shed { fill: var(--ink, #12161c); stroke: var(--border-strong, #3a434d); }
  .shed.err { stroke: var(--err, #e2685c); }
  .shed.warn { stroke: var(--warn, #d9a441); }
  .machine { cursor: pointer; outline: none; }
  /* THE ZOOM. The camera is the viewBox (world-zoom.ts); these are only
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
  .lamp.working { fill: var(--warn, #d9a441); }
  .lamp.off { fill: var(--border-strong, #3a434d); }
  .machine:hover .shed, .machine:focus-visible .shed { stroke: var(--fog, #e8ecef); }
  .lamp { fill: var(--border-strong, #3a434d); }
  .lamp.ok { fill: var(--ok, #4fb98a); }
  .lamp.warn { fill: var(--warn, #d9a441); }
  .lamp.err { fill: var(--err, #e2685c); animation: blink 1s steps(2) infinite; }
  @keyframes blink { 50% { opacity: 0.25; } }
  @media (prefers-reduced-motion: reduce) {
    .lamp { animation: none !important; }
  }
</style>
