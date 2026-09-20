// THE MACHINERY IN A TERRITORY (design d2154293, car 5; David's
// feedback c3105b2a, 2026-09-19: zoom into a region to see "moving cars
// and running machinery"). Car 3 gave a territory its moving cars —
// the wagons standing at the stations it covers. This gives it the
// machines that move them: the gate bays, the conductor, the stations,
// the runners.
//
// NOTHING HERE JUDGES A MACHINE. The server does (`boss_jobs::regions`,
// a `machines` list per region), because a machine's state has to mean
// the same thing to the map, the floor and the CLI — the one-definition
// rule that made `/api/yard/regions` the only reading of a region in
// the first place. This module SELECTS, ORDERS and PLACES, and turns a
// state into the class the glyph wears.
//
// A GLYPH MUST READ WITHOUT A LEGEND (Factorio's bar: state legible at
// a glance, without reading labels — a glyph that needs a legend is a
// label with extra steps). Four states, four different things the eye
// does with them:
//
//   running  — the glyph MOVES. A piston sweeps inside the housing.
//              Motion is the one channel nothing else on this map uses,
//              and it is what "running" means everywhere machines are
//              drawn.
//   idle     — still, and SOLID. Housing drawn, nothing inside it.
//   failed   — red and BLINKING, the yard's own alarm idiom (the `err`
//              lamp already blinks on this map).
//   unknown  — the housing is DASHED and carries a `?`. A broken
//              outline reads as "not a reading" before any colour is
//              considered, and the question mark says which reading is
//              missing without a key.
//
// Idle and unknown are the pair this design exists to keep apart. A
// machine nobody can measure drawn as a calm idle box is the
// false-empty class at its most visible, so idle is solid and quiet
// while unknown is broken and marked — different SHAPE, not a different
// shade of the same shape, because shade alone fails at world scale.

import type { Machine, MachineState } from './regions';
import type { Territory } from './world';

/** The strip a territory reserves along its bottom edge for machinery.
 *  `world-zoom.ts` subtracts it from the room the interior's wagon
 *  plates get, so a machine and a wagon never draw over each other —
 *  one definition of the height, read by both. */
export const MACHINERY_STRIP_H = 20;

const GLYPH_W = 12;
const GLYPH_H = 12;
const GAP = 3;
const EDGE = 8;

/** A machine placed in world coordinates. */
export type MachineGlyph = Readonly<{
  machine: Machine;
  x: number;
  y: number;
  w: number;
  h: number;
}>;

/** What matters first, so the machines that get cut when a territory
 *  is narrow are the ones nothing is wrong with. A failure is never
 *  hidden; neither is an unknown, which is the reading that asks for
 *  attention next. */
const RANK: Readonly<Record<MachineState, number>> = {
  failed: 0,
  unknown: 1,
  running: 2,
  idle: 3,
};

/** The machines of a territory, in drawing order: by what matters,
 *  then by id — so a glyph keeps its place between reads and a change
 *  of state is a change of state, not a reshuffle. */
export function machineOrder(machines: ReadonlyArray<Machine>): ReadonlyArray<Machine> {
  return machines
    .slice()
    .sort((a, b) => RANK[a.state] - RANK[b.state] || a.id.localeCompare(b.id));
}

/** Lay the machinery along the territory's bottom edge. What does not
 *  fit is COUNTED, never silently dropped — the map's own "+N" idiom. */
export function machineryStrip(
  t: Territory,
  machines: ReadonlyArray<Machine>,
): Readonly<{ placed: ReadonlyArray<MachineGlyph>; hidden: number }> {
  const ordered = machineOrder(machines);
  const avail = t.w - 2 * EDGE;
  const capacity = Math.max(0, Math.floor((avail + GAP) / (GLYPH_W + GAP)));
  const y = t.y + t.h - MACHINERY_STRIP_H + (MACHINERY_STRIP_H - GLYPH_H) / 2;
  const placed = ordered.slice(0, capacity).map(
    (machine, i): MachineGlyph => ({
      machine,
      x: t.x + EDGE + i * (GLYPH_W + GAP),
      y,
      w: GLYPH_W,
      h: GLYPH_H,
    }),
  );
  return { placed, hidden: Math.max(0, ordered.length - placed.length) };
}

/** The whole reading, for the glyph's `<title>` and its aria-label —
 *  the sentence the server wrote, never a shortened copy of it. */
export function machineTitle(m: Machine): string {
  return `${m.name} · ${m.state} — ${m.why}`;
}

/** What the machinery of a region amounts to, in words: the line a
 *  screen reader gets instead of the glyphs, and the one an operator
 *  gets on hover. States with none of them are left out; a region with
 *  no machinery says so rather than printing zeroes. */
export function machineryLabel(machines: ReadonlyArray<Machine>): string {
  if (machines.length === 0) return 'no machinery of ours works here';
  const counts = (['failed', 'unknown', 'running', 'idle'] as const)
    .map((state) => [state, machines.filter((m) => m.state === state).length] as const)
    .filter(([, n]) => n > 0)
    .map(([state, n]) => `${n} ${state}`);
  return `${machines.length} machine${machines.length === 1 ? '' : 's'}: ${counts.join(', ')}`;
}
