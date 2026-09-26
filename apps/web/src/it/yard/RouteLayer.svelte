<svelte:options namespace="svg" />

<script lang="ts">
  // THE ROUTE LAYER — every route the server serves, drawn (design
  // e765b3fc, car R3 on feedback 84cba7e2). A `<g>` inside the transit
  // map's SVG, apart from the stations and the alarms, so the map's own
  // component stays the stations' and this one the track's — and so the
  // moves of car M2 can draw over exactly these paths without editing
  // either (route-layout.ts `routePath` answers the same path by the
  // same (from, to) key).
  //
  // WHAT IT DRAWS, all of it from the routes read (route-layout.ts
  // `sectionsOf`):
  //   * a SECTION per route between two stations, in its line's colour —
  //     red where the server says it is not flowing, dotted where it
  //     cannot tell, dashed red where only the moves record supports it
  //     — with its waiting blocks and its trains, and a door to its
  //     panel;
  //   * an EXIT per station a terminal closes packets at — an off-ramp
  //     ending at a buffer stop, naming its terminals (David,
  //     added_2026_09_25_david_offramps: "every packet that leaves the map
  //     must leave by a drawn route");
  //   * an ENTRY per station packets are admitted at.
  // Every element carries `data-section="<from>→<to>"` and `data-from` /
  // `data-to`. Colours are --map-* tokens only (map-palette.test.ts, the
  // a-colour-is-a-token lint).
  import { navigate } from '@boss/web-kit/nav';
  import type { Border } from './borders';
  import { sectionHref } from './regions';
  import { exitNames } from './routes';
  import type { Section } from './route-layout';
  import { sectionGround, stationLabel, trainsOf, waitingBlocks } from './transit';
  import { pointAt } from './world-motion';

  type Props = Readonly<{
    /** The served routes, laid out (route-layout.ts `sectionsOf`). */
    sections: ReadonlyArray<Section>;
    /** Each section's border reading, by its key — its rate, its queue,
     *  whether it flows. A section with none reads "no reading". */
    borders: ReadonlyMap<string, Border>;
    reduced: boolean;
    /** The selected section's key, marked with a casing; null for none. */
    selected: string | null;
    /** The real moves ride these sections (flight `it-map-live`, car M2
     *  of design e765b3fc): the rate-replay blocks are then off, so no
     *  crossing is drawn twice, once as a picture of it. */
    live?: boolean;
  }>;
  let { sections, borders, reduced, selected, live = false }: Props = $props();

  const track = $derived(sections.filter((s) => s.kind === 'section'));
  const ramps = $derived(sections.filter((s) => s.kind !== 'section'));

  /** A section's name and nothing more: its rate, its queue and its
   *  verdict are the panel's (car N2). */
  const sectionTitle = (from: string, to: string): string => `the ${stationLabel(from)} → ${stationLabel(to)} section`;

  /** An exit names the terminals it carries; an entry, the steps that
   *  put a packet on the map there. */
  const rampTitle = (s: Section): string =>
    s.kind === 'exit'
      ? `leaves the map from ${stationLabel(s.from ?? '')}: ${exitNames(s.route).join(', ')}`
      : `enters the map at ${stationLabel(s.to ?? '')}: ${exitNames(s.route).join(', ')}`;

  /** The buffer stop across an exit's end: a short bar square to it. */
  function bufferStop(s: Section): string {
    const end = pointAt(s.walked, s.walked.length);
    const back = pointAt(s.walked, s.walked.length - 1);
    const vertical = Math.abs(end.x - back.x) < Math.abs(end.y - back.y);
    return vertical ? `M${end.x - 6} ${end.y} H${end.x + 6}` : `M${end.x} ${end.y - 6} V${end.y + 6}`;
  }

  function open(e: MouseEvent, href: string): void {
    e.preventDefault();
    navigate(href);
  }
</script>

<g class="routes" data-routes>
  {#each ramps as s (s.key)}
    <g class="ramp" data-ramp={s.kind} data-section={s.key} data-from={s.from ?? undefined} data-to={s.to ?? undefined}
      data-declared={s.declared ? 'true' : 'false'}>
      <title>{rampTitle(s)}</title>
      <path d={s.d} class="ramp-line line-{s.line}" class:undeclared={!s.declared} />
      {#if s.kind === 'exit'}
        <path d={bufferStop(s)} class="buffer line-{s.line}" class:undeclared={!s.declared} />
      {/if}
    </g>
  {/each}

  <!-- Each section is a door to its own panel: a wide unpainted stroke
       takes the click, so an 8-unit line is not a needle to aim at. The
       selected one stands in an ink casing. -->
  {#each track as s (s.key)}
    {@const from = s.from ?? ''}
    {@const to = s.to ?? ''}
    {@const b = borders.get(s.key)}
    {@const ground = sectionGround(b)}
    {@const waiting = waitingBlocks(s, b)}
    {@const trains = live ? null : trainsOf(b, reduced)}
    {@const href = sectionHref(from, to)}
    {@const isSelected = selected === s.key}
    <a class="section-link" {href} data-section-link={s.key} data-selected={isSelected ? 'true' : undefined}
      aria-current={isSelected ? 'true' : undefined} aria-label={sectionTitle(from, to)}
      onclick={(e) => open(e, href)}>
      {#if isSelected}
        <path d={s.d} class="casing" data-selected-mark={s.key} />
      {/if}
      <path d={s.d} class="section line-{s.line}" class:held={ground === 'held'} class:unknown={ground === 'unknown'}
        class:undeclared={!s.declared}
        data-section={s.key} data-from={from} data-to={to} data-line={s.line} data-ground={ground}
        data-declared={s.declared ? 'true' : 'false'}>
        <title>{sectionTitle(from, to)}{s.declared ? '' : ' — observed, declared by no protocol or hand-off'}</title>
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
</g>

<style>
  /* Every colour a --map-* token from styles.css with no fallback: the
     lines the Design department's --map-line-*, the stall and the
     undeclared the map's own bad edge, the words its ink and muted. */
  .line-delivery { stroke: var(--map-line-delivery); }
  .line-publish { stroke: var(--map-line-publish); }
  .line-siding { stroke: var(--map-line-siding); }
  .line-tenant { stroke: var(--map-line-tenant); }

  .section { fill: none; stroke-width: 8; stroke-linecap: round; stroke-linejoin: round; }
  .section.held { stroke: var(--map-bad-edge); }
  .section.unknown { stroke-dasharray: 2 6; opacity: 0.6; }
  /* OBSERVED, UNDECLARED (design e765b3fc §2b): packets moved this way
     and no protocol or hand-off declares it — dashed in the map's own
     trouble red, over its line, so the drawing says it is a finding. */
  .section.undeclared, .ramp-line.undeclared, .buffer.undeclared {
    stroke: var(--map-bad-edge); stroke-dasharray: 9 6; stroke-linecap: butt; opacity: 1; }
  /* An exit or an entry: thinner than a section, because nothing waits
     on it — it is where the map begins or ends for a packet. */
  .ramp-line { fill: none; stroke-width: 4; stroke-linecap: round; }
  .buffer { fill: none; stroke-width: 4; stroke-linecap: square; }
  /* The section's door: a wide stroke nobody sees takes the click. */
  .section-link { cursor: pointer; }
  .section-link:focus-visible { outline: none; }
  .hit { fill: none; stroke: transparent; stroke-width: 22; stroke-linecap: round; pointer-events: stroke; }
  /* THE SELECTION'S MARK (car N2), in the map's ink: a casing under the
     selected section, as a transit diagram draws an interchange. Not a
     state colour, so the mark never reads as a verdict. */
  .casing { fill: none; stroke: var(--map-ink); stroke-width: 16; stroke-linecap: round; stroke-linejoin: round; }
  .section-link:focus-visible .section, .section-link:hover .section { stroke-width: 11; }

  .waiting { fill: var(--map-ink); }
  .train { fill: var(--map-surface); stroke-width: 2; }
  text.more { font-family: var(--font-mono); font-size: 9.5px; fill: var(--map-muted); font-variant-numeric: tabular-nums; }
</style>
