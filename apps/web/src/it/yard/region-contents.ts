// WHAT A REGION CONTAINS, AND WHERE IT SITS IN THE REGION'S RECT.
//
// This module SELECTS and PLACES: which stations a region covers,
// which wagons are standing in it, and where each plate goes inside
// the rect it is handed. One definition per number — it derives no
// count and invents no state.
//
// THERE IS NO CAMERA HERE ANY MORE (David, 2026-09-20; backlog
// ca37478f). Until today a click walked the SVG's viewBox from the
// whole world into a territory's rect — zoomBoxOf, lerpBox, easeInOut,
// viewBoxText, WORLD_BOX, ZOOM_MS — and everything drawn got larger in
// proportion. David: "I just wanted the world map view to get replaced
// with the more detailed region map view on click but not literally
// increase the size of content on the world map." So the world map
// draws the world, RegionMap draws ONE region on its own canvas, and
// the route decides which. The interpolation is DELETED rather than
// left unused, and this file is named for what it does now.
//
// `interiorLayout` takes the rect to lay out INSIDE, which is the only
// thing the swap changed here: a slot on the world line before, a
// region's whole canvas (region-canvas.ts) now.
//
// Everything is pure arithmetic, so a broken layout fails a unit test
// before it is a broken picture (region-contents.test.ts).

import { REGION_NAMES, type RegionName } from './regions';
import { MACHINERY_STRIP_H } from './world-machines';
import { territoryOf, type Territory } from './world';
import type { Scene, Station, Wagon } from './yard-floor';

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

/** The regions whose interior is WAGONS IN TRANSIT — the six the
 *  yard's floor stands rolling stock in. Receiving and marshalling
 *  hold queues instead, and their interior is a platform deck
 *  (world-interior.ts, car 4), so they are absent here by design. */
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
export const INTERIOR_HEAD = 64;
const EDGE = 8;

/** Lay the wagons out inside the territory. What does not fit is
 *  COUNTED, never dropped silently — the yard's own "+N" idiom, so a
 *  full region reads as full rather than as a tidy one. */
export function interiorLayout(t: Territory, wagons: ReadonlyArray<Wagon>): Readonly<{ placed: ReadonlyArray<Placed>; hidden: number }> {
  const availW = t.w - 2 * EDGE;
  // The garage is wide and low: it gets its plates nearer its own head,
  // because INTERIOR_HEAD is measured for a full-height line territory.
  const head = Math.min(INTERIOR_HEAD, Math.max(24, t.h - PLATE_H - EDGE));
  // The bottom of every territory belongs to its machinery (car 5), so
  // a wagon plate and a machine glyph never draw over each other. The
  // height is read from the strip's own definition, not copied.
  const availH = t.h - head - EDGE - MACHINERY_STRIP_H;
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
