// CLICKING A TERRITORY IS A ZOOM (design d2154293, car 3; David's
// feedback c3105b2a, 2026-09-19: "zoom into the region by clicking to
// see" what is moving inside it). The world map is ONE SVG in ONE
// coordinate space, so going into a region is not a navigation to
// another surface — it is the viewBox walking from the whole world to
// that territory's rect. Nothing unmounts, nothing is replaced, the
// borders and the neighbours stay where they are; the camera moves.
//
// Zoomed, a territory shows its INTERIOR: the wagons standing at the
// stations that territory covers, taken straight off the floor's Scene
// (yard-floor.ts) — the same wagons the Train Yard draws, with the same
// ids, tags, lamps and statuses. One definition per number: this module
// SELECTS and PLACES, it derives no count and invents no state.
//
// Everything here is pure arithmetic over the layout in world.ts and
// the Scene, so a broken zoom fails a unit test before it is a broken
// picture (world-zoom.test.ts). WorldMap.svelte owns the animation
// frames; this owns what the frames are walking between.

import { REGION_NAMES, type RegionName } from './regions';
import { WORLD, territoryOf, type Territory } from './world';
import type { Scene, Station, Wagon } from './yard-floor';

/** A viewBox: the rectangle of the world the SVG is showing. */
export type Box = Readonly<{ x: number; y: number; w: number; h: number }>;

/** The whole world — the box `/it` opens on and Escape returns to. */
export const WORLD_BOX: Box = { x: 0, y: 0, w: WORLD.width, h: WORLD.height };

/** How long the camera takes. Long enough that the eye follows the
 *  move (which is the point — a cut would read as a page change),
 *  short enough that it is never a wait. */
export const ZOOM_MS = 420;

/** Room left around a zoomed territory, so the outline it zoomed into
 *  is not cropped and the rails leaving it stay in view. */
const PAD = 18;

/** The box the camera walks to for a region — the whole world for
 *  `null`, and for a name the layout does not know, because a door
 *  that opens nowhere must still leave a picture on the screen. */
export function zoomBoxOf(region: string | null): Box {
  if (region === null) return WORLD_BOX;
  const t = territoryOf(region);
  if (t === undefined) return WORLD_BOX;
  return { x: t.x - PAD, y: t.y - PAD, w: t.w + 2 * PAD, h: t.h + 2 * PAD };
}

/** Where the camera is, a fraction `k` of the way from `a` to `b`. */
export function lerpBox(a: Box, b: Box, k: number): Box {
  const at = (from: number, to: number) => from + (to - from) * k;
  return { x: at(a.x, b.x), y: at(a.y, b.y), w: at(a.w, b.w), h: at(a.h, b.h) };
}

/** Ease in, ease out (the cubic) — a camera that starts and stops
 *  gently reads as one continuous world; a linear ramp reads as a
 *  slide transition between two surfaces. */
export function easeInOut(k: number): number {
  return k < 0.5 ? 4 * k * k * k : 1 - Math.pow(-2 * k + 2, 3) / 2;
}

/** The four numbers, rounded to hundredths — SVG parses a plain
 *  decimal, and an exponent (1e-7, which a raw lerp produces near the
 *  ends) makes the whole attribute invalid and the map blank. */
export function viewBoxText(b: Box): string {
  const n = (v: number) => String(Math.round(v * 100) / 100);
  return `${n(b.x)} ${n(b.y)} ${n(b.w)} ${n(b.h)}`;
}

// ---------------------------------------------------------------------
// Which territory a wagon is standing in.
// ---------------------------------------------------------------------

/** Every station of the floor, mapped to the territory that covers it.
 *  A total record, not a lookup with a fallback: a new Station is a
 *  type error here, which is how a wagon can never quietly vanish from
 *  the world (it would otherwise stand in no region and be drawn
 *  nowhere, the false-empty class). The groupings are the ones
 *  regions.ts already made when it named each region's floor panel:
 *  the gates region IS the approach — publishing, queued, in a bay, in
 *  limbo — and the cancelled siding is drawn in the arrivals yard. */
const STATION_REGION: Readonly<Record<Station, RegionName>> = {
  approach: 'gates',
  'gate-queue': 'gates',
  gate: 'gates',
  limbo: 'gates',
  dock: 'dock',
  garage: 'garage',
  train: 'track',
  arrivals: 'arrivals',
  cancelled: 'arrivals',
  'inspection-shed': 'shed',
  'siding-event': 'shed',
  'siding-no-probe': 'shed',
};

export function regionOfStation(station: Station): RegionName {
  return STATION_REGION[station];
}

/** The regions that have an interior to zoom into — the six the yard's
 *  floor stands wagons in. Receiving and marshalling have pages of
 *  their own and grow interiors on car 4; until then a click on one of
 *  them opens its page, as it did before this car. */
export const INTERIOR_REGIONS: ReadonlyArray<RegionName> = REGION_NAMES.filter((name) =>
  Object.values(STATION_REGION).includes(name),
);

export function hasInterior(region: string): boolean {
  return (INTERIOR_REGIONS as ReadonlyArray<string>).includes(region);
}

/** The wagons standing in a territory, in the order the floor stands
 *  them: by station, then by slot — so a wagon keeps its place between
 *  polls and a move is a move, not a reshuffle. */
export function interiorWagons(scene: Scene, region: string): ReadonlyArray<Wagon> {
  if (!hasInterior(region)) return [];
  const stations = Object.keys(STATION_REGION) as ReadonlyArray<Station>;
  const order = new Map(stations.map((s, i) => [s, i] as const));
  return scene.wagons
    .filter((w) => regionOfStation(w.station) === region)
    .slice()
    .sort((a, b) => (order.get(a.station)! - order.get(b.station)!) || a.slot - b.slot || a.id.localeCompare(b.id));
}

// ---------------------------------------------------------------------
// Where the plates go inside the outline.
// ---------------------------------------------------------------------

/** A wagon plate, placed in world coordinates. */
export type Placed = Readonly<{ wagon: Wagon; x: number; y: number; w: number; h: number }>;

/** The grid: plates wide enough for an eleven-character nameplate at
 *  9px mono (the width YardMap gives a wagon), laid left to right then
 *  down, under the territory's own count and trend block. */
const PLATE_W = 62;
const PLATE_H = 18;
const GAP = 4;
/** Clear of the compact header a ZOOMED territory prints — its name,
 *  its count over its bound, its state and its why on two lines. The
 *  full count/trend/why block the world draws at rest is not drawn
 *  when a territory is zoomed: the rect's room goes to the interior,
 *  which is what the zoom was for. */
const HEAD = 64;
const EDGE = 8;

/** Lay the wagons out inside the territory. What does not fit is
 *  COUNTED, never dropped silently — the yard's own "+N" idiom, so a
 *  full region reads as full rather than as a tidy one. */
export function interiorLayout(t: Territory, wagons: ReadonlyArray<Wagon>): Readonly<{ placed: ReadonlyArray<Placed>; hidden: number }> {
  const availW = t.w - 2 * EDGE;
  // The garage is wide and low: it gets its plates nearer its own head,
  // because HEAD is measured for a full-height line territory.
  const head = Math.min(HEAD, Math.max(24, t.h - PLATE_H - EDGE));
  const availH = t.h - head - EDGE;
  const cols = Math.max(1, Math.floor((availW + GAP) / (PLATE_W + GAP)));
  const rows = Math.max(1, Math.floor((availH + GAP) / (PLATE_H + GAP)));
  const capacity = cols * rows;
  const placed = wagons.slice(0, capacity).map((wagon, i): Placed => ({
    wagon,
    x: t.x + EDGE + (i % cols) * (PLATE_W + GAP),
    y: t.y + head + Math.floor(i / cols) * (PLATE_H + GAP),
    w: PLATE_W,
    h: PLATE_H,
  }));
  return { placed, hidden: Math.max(0, wagons.length - placed.length) };
}
