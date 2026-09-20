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
  // The arithmetic of all three is world-zoom.ts and world-interior.ts, unit-pinned;
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
  import { hasPlatforms, platformLayout, type Deck, type Platform } from './world-interior';
  import type { Scene } from './yard-floor';

  type Props = Readonly<{
    regions: Regions;
    /** The region the camera is in, or null for the whole world. */
    zoomed?: string | null;
    /** The floor, for the interiors. Null until the floor's reads land
     *  — a zoomed territory then says so rather than drawing an empty
     *  region, which would read as a calm one. */
    floor?: Scene | null;
    /** The queues standing in a zoomed receiving or marshalling — the
     *  platform deck those two regions show instead of wagons in
     *  transit (car 4). Handed up by the page that owns the read, so
     *  the map derives nothing; null for every other territory. */
    deck?: Deck | null;
    /** Back to the world: Escape, or the frame around the territory. */
    onleave?: () => void;
    /** The rails' readings, or null while unread or unreadable: the
     *  rails still DRAW — the layout is the map — but every number on
     *  them then reads unknown rather than zero (car 2). */
    borders?: Borders | null;
  }>;
  let {
    regions,
    zoomed = null,
    floor = null,
    deck = null,
    onleave = () => {},
    borders = null,
  }: Props = $props();

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
  /** What stands on a platform, over its bound where it has one. A
   *  count nobody could take is `?` — it gets the unknown band, and it
   *  is not the same picture as a 0 (car 2's rule, the same token). */
  const standingText = (p: Platform): string =>
    p.standing === null ? '?' : p.bound === null ? String(p.standing) : `${p.standing} / ${p.bound}`;
  /** What LEFT the queue in the window, or `?` where the log could not
   *  be made to count it — never a 0, which on a queue reads as
   *  "nothing is being worked". */
  const rateText = (p: Platform): string => (p.rate === null ? '?' : `${p.rate}\u2193`);
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
          {#if hasPlatforms(t.name)}
            <!-- A REGION THAT HOLDS QUEUES (car 4). Receiving and
                 marshalling stand no rolling stock in transit, so their
                 interior is a platform per queue — a station here, a
                 channel there — with its packets standing on the track,
                 its bound drawn where the bound falls, and the packets
                 past it flagged. The numbers are the region's own read
                 model, handed up by the page below; the marks are the
                 count drawn, and the count beside the name is the
                 figure to read when a track runs out of room. -->
            {#if deck === null || deck.kind === 'reading'}
              <text x={t.x + 8} y={t.y + 84} class="tiny">reading what is standing here…</text>
            {:else if deck.kind === 'unavailable'}
              <!-- a read that failed is a failure, never an empty region -->
              <text x={t.x + 8} y={t.y + 84} class="tiny err why">
                {#each wrapWords(`the queues cannot be read — ${deck.why}`, chars(t), 3) as line, i (i)}
                  <tspan x={t.x + 8} dy={i === 0 ? 0 : 12}>{line}</tspan>
                {/each}
              </text>
            {:else if deck.platforms.length === 0}
              <text x={t.x + 8} y={t.y + 84} class="tiny">no queue is declared here</text>
            {:else}
              {@const laid = platformLayout(t, deck.platforms)}
              <g class="interior" data-interior={t.name}>
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
                      <!-- the WIP bound, where it falls on the track -->
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
                  <text x={t.x + t.w - 8} y={t.y + t.h - 6} text-anchor="end" class="tiny"
                    >+{laid.hidden} more</text>
                {/if}
              </g>
            {/if}
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
  @media (prefers-reduced-motion: reduce) {
    .lamp, .glyph, .traffic { animation: none !important; }
  }
</style>
