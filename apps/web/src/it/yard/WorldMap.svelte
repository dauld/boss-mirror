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
  // A territory is a door: clicking it opens /it/yard/<region>, the
  // floor that already exists (the viewBox zoom is car 3; the borders
  // carry their traffic in car 2). NO NEW STYLING — the classes here
  // are YardMap's, by name and by token, so the visual redesign reskins
  // one grammar (Q1, decided 2026-09-19).
  import { navigate } from '@boss/web-kit/nav';
  import { countText, floorHref, lampOf, trendText, type Region, type Regions } from './regions';
  import { BORDERS, TERRITORIES, WORLD, borderPath, territoryOf, wrapWords, type Territory } from './world';

  type Props = Readonly<{ regions: Regions }>;
  let { regions }: Props = $props();

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
  <svg viewBox="0 0 {WORLD.width} {WORLD.height}" role="img" aria-label="the IT world: eight territories along the packet flow">
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
      </a>
    {/each}

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
