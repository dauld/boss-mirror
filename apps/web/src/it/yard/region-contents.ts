// WHAT A REGION CONTAINS, AND WHERE IT SITS IN THE REGION'S RECT.
//
// This module SELECTS and MEASURES: which stations a region covers,
// and the room inside the rect it is handed that a region's contents
// may use. One definition per number — it derives no count and
// invents no state.
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
// `contentsBox` takes the rect to lay out INSIDE, which is the only
// thing the swap changed here: a slot on the world line before, a
// region's whole canvas (region-canvas.ts) now.
//
// Everything is pure arithmetic, so a broken layout fails a unit test
// before it is a broken picture (region-contents.test.ts).

import { REGION_NAMES, type RegionName } from './regions';
import { MACHINERY_STRIP_H } from './world-machines';
import type { Territory } from './world';
import type { Station } from './yard-floor';

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
 *  limbo — and the cancelled siding is drawn in the arrivals yard.
 *
 *  `as const` keeps each value's literal, so the record also DEFINES
 *  the floor regions (`FloorRegion`) and which stations each one holds
 *  (`StationOf`) — the keys floor-slices.ts lays the floor out by. */
const STATION_REGION = {
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
} as const satisfies Readonly<Record<Station, RegionName>>;

/** The six regions the yard's floor stands wagons in — read off
 *  STATION_REGION's values, so a region no station maps to is not one. */
export type FloorRegion = (typeof STATION_REGION)[Station];

/** The stations a floor region holds: `StationOf<'gates'>` is
 *  approach | gate-queue | gate | limbo. A layout keyed by it must name
 *  every one of them and no other region's (floor-slices.ts). */
export type StationOf<R extends FloorRegion> = {
  [S in Station]: (typeof STATION_REGION)[S] extends R ? S : never;
}[Station];

export function regionOfStation(station: Station): FloorRegion {
  return STATION_REGION[station];
}

/** The regions whose interior is WAGONS IN TRANSIT — the six the
 *  yard's floor stands rolling stock in. Receiving and marshalling
 *  hold queues instead, and their interior is a platform deck
 *  (world-interior.ts, car 4), so they are absent here by design. */
export const INTERIOR_REGIONS: ReadonlyArray<RegionName> = REGION_NAMES.filter((name) =>
  (Object.values(STATION_REGION) as ReadonlyArray<RegionName>).includes(name),
);

export function hasInterior(region: string): boolean {
  return (INTERIOR_REGIONS as ReadonlyArray<string>).includes(region);
}

// ---------------------------------------------------------------------
// The room inside the outline.
//
// The wagon plates that were laid out here — `interiorWagons`, the
// `Placed` grid and `interiorLayout` — are deleted (design fe77a1d2,
// car 3): the region map draws its slice of the floor itself
// (floor-slices.ts, RegionFloor.svelte) and nothing read the plates
// any more. What stays is the room the queue boards' platforms are
// laid out in (world-interior.ts `platformLayout`).
// ---------------------------------------------------------------------

/** Clear of the compact header a ZOOMED territory prints — its name,
 *  its count over its bound, its state and its why on two lines. The
 *  full count/trend/why block the world draws at rest is not drawn
 *  when a territory is zoomed: the rect's room goes to the interior,
 *  which is what the zoom was for. */
const INTERIOR_HEAD = 64;
const EDGE = 8;

/** THE ROOM A REGION'S CONTENTS MAY USE — one definition, read by the
 *  layout that divides a region's rect (CLAUDE.md 9a; backlog
 *  3a916816; there were two until the wagon plates were deleted,
 *  fe77a1d2 car 3). The bottom of every territory belongs to its machinery
 *  (car 5), and the height of that strip is read from the strip's own
 *  definition rather than copied.
 *
 *  It is a function because the header clamp needs the height of ONE
 *  row of whatever the caller is laying out: a wide, low territory
 *  gets its contents nearer its own head, because INTERIOR_HEAD is
 *  measured for a full-height line territory.
 *
 *  `platformLayout` divided this same canvas WITHOUT the strip's term
 *  and did not import its height at all, so a platform row and a
 *  machine glyph could be placed in the same pixels — and on the world
 *  line's 200-high territory they were. It reads this now, so no
 *  layout can disagree with the strip about where the bottom is. */
export function contentsBox(
  t: Territory,
  rowH: number,
): Readonly<{ x: number; y: number; w: number; h: number }> {
  const head = Math.min(INTERIOR_HEAD, Math.max(24, t.h - rowH - EDGE));
  return {
    x: t.x + EDGE,
    y: t.y + head,
    w: t.w - 2 * EDGE,
    h: Math.max(0, t.h - head - EDGE - MACHINERY_STRIP_H),
  };
}

/** Where a layout's "+N more" note is drawn: right-aligned to the
 *  contents box, on a baseline just above its first row (backlog
 *  ba83225e). RegionMap drew it at the canvas's bottom edge minus six —
 *  inside the machinery strip, clear of the glyphs only because they
 *  fill from the left. Above the box is the band the head clamp keeps
 *  (never under 24 px), which neither the rows nor the strip can reach,
 *  and the note has no descender, so its baseline is its lowest pixel. */
export type NoteAt = Readonly<{ x: number; y: number }>;
const NOTE_GAP = 4;

export function overflowNoteAt(box: Readonly<{ x: number; y: number; w: number }>): NoteAt {
  return { x: box.x + box.w, y: box.y - NOTE_GAP };
}
