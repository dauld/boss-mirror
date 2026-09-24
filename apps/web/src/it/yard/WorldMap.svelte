<script lang="ts">
  // THE IT WORLD MAP (design d2154293, car 1). One SVG in one
  // coordinate space: world.ts declares the regions as
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
  //
  // THE BORDERS ARE DRAWN, NOT TOOLTIPPED (design 62de32ae decision 6,
  // decided 2026-09-24). Each rail runs along the main line above the
  // territories (world.ts `railOf`) with the machine that moves it
  // WRITTEN on it — its name, its lamp, its status — the waiting count
  // standing on it and the rate beneath; the rail itself is as wide as
  // its rate. A click opens the crossing inline, under the map: what
  // one crossing is, the rate against the window before, the machine
  // with where its reading came from, and every packet waiting with its
  // own reason. All of it used to ride native hover titles, which touch
  // cannot reach and a screenshot cannot hold. The panel is the one
  // piece of new styling here, and it names no colour but a --map-*
  // token, so the redesign still reskins one grammar.
  import { navigate } from '@boss/web-kit/nav';
  import {
    bandText,
    compactCountText,
    countText,
    floorHref,
    kpiText,
    lampOf,
    stateText,
    trendText,
    type Region,
    type Regions,
  } from './regions';
  import {
    densityOf,
    crossedText,
    machineStatus,
    machineText,
    railWidth,
    rateText as borderRateText,
    unlistedText,
    waitingText,
    type Border,
    type Borders,
  } from './borders';
  import {
    BORDERS,
    TERRITORIES,
    WORLD,
    labelChars,
    railOf,
    railWriting,
    territoryOf,
    territoryText,
    wrapName,
    wrapWords,
    type TerritoryText,
  } from './world';
  import { machineTitle, machineryLabel, machineryStrip } from './world-machines';
  import { flightOn } from '@boss/web-kit/session/flights.svelte';
  import { MediaQuery } from 'svelte/reactivity';
  import MotionLayer from './MotionLayer.svelte';
  import {
    COMPRESSIONS,
    DEFAULT_COMPRESSION,
    REPLAY_TEXT,
    compressionText,
    emitPerSec,
    heldMs,
    heldText,
    motionRateText,
    pathPoints,
    railStill,
    staleFor,
    walk,
  } from './world-motion';

  type Props = Readonly<{
    regions: Regions;
    /** The rails' readings, or null while unread or unreadable: the
     *  rails still DRAW — the layout is the map — but every number on
     *  them then reads unknown rather than zero (car 2). */
    borders?: Borders | null;
    /** When (this page's clock, ms) the rails were last read WELL — the
     *  moving map's held clocks count on from it, and it greys past
     *  three missed reads. */
    bordersAt?: number | null;
  }>;
  let { regions, borders = null, bordersAt = null }: Props = $props();

  // THE MAP MOVES — behind its flight (design 31bade8f, car M2; flight
  // design c4c2a607). Off, and for every viewer the flight does not
  // list, the map below is exactly the map it was: no canvas, no bar,
  // the CSS traffic dashes. On, the MotionLayer canvas replays each
  // rail's measured rate over it, the piles stand at the rails' heads,
  // a rail the server judges still stops and says for how long, and
  // nothing decorative moves — no dashes, no blinking lamps.
  const motion = $derived(flightOn('it-map-motion'));
  /** ×600 by default, and stated (Q1, decided 2026-09-24). */
  let compression = $state<number>(DEFAULT_COMPRESSION);
  /** The reader's reduced-motion setting, and the page's own toggle
   *  over it (decision 9). */
  const prefersReduced = new MediaQuery('(prefers-reduced-motion: reduce)');
  let reducedChoice = $state<boolean | null>(null);
  const reduced = $derived(reducedChoice ?? prefersReduced.current);
  /** This page's clock, for the held clocks and the missed-read check:
   *  a tick a second, and the clocks it drives move once a minute under
   *  reduced motion. Only while the flight is on. */
  let nowMs = $state(Date.now());
  $effect(() => {
    if (!motion) return;
    const t = setInterval(() => (nowMs = Date.now()), 1_000);
    return () => clearInterval(t);
  });
  const clockNow = $derived(reduced ? Math.floor(nowMs / 60_000) * 60_000 : nowMs);
  const staleSecs = $derived(motion ? staleFor(bordersAt, nowMs) : null);
  /** Each rail's run, for the "●=k" its tokens stand for. */
  const railLength = (d: string): number => walk(pathPoints(d)).length;
  const rateOnRail = (row: Border | undefined, d: string): string =>
    motionRateText(row, emitPerSec(row?.rate.current ?? null, compression > 0 ? compression : DEFAULT_COMPRESSION, railLength(d)));
  const heldOnRail = (row: Border | undefined): string =>
    row === undefined || bordersAt === null
      ? ''
      : heldText(row, railStill(row), heldMs(row, borders?.now ?? '', bordersAt, clockNow));

  const key = (from: string, to: string): string => `${from}→${to}`;
  const byBorder = $derived(new Map((borders?.borders ?? []).map((b) => [key(b.from, b.to), b] as const)));
  /** A border the server answered that the layout does not draw — said
   *  at the foot of the map beside an unmapped region, never dropped. */
  const unmappedBorders = $derived(
    (borders?.borders ?? [])
      .filter((b) => !BORDERS.some((d) => d.from === b.from && d.to === b.to))
      .map((b) => key(b.from, b.to)),
  );

  /** The rail each declared border draws, derived from the two
   *  territories it joins — never placed by hand. */
  const rails = BORDERS.flatMap((b) => {
    const from = territoryOf(b.from);
    const to = territoryOf(b.to);
    return from && to ? [{ key: key(b.from, b.to), from: b.from, to: b.to, at: from, rail: railOf(from, to) }] : [];
  });
  /** The rails as the motion layer walks them: the path and the
   *  territory whose corner the pile stands in. */
  const motionRails = rails.map((r) => ({ key: r.key, rail: r.rail, from: r.at }));

  /** The crossing whose panel is open under the map, by its key; null
   *  when none is. A second click on the same rail closes it. */
  let opened = $state<string | null>(null);
  const toggle = (k: string): void => {
    opened = opened === k ? null : k;
  };
  const onKey = (e: KeyboardEvent, k: string): void => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      toggle(k);
    } else if (e.key === 'Escape') {
      opened = null;
    }
  };

  /** What the badge prints: the queue depth, or `?` for a count the
   *  server could not take. Never 0 for "I do not know". */
  const tokenText = (b: Border | undefined): string =>
    b === undefined || b.waiting === null ? '?' : String(b.waiting);
  /** The badge is as wide as its count. */
  const badgeW = (b: Border | undefined): number => 10 + 6 * tokenText(b).length;

  /** `3/d`, or `?` — the rate under the rail. The comparison with the
   *  window before is in the crossing's panel. */
  const railRate = (b: Border | undefined): string =>
    b === undefined || b.rate.current === null ? '?' : `${Math.round(b.rate.current * 10) / 10}/d`;

  const densityFor = (b: Border | undefined) => densityOf(b === undefined ? null : b.rate.current);
  const widthFor = (b: Border | undefined): number => railWidth(b === undefined ? null : b.rate.current);
  const stateOfBorder = (b: Border | undefined) => b?.state ?? 'troubled';
  /** The machine's lamp: its own silence when it declares a cadence,
   *  otherwise unlit — an unlit lamp is "cannot tell", not "fine". */
  const machineLamp = (b: Border | undefined): string =>
    b === undefined || b.machine.silent === null ? 'unknown' : b.machine.silent ? 'err' : 'ok';
  /** The machine's name as the rail writes it — the name the server
   *  sent, broken after a hyphen when it will not fit on one line. */
  const nameOf = (b: Border | undefined): string => b?.machine.name ?? 'no reading';
  const statusOf = (b: Border | undefined): string => (b === undefined ? 'no reading' : machineStatus(b.machine));
  /** The one-line account a screen reader gets for a rail. */
  const railLabel = (b: Border | undefined, from: string, to: string): string =>
    b === undefined
      ? `${from} → ${to} — the borders read answered nothing for this rail`
      : `${from} → ${to} · ${b.state} — ${b.why}. ${waitingText(b)}; ${machineText(b.machine)}. Open the crossing.`;

  const byName = $derived(new Map(regions.regions.map((r) => [r.name, r] as const)));
  /** A region the server answered that the layout has no territory
   *  for is said at the foot of the map, never dropped: the layout is
   *  pinned to the server's list, but a newer server is not a blank. */
  const unmapped = $derived(regions.regions.filter((r) => territoryOf(r.name) === undefined).map((r) => r.name));

  // The text inside an outline is laid out by world.ts `territoryText`
  // — one column on the line, two on a siding — so every line of it is
  // pinned inside its box and above the machinery strip.
  const stateOf = (r: Region | undefined) => r?.state ?? 'troubled';
  const whyOf = (r: Region | undefined) => r?.why ?? 'the server answered no reading for this region';
  /** What a non-clear territory says beside its trend: the band that
   *  decided it, read against its number (design 62de32ae, decision 1),
   *  and for trouble the why after it — as many lines as the layout
   *  leaves above the machinery strip. A payload with no band (an older
   *  server) gives the why every line. */
  function verdictLines(lt: TerritoryText, r: Region | undefined): ReadonlyArray<string> {
    const state = stateOf(r);
    if (state === 'clear') return [];
    const n = lt.verdictLines;
    const band = bandText(r);
    if (band === null) return wrapWords(whyOf(r), lt.verdictChars, n);
    const lines = wrapWords(band, lt.verdictChars, 2);
    return state === 'troubled' ? [...lines, ...wrapWords(whyOf(r), lt.verdictChars, n - lines.length)] : lines;
  }
  /** The hover and the screen reader get everything the outline has no
   *  room for: the count in its unit, the state with how long it has
   *  held, the band that decided it, the KPI and the why. */
  const titleOf = (name: string, r: Region | undefined): string =>
    r === undefined
      ? `${name} · troubled — ${whyOf(r)}`
      : [
          `${name} · ${countText(r)} · ${stateText(r)}${bandText(r) ? ` [${bandText(r)}]` : ''} — ${r.why}`,
          kpiText(r),
        ]
          .filter((s) => s !== '')
          .join('\n');

  function open(e: MouseEvent, href: string): void {
    e.preventDefault();
    navigate(href);
  }
</script>

<!-- The world itself, one SVG — rendered alone with the flight off, and
     under the motion layer with it on, so off is the map it always was. -->
{#snippet world()}
  <svg
    viewBox="0 0 {WORLD.width} {WORLD.height}"
    role="img"
    aria-label="the IT world: the territories along the packet flow">
    <!-- the borders: a rail per hop of the flow, along the main line
         above the territories (world.ts `railOf`), drawn under them so
         each drop meets its outline -->
    {#each rails as r (r.key)}
      {@const row = byBorder.get(r.key)}
      {@const w = widthFor(row)}
      <path d={r.rail.d} class="tie" data-border={r.key} />
      <!-- THE RAIL IS AS WIDE AS ITS RATE (decision 6) — logarithmic,
           a hairline when nothing crossed, and an unmeasured rate its
           own dotted band, never the empty one. -->
      <!-- On the moving map the rail also carries the server's stillness
           (design 31bade8f decision 4): a held rail is drawn in trouble
           red whatever its state, because stillness is the stall. -->
      <path d={r.rail.d} class="rail" data-rail={r.key} data-state={stateOfBorder(row)} data-width={w}
        data-still={motion ? (railStill(row) ?? 'moving') : undefined} style="stroke-width: {w}" />
      <!-- the traffic itself: dashes running along the rail in the
           direction of travel, their pace from the measured rate -->
      <path
        d={r.rail.d}
        class="traffic"
        data-traffic={r.key}
        data-density={densityFor(row)}
        style="stroke-width: {Math.max(1.5, w - 1)}" />
    {/each}

    <!-- the territories: an outline each, the region's numbers inside -->
    {#each TERRITORIES as t (t.name)}
      {@const r = byName.get(t.name)}
      {@const state = stateOf(r)}
      {@const troubled = state === 'troubled'}
      {@const machinery = machineryStrip(t, r?.machines ?? [])}
      {@const lt = territoryText(t)}
      <a
        class="territory machine"
        data-region={t.name}
        data-state={state}
        href={floorHref(t.name)}
        aria-label={titleOf(t.name, r)}
        onclick={(e) => open(e, floorHref(t.name))}>
        <title>{titleOf(t.name, r)}</title>
        <rect x={t.x} y={t.y} width={t.w} height={t.h} class="shed" class:warn={state === 'attention'} class:err={troubled} />
        <text x={lt.name.x} y={lt.name.y}>{t.name}</text>
        <text x={lt.count.x} y={lt.count.y} class="count">{r ? compactCountText(r) : 'no reading'}</text>
        <circle cx={lt.lamp.x} cy={lt.lamp.y} r="4" class="lamp {lampOf(state)}" />
        <!-- the state with how long the record says it has held
             (design 62de32ae, decision 2): "troubled for 16m" -->
        <text x={lt.state.x} y={lt.state.y} class="state" class:err={troubled}>{stateText(r)}</text>
        {#if r}
          <!-- THE KPI, each measure in its unit (decision 9), as the
               server wrote it — the region's one number to read -->
          {#if r.kpi.length > 0}
            <text x={lt.kpi.x} y={lt.kpi.y} class="tiny kpi">
              {#each wrapWords(r.kpi[0]!.text, lt.chars, lt.kpiLines) as line, i (i)}
                <tspan x={lt.kpi.x} dy={i === 0 ? 0 : 12}>{line}</tspan>
              {/each}
            </text>
          {/if}
          <!-- the trend as the card printed it, one part per line so
               the samples do not run past the outline -->
          <text x={lt.metric.x} y={lt.metric.y} class="tiny">{r.trend.metric}</text>
          <text x={lt.trend.x} y={lt.trend.y} class="tiny trend">
            {#each trendText(r.trend).split(' · ').slice(0, lt.trendLines) as part, i (i)}
              <tspan x={lt.trend.x} dy={i === 0 ? 0 : 12}>{part}</tspan>
            {/each}
          </text>
        {/if}
        {#if state !== 'clear'}
          <!-- THE BAND THAT DECIDED IT (decision 1), read against its
               number, where the state is — and for trouble the why:
               a verdict must name what failed (whole in the title) -->
          <text x={lt.verdict.x} y={lt.verdict.y} class="tiny why" class:err={troubled} class:warn={!troubled}>
            {#each verdictLines(lt, r) as line, i (i)}
              <tspan x={lt.verdict.x} dy={i === 0 ? 0 : 12}>{line}</tspan>
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

    <!-- THE CROSSINGS, drawn ON TOP of the territories: the machine
         that moves each rail's traffic WRITTEN on it — its name, its
         lamp, its status — what stands at the border now on the rail,
         and the rate under it. Every state is the server's (design
         d2154293); the map decides only where to write it and how wide
         to draw the rail. A click opens the crossing under the map. -->
    {#each rails as r (`c:${r.key}`)}
      {@const row = byBorder.get(r.key)}
      {@const state = stateOfBorder(row)}
      {@const names = wrapName(nameOf(row), labelChars(r.rail.label), 2)}
      {@const status = statusOf(row)}
      {@const at = railWriting(r.rail, names.length, status.length)}
      {@const bw = badgeW(row)}
      <g
        class="crossing"
        class:open={opened === r.key}
        data-crossing={r.key}
        data-state={state}
        data-waiting={row === undefined || row.waiting === null ? 'unknown' : row.waiting}
        role="button"
        tabindex="0"
        aria-expanded={opened === r.key}
        aria-label={railLabel(row, r.from, r.to)}
        onclick={() => toggle(r.key)}
        onkeydown={(e) => onKey(e, r.key)}>
        <!-- the room the writing takes, so the whole of it is the
             click target and not only the strokes of the letters -->
        <rect x={r.rail.label.x} y={r.rail.label.y} width={r.rail.label.w} height={r.rail.label.h} class="hit" />
        <!-- the machine, by the name its own registry gives it -->
        {#each names as line, i (i)}
          <text x={at.names[i]!.x} y={at.names[i]!.y} text-anchor={at.anchor} class="tiny machine-name">{line}</text>
        {/each}
        <!-- its lamp: lit red once it has been silent past its OWN
             declared cadence, green inside it, unlit (a hollow ring)
             when nothing can tell — and its status beside it -->
        <circle cx={at.lamp.x} cy={at.lamp.y} r="3.5" class="machine-lamp {machineLamp(row)}" />
        <text x={at.status.x} y={at.status.y} class="tiny machine-status" class:err={machineLamp(row) === 'err'}
          >{status}</text>
        <!-- what waits to cross, standing ON the rail: red only when
             the rail is troubled -->
        <rect x={r.rail.mid.x - bw / 2} y={r.rail.mid.y - 7} width={bw} height="14" rx="7" class="token {state}" />
        <text x={r.rail.mid.x} y={r.rail.mid.y + 3} text-anchor="middle" class="token-count">{tokenText(row)}</text>
        <!-- and the rate — on the moving map with what one token stands
             for (●=k) or "0 vs 4/d" when nothing arrives, and a still
             rail's held clock under it: the one moving thing left on it -->
        <text x={at.rate.x} y={at.rate.y} text-anchor={at.anchor} class="tiny rate"
          >{motion ? rateOnRail(row, r.rail.d) : railRate(row)}</text>
        {#if motion && heldOnRail(row) !== ''}
          <text x={at.rate.x} y={at.rate.y + 12} text-anchor={at.anchor} class="tiny held" data-held={r.key}
            >{heldOnRail(row)}</text>
        {/if}
      </g>
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
{/snippet}

<section class="yard" class:motion class:reduced={motion && reduced} aria-label="the IT world map">
  {#if motion}
    <!-- THE MOTION, STATED (decision 2): the compression and what a
         token is are always on screen, beside the controls — pause, ×60,
         ×600, ×3600 — and the page's own reduced-motion toggle. -->
    <div class="motion-bar" role="group" aria-label="the map's motion" data-motion-bar>
      <span class="motion-label">time</span>
      <span class="motion-speeds">
        {#each COMPRESSIONS as c (c)}
          <button type="button" aria-pressed={compression === c} onclick={() => (compression = c)}
            >{c === 0 ? 'pause' : `×${c}`}</button>
        {/each}
      </span>
      <span class="motion-said" data-compression={compression}>{compressionText(compression)}</span>
      <label class="motion-reduced"
        ><input type="checkbox" checked={reduced} onchange={(e) => (reducedChoice = e.currentTarget.checked)} /> reduced
        motion</label>
      <span class="motion-note">{reduced ? `${REPLAY_TEXT} — drawn as static density` : REPLAY_TEXT}</span>
      {#if staleSecs !== null}
        <!-- A frozen map must never keep moving as if it were live. -->
        <span class="motion-stale" data-stale={staleSecs}>not read for {staleSecs} s — nothing on the map is moving</span>
      {/if}
    </div>
    <div class="stage" class:stale={staleSecs !== null}>
      {@render world()}
      <MotionLayer rails={motionRails} {borders} {regions} {compression} {reduced} stale={staleSecs !== null} />
    </div>
  {:else}
    {@render world()}
  {/if}


  {#if opened !== null}
    <!-- THE CROSSING, INLINE (decision 6): everything the border read
         says about one rail, under the map, where it can be read,
         touched and screenshotted. Every sentence is the server's. -->
    {@const open = rails.find((r) => r.key === opened)}
    {@const row = byBorder.get(opened)}
    {@const state = stateOfBorder(row)}
    <div class="crossing-panel" data-panel={opened} data-state={state} role="region" aria-label="the crossing {opened}">
      <div class="crossing-head">
        <span class="crossing-lamp {lampOf(state)}"></span>
        <span class="crossing-title">{open?.from ?? ''} → {open?.to ?? ''}</span>
        <span class="crossing-state">{state}</span>
        <button type="button" class="crossing-close" aria-label="close the crossing" onclick={() => (opened = null)}
          >close</button>
      </div>
      {#if row === undefined}
        <p class="crossing-why">the borders read answered nothing for this rail</p>
      {:else}
        <p class="crossing-why">{row.why}</p>
        <dl class="crossing-facts">
          <dt>one crossing</dt>
          <dd>{row.crossing}</dd>
          <dt>rate</dt>
          <dd>{borderRateText(row.rate)}</dd>
          <dt>last crossed</dt>
          <dd>{crossedText(row, borders?.now ?? '')}</dd>
          <dt>machine</dt>
          <dd>{machineText(row.machine)} — {row.machine.why}</dd>
          <dt>waiting</dt>
          <dd>{waitingText(row)}</dd>
          {#if motion}
            <!-- What the moving map draws, in words: whom the pile waits
                 on, and the rule its stillness was judged by. -->
            <dt>waits on</dt>
            <dd data-by-class>
              {row.holds_by_class === null
                ? 'cannot tell'
                : `${row.holds_by_class.machine} in line for a machine · ${row.holds_by_class.person} on a person or the world · ${row.holds_by_class.unknown} cannot tell · ${row.holds_by_class.stuck} stuck`}
            </dd>
            <dt>flowing</dt>
            <dd>{row.flowing === null ? 'cannot tell' : row.flowing ? 'yes' : 'no'} — {row.flowing_why}</dd>
          {/if}
        </dl>
        {#if row.holds.length > 0}
          <ol class="crossing-holds">
            {#each row.holds as h, i (i)}
              <li><span class="crossing-what">{h.what}</span> — {h.why}</li>
            {/each}
          </ol>
        {/if}
        {#if unlistedText(row) !== ''}
          <p class="crossing-more">{unlistedText(row)}</p>
        {/if}
      {/if}
    </div>
  {/if}
</section>

<style>
  /* YardMap's classes, by name — the rails are the strong rule, the
     ties the rule, the ground the map's bg, the outlines the sheds, the
     lamps each state's line. Every colour is a --map-* token from
     styles.css with no fallback, so the palette lives once and a
     retired token fails map-palette.test.ts (42f66fb3). The count is
     the deck's entity title (18px, 600). */
  .yard {
    --rail: var(--map-rule-strong);
    --tie: var(--map-rule);
    background: var(--map-bg);
    border: 1px solid var(--map-rule);
    padding: var(--s2);
    overflow-x: auto;
    margin-top: var(--s3);
  }
  .yard svg {
    display: block;
    width: 100%;
    min-width: 900px;
    height: auto;
    font-family: var(--font-mono);
  }
  .yard text {
    fill: var(--map-muted);
    font-size: 10px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .yard text.tiny { font-size: 9px; letter-spacing: 0.04em; text-transform: none; }
  .yard text.count { font-size: 18px; font-weight: 600; fill: var(--map-ink); letter-spacing: 0; text-transform: none; }
  .yard text.err { fill: var(--map-bad-ink); }
  .yard text.warn { fill: var(--map-warn-ink); }
  .yard text.kpi { fill: var(--map-ink); }
  .rail { stroke: var(--rail); stroke-width: 2; fill: none; }
  .rail[data-state='attention'] { stroke: var(--map-warn-edge); }
  .rail[data-state='troubled'] { stroke: var(--map-bad-edge); }
  /* The traffic: dashes running the rail from A to B. Its WIDTH is the
     rail's, from the measured rate (`railWidth`, set on the element);
     its pace and dash come from the density band. `unknown` is
     deliberately a sparse, unlit dotting so an unmeasured rail cannot
     read as an empty one. */
  .traffic { fill: none; stroke: var(--map-ok-edge); stroke-linecap: round; opacity: 0.85;
    stroke-width: 2; stroke-dasharray: 2 10; animation: flow 3s linear infinite; }
  .traffic[data-density='unknown'] { stroke: var(--map-muted); stroke-dasharray: 1 7;
    opacity: 0.4; animation: none; }
  .traffic[data-density='none'] { stroke: none; animation: none; }
  .traffic[data-density='light'] { stroke-dasharray: 2 14; animation-duration: 4s; }
  .traffic[data-density='steady'] { stroke-dasharray: 4 10; animation-duration: 2.4s; }
  .traffic[data-density='heavy'] { stroke-dasharray: 6 6; animation-duration: 1.4s; }
  @keyframes flow { to { stroke-dashoffset: -48; } }
  /* The badge standing on the rail: what waits to cross, right now —
     red only when the rail is troubled (review 2026-09-24, finding 5),
     so a queue that is flowing does not shout; edged amber when a
     declared band is crossed (car A's attention). */
  .token { fill: var(--map-surface); stroke: var(--map-rule-strong); stroke-width: 1.5; }
  .token.attention { stroke: var(--map-warn-edge); }
  .token.troubled { fill: var(--map-bad-bg); stroke: var(--map-bad-edge); }
  .yard text.token-count { font-size: 9px; letter-spacing: 0; text-transform: none;
    fill: var(--map-ink); pointer-events: none; }
  .yard text.rate { fill: var(--map-muted); }
  /* The machine written on its rail: the name in ink, the status in the
     muted line, red when it has been silent past its own cadence. */
  .yard text.machine-name { fill: var(--map-ink); font-size: 10px; letter-spacing: 0; }
  .yard text.machine-status { letter-spacing: 0; }
  /* The machine's lamp. Lit green inside its declared cadence, red and
     blinking past it, and a hollow broken ring when nothing can tell —
     a different SHAPE from lit, as the machinery glyphs are. */
  .machine-lamp { fill: var(--map-surface); stroke: var(--map-muted); stroke-width: 1; stroke-dasharray: 1.5 1.5; }
  .machine-lamp.ok { fill: var(--map-ok-edge); stroke: var(--map-ok-edge); stroke-dasharray: none; }
  .machine-lamp.err { fill: var(--map-bad-edge); stroke: var(--map-bad-edge); stroke-dasharray: none;
    animation: blink 1s steps(2) infinite; }
  .glyph { fill: var(--map-surface); stroke: var(--map-rule-strong); }
  /* The whole of a crossing's writing is its click target. */
  .crossing { cursor: pointer; outline: none; }
  .crossing .hit { fill: none; pointer-events: all; }
  .crossing:hover .hit, .crossing:focus-visible .hit { stroke: var(--map-rule); stroke-dasharray: 2 3; }
  .crossing.open .hit { stroke: var(--map-ink); stroke-dasharray: none; }
  /* THE CROSSING, INLINE, under the map. The deck's grammar: a hairline
     box on the map's surface, mono labels, the why in body type. */
  .crossing-panel { margin-top: var(--s2); border: 1px solid var(--map-rule-strong);
    background: var(--map-surface); padding: var(--s2) var(--s3); font-size: 13px; color: var(--map-ink); }
  .crossing-panel[data-state='troubled'] { border-color: var(--map-bad-edge); }
  .crossing-head { display: flex; align-items: center; gap: var(--s2); font-family: var(--font-mono);
    font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; }
  .crossing-title { color: var(--map-ink); font-weight: 600; }
  .crossing-state { color: var(--map-muted); }
  .crossing-lamp { width: 8px; height: 8px; border-radius: 50%; background: var(--map-rule-strong); }
  .crossing-lamp.ok { background: var(--map-ok-edge); }
  .crossing-lamp.warn { background: var(--map-warn-edge); }
  .crossing-lamp.err { background: var(--map-bad-edge); }
  .crossing-close { margin-left: auto; font: inherit; color: var(--map-muted); background: none;
    border: 1px solid var(--map-rule); padding: 2px 8px; cursor: pointer; }
  .crossing-close:hover, .crossing-close:focus-visible { color: var(--map-ink); border-color: var(--map-ink); }
  .crossing-why { margin: var(--s2) 0; }
  .crossing-panel[data-state='troubled'] .crossing-why { color: var(--map-bad-ink); }
  .crossing-facts { display: grid; grid-template-columns: max-content 1fr; gap: 4px var(--s3); margin: 0; }
  .crossing-facts dt { font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.08em;
    text-transform: uppercase; color: var(--map-muted); }
  .crossing-facts dd { margin: 0; overflow-wrap: anywhere; }
  .crossing-holds { margin: var(--s2) 0 0; padding-left: 1.5em; }
  .crossing-holds li { margin: 2px 0; overflow-wrap: anywhere; }
  .crossing-what { font-family: var(--font-mono); font-size: 12px; }
  .crossing-more { margin: var(--s2) 0 0; color: var(--map-muted); }
  .tie { stroke: var(--tie); stroke-width: 6; stroke-dasharray: 3 9; fill: none; }
  .shed { fill: var(--map-surface); stroke: var(--map-rule-strong); }
  .shed.err { stroke: var(--map-bad-edge); }
  .shed.warn { stroke: var(--map-warn-edge); }
  .machine { cursor: pointer; outline: none; }
  /* THE ZOOM. The camera is the viewBox (region-contents.ts); these are only
     what the zoom LOOKS like: the territories the camera left fade
     back, the one it is in does not, and the ground around it is the
     way out. No new tokens — the plates are YardMap's wagon body and
     its lamps. */
  .frame { fill: var(--map-bg); cursor: zoom-out; }
  .territory.away { opacity: 0.35; }
  .territory.here .shed { stroke-width: 2; }
  .leave { cursor: zoom-out; fill: var(--map-muted); font-size: 9px; letter-spacing: 0.08em; }
  .leave:hover, .leave:focus-visible { fill: var(--map-ink); }
  .plate .wagon { fill: var(--map-surface); stroke: var(--map-rule-strong); }
  .plate .wagon.ok { stroke: var(--map-ok-edge); }
  .plate .wagon.warn { stroke: var(--map-warn-edge); }
  .plate .wagon.red { stroke: var(--map-bad-edge); }
  .yard text.plate-tag { fill: var(--map-ink); letter-spacing: 0; }
  /* THE PLATFORMS (car 4), in the same grammar: the track is a tie, a
     mark is a wagon body, the bound is a signal on the track. Every
     colour is a named token — a state or a surface never enters this
     file as a hex (42f66fb3). An UNKNOWN number gets its own band:
     dimmer than a figure and never the same ink as a 0, because the
     two are different facts. */
  .platform .track { stroke: var(--tie); stroke-width: 1; }
  .platform .mark { fill: var(--map-muted); }
  .platform .mark.flagged { fill: var(--map-bad-edge); }
  .platform .bound { stroke: var(--map-warn-edge); stroke-width: 1; }
  .yard text.standing { fill: var(--map-ink); letter-spacing: 0; }
  .yard text.rate { fill: var(--map-muted); letter-spacing: 0; }
  .yard text.unknown { fill: var(--map-muted); opacity: 0.6; font-style: italic; }
  .lamp.working { fill: var(--map-warn-edge); }
  .lamp.off { fill: var(--map-rule-strong); }
  .machine:hover .shed, .machine:focus-visible .shed { stroke: var(--map-ink); }
  .lamp { fill: var(--map-rule-strong); }
  .lamp.ok { fill: var(--map-ok-edge); }
  .lamp.warn { fill: var(--map-warn-edge); }
  .lamp.err { fill: var(--map-bad-edge); animation: blink 1s steps(2) infinite; }
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
  /* THE MOVING MAP (flight it-map-motion, design 31bade8f). Motion means
     work and only work (decision 1): the CSS dashes were decoration at
     five bands of pace, so they go — the canvas replays the measured
     rate instead — except an UNMEASURED rail's still grey dotting,
     which is how unknown is drawn (decision 5). No lamp blinks: the one
     cue for trouble is a single ring (Q2), never a loop. A region past
     its band freezes its machinery (decision 4). */
  .motion .traffic { animation: none; }
  .motion .traffic:not([data-density='unknown']) { display: none; }
  .motion .lamp.err, .motion .machine-lamp.err { animation: none; }
  .motion .territory:not([data-state='clear']) .piston, .reduced .piston { animation: none; }
  .motion .rail[data-still='held'] { stroke: var(--map-bad-edge); }
  .yard text.held { fill: var(--map-bad-ink); letter-spacing: 0; }
  .stage { position: relative; min-width: 900px; }
  /* Past three missed reads the whole map greys and stops (decision 6). */
  .stage.stale { filter: grayscale(1); opacity: 0.6; }
  .motion-bar { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s1) var(--s3);
    margin-bottom: var(--s2); font-family: var(--font-mono); font-size: 11px; color: var(--map-muted); }
  .motion-label { text-transform: uppercase; letter-spacing: 0.08em; }
  .motion-speeds { display: inline-flex; }
  .motion-speeds button { font: inherit; color: var(--map-ink); background: var(--map-surface);
    border: 1px solid var(--map-rule-strong); padding: 2px 8px; cursor: pointer; }
  .motion-speeds button + button { border-left: none; }
  .motion-speeds button[aria-pressed='true'] { background: var(--map-ink); color: var(--map-surface); }
  .motion-speeds button:focus-visible, .motion-reduced input:focus-visible { outline: 2px solid var(--map-accent); outline-offset: 1px; }
  .motion-said { color: var(--map-ink); }
  .motion-reduced { display: inline-flex; align-items: center; gap: 4px; cursor: pointer; }
  .motion-stale { color: var(--map-bad-ink); }
  @media (prefers-reduced-motion: reduce) {
    .lamp, .glyph, .traffic, .machine-lamp { animation: none !important; }
    /* A still piston is still a FILLED housing, which idle never is —
       motion is the cue, the fill is the fallback. */
    .glyph .piston { animation: none !important; }
  }
</style>
