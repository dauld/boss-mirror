<script lang="ts">
  // THE PLANT STRIP (design 62de32ae, decision 11; car E on backlog
  // c3105b2a): the machinery that serves every region, along the world
  // map's edge. The layout and the reasoning are plant.ts; the glyphs
  // are the machinery idiom WorldMap and RegionMap already draw —
  // running MOVES, idle is still and solid, failed blinks red, unknown
  // is a broken outline with a `?` — with the name and state written
  // beside each, because the strip has the room a territory's corner
  // did not.
  import { WORLD } from './world';
  import { PLANT_H, plantLayout } from './plant';
  import { machineTitle, machineryLabel } from './world-machines';
  import type { Machine } from './regions';

  type Props = Readonly<{ machines: ReadonlyArray<Machine> }>;
  let { machines }: Props = $props();

  const laid = $derived(plantLayout(machines, WORLD.width));
</script>

<!-- An older server sends no plant; no strip is drawn for it rather than
     one that reads as an estate with nothing running. -->
{#if machines.length > 0}
  <section class="plant" aria-label="the plant — machinery serving every region">
    <svg viewBox="0 0 {WORLD.width} {PLANT_H}" role="img" aria-label="the plant · {machineryLabel(machines)}">
      <text x={laid.leadX} y={PLANT_H / 2 + 3.5} class="lead">Plant · serves every region</text>
      {#each laid.placed as p (p.machine.id)}
        <g class="glyph {p.machine.state}" data-machine={p.machine.id} data-machine-state={p.machine.state}>
          <title>{machineTitle(p.machine)}</title>
          <rect x={p.x} y={p.y} width={p.w} height={p.h} class="shed" class:err={p.machine.state === 'failed'} />
          {#if p.machine.state === 'running'}
            <rect class="lamp ok piston" x={p.x + 2} y={p.y + 3} width="4" height={p.h - 6} />
          {:else if p.machine.state === 'failed'}
            <rect class="lamp err" x={p.x + 3} y={p.y + 3} width={p.w - 6} height={p.h - 6} />
          {:else if p.machine.state === 'unknown'}
            <text class="mark" x={p.x + p.w / 2} y={p.y + p.h - 3} text-anchor="middle">?</text>
          {/if}
          <text x={p.textX} y={PLANT_H / 2 + 3.5} class="name" class:err={p.machine.state === 'failed'}>{p.label}</text>
        </g>
      {/each}
      {#if laid.hidden > 0}
        <!-- what did not fit is COUNTED, never quietly dropped -->
        <text x={WORLD.width - 8} y={PLANT_H / 2 + 3.5} text-anchor="end" class="name">+{laid.hidden} more</text>
      {/if}
    </svg>
  </section>
{/if}

<style>
  /* The map's own --map-* tokens, with no fallback, as every yard
     surface uses them (42f66fb3, map-palette.test.ts). */
  /* The outline is the section's border, not an SVG stroke: scaled with
     the viewBox, a one-unit stroke along the bottom edge rendered below a
     pixel and vanished at 1440px. */
  .plant { margin-top: var(--s2); border: 1px solid var(--map-rule-strong); background: var(--map-surface); }
  .plant svg { display: block; width: 100%; min-width: 900px; height: auto; font-family: var(--font-mono); }
  .plant text { fill: var(--map-muted); font-size: 10px; letter-spacing: 0.02em; }
  .plant text.lead { fill: var(--map-ink); letter-spacing: 0.08em; text-transform: uppercase; }
  .plant text.name { fill: var(--map-ink); }
  .plant text.err { fill: var(--map-bad-ink); }
  .plant text.mark { font-size: 9px; }
  .shed { fill: var(--map-surface); stroke: var(--map-rule-strong); }
  .shed.err { stroke: var(--map-bad-edge); }
  .lamp { fill: var(--map-rule-strong); }
  .lamp.ok { fill: var(--map-ok-edge); }
  .lamp.err { fill: var(--map-bad-edge); animation: blink 1s steps(2) infinite; }
  @keyframes blink { 50% { opacity: 0.25; } }
  .glyph.running .piston { animation: piston 1.1s ease-in-out infinite alternate; }
  .glyph.idle .shed { opacity: 0.55; }
  .glyph.failed .shed { stroke-width: 1.5; }
  .glyph.unknown .shed { stroke-dasharray: 2 2; }
  @keyframes piston { to { transform: translateX(4px); } }
  @media (prefers-reduced-motion: reduce) {
    .lamp, .glyph .piston { animation: none !important; }
  }
</style>
