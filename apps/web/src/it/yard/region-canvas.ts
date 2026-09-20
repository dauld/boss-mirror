// THE REGION'S OWN CANVAS (David, 2026-09-20; backlog ca37478f).
//
// "I just wanted the world map view to get replaced with the more
// detailed region map view on click but not literally increase the
// size of content on the world map."
//
// Car 3 read "zoom into the region by clicking to see" as a CAMERA:
// one SVG, one coordinate space, the viewBox walking from the whole
// world into a territory's rect. Everything drawn got larger in
// proportion, and — the part that actually cost something — every
// interior number was then expressed in WORLD coordinates. A region's
// layout was constrained by where its rectangle happened to sit and
// how big it was relative to its neighbours, so a narrow region got a
// cramped interior for a reason that had nothing to do with the
// region.
//
// A region map is a different picture of a different thing, not the
// same picture nearer. It owns its whole canvas and lays out for what
// it actually contains. That is both what was asked for and the
// simpler thing to draw: the camera, its easing and its interpolation
// are gone.
//
// THE RECT IS A `Territory` because that is what the three layout
// functions already take — `interiorLayout`, `platformLayout` and
// `machineryStrip` each place their contents inside one. Handing them
// a canvas-sized rect instead of a world-sized slot is the entire
// change on their side; none of them needed editing.

import type { RegionName } from './regions';
import { WORLD, type Territory } from './world';

/** As wide as the world, so a region map and the world map scale the
 *  same way on the page and the eye does not have to re-measure. The
 *  height is the room a region's own contents want, not the height of
 *  a slot on a line — which is the whole point of the swap. */
export const REGION_CANVAS = { width: WORLD.width, height: 420 } as const;

/** The region's rect: its whole canvas. Named `territoryOfRegionView`
 *  rather than `rect` because what it returns IS a Territory — the
 *  same shape the world hands the layout functions, so one definition
 *  of "lay your contents out in here" serves both views. */
export function regionCanvas(name: RegionName): Territory {
  return { name, x: 0, y: 0, w: REGION_CANVAS.width, h: REGION_CANVAS.height };
}
