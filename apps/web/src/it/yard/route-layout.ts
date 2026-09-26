// THE ROUTE LAYOUT — where every route the server serves is drawn
// (design e765b3fc, car R3 on feedback 84cba7e2).
//
// ONE PLACE ANSWERS "WHAT PATH DOES THE MOVE (from, to) TRAVEL". The
// transit map draws its sections, exits and entries from here
// (RouteLayer.svelte), and the moves of car M2 — a ping at a station, a
// slow transit between two — travel the same paths, so a dot can never
// run along a line the map does not draw:
//
//     routePath(routes, 'gates', 'track')   the section a train departs by
//     routePath(routes, 'dock', null)       the dock's off-ramp (an exit)
//     routePath(routes, null, 'receiving')  receiving's entry stub
//
// each answering `{ d, walked }` — the SVG path, and the walked polyline
// `pointAt(walked, t · walked.length)` reads a point along — or null
// where the server serves no such route (a move on it is then one the
// map cannot draw, and says so rather than inventing a track). In the
// DOM every drawn element carries the same key, `data-section="<from>→<to>"`
// (an empty end for an exit or an entry), with `data-from` / `data-to`.
//
// NOTHING HERE IS AN EDGE. The station positions are transit.ts's data
// (a layout is not a route), the routes are the server's, and the lint
// `a-map-edge-is-served-not-drawn` refuses a station-pair literal in
// this directory. Pure functions, so `bun test` pins every rule.

import type { Route, Routes } from './routes';
import { LINE_TOKEN, STATIONS, type Station, type TransitLine } from './transit';
import { type Point, type Walked, pathPoints, walk } from './world-motion';

// ---------------------------------------------------------------------
// The router: a served route onto the page, at fixed angles only.
// ---------------------------------------------------------------------

/** How far an arc over (or under) the main line stands off it: past the
 *  station names above, past the counts below, and a step further for
 *  every station it passes, so two arcs over one stretch never share a
 *  track. */
export const ARC_ABOVE = 62;
export const ARC_BELOW = 56;
export const ARC_STEP = 14;
/** How far apart the two directions of one straight run are drawn. */
export const LANE = 5;
/** An entry's stub and an exit's off-ramp: from outside the ring to
 *  this far out. */
const RING = 12;
export const RAMP = 14;

const fmt = (n: number): string => String(Math.round(n * 100) / 100);
const pathOf = (pts: ReadonlyArray<Point>): string =>
  pts.map((p, i) => `${i === 0 ? 'M' : 'L'}${fmt(p.x)} ${fmt(p.y)}`).join(' ');

/** A polyline moved `by` units to the right of its own direction of
 *  travel — the lane a straight run takes when the run back is served
 *  too, as a double track keeps its directions apart. */
function shifted(pts: ReadonlyArray<Point>, by: number): ReadonlyArray<Point> {
  const a = pts[0]!;
  const b = pts[pts.length - 1]!;
  const len = Math.hypot(b.x - a.x, b.y - a.y) || 1;
  const nx = (-(b.y - a.y) / len) * by;
  const ny = ((b.x - a.x) / len) * by;
  return pts.map((p) => ({ x: p.x + nx, y: p.y + ny }));
}

/** THE OCTILINEAR ROUTER (design e765b3fc §2c: "each section's path is
 *  computed by a small octilinear router between two station positions,
 *  with parallel lines offset"). Every leg is horizontal, vertical or at
 *  45°, and the path starts at `a`'s centre and ends at `b`'s:
 *
 *  - ON THE MAIN LINE, forward to the next station: straight along it.
 *    Forward past a station, it rises over the line in an arc — 45° up,
 *    along, 45° down — so it never runs through the station it skips.
 *    Backward, it drops under the line the same way, so the two
 *    directions between two stations never share a track (the train
 *    made up at the dock runs BACK to the gates under the line, then
 *    over the dock to the track).
 *  - BETWEEN ROWS, a straight leg first and then the 45° into `b`, so
 *    the run back — straight first from ITS end — draws the other half
 *    of a parallelogram rather than over this one.
 *  - A run that is one straight leg (vertical, or exactly 45°) is moved
 *    LANE to the right of its travel when the run back is served too.
 *
 *  `stations` is the layout the arcs clear; `twoWay` whether `b → a` is
 *  served as well. */
export function octilinear(a: Station, b: Station, stations: ReadonlyArray<Station>, twoWay: boolean): ReadonlyArray<Point> {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  if (dy === 0) {
    const lo = Math.min(a.x, b.x);
    const hi = Math.max(a.x, b.x);
    const passed = stations.filter((s) => s.y === a.y && s.x > lo && s.x < hi).length;
    const forward = dx > 0;
    if (forward && passed === 0) return [{ x: a.x, y: a.y }, { x: b.x, y: b.y }];
    const steps = forward ? passed - 1 : passed;
    const want = (forward ? ARC_ABOVE : ARC_BELOW) + ARC_STEP * steps;
    const lift = Math.min(want, Math.abs(dx) / 2);
    const y = a.y + (forward ? -lift : lift);
    const dir = Math.sign(dx);
    return [
      { x: a.x, y: a.y },
      { x: a.x + dir * lift, y },
      { x: b.x - dir * lift, y },
      { x: b.x, y: b.y },
    ];
  }
  const ax = Math.abs(dx);
  const ay = Math.abs(dy);
  if (dx === 0 || ax === ay) {
    const pts = [{ x: a.x, y: a.y }, { x: b.x, y: b.y }];
    return twoWay ? shifted(pts, LANE) : pts;
  }
  const elbow =
    ax > ay
      ? { x: a.x + Math.sign(dx) * (ax - ay), y: a.y }
      : { x: a.x, y: a.y + Math.sign(dy) * (ay - ax) };
  return [{ x: a.x, y: a.y }, elbow, { x: b.x, y: b.y }];
}

/** An exit's off-ramp: out of the ring and away from the line — down
 *  from a main-line station (its name is above), right from a station
 *  under the line (its name is below it) — ending at a buffer stop. */
export function exitPath(s: Station): ReadonlyArray<Point> {
  return s.below
    ? [{ x: s.x + RING, y: s.y }, { x: s.x + RING + RAMP, y: s.y }]
    : [{ x: s.x, y: s.y + RING }, { x: s.x, y: s.y + RING + RAMP }];
}

/** An entry's stub: into the ring from above a main-line station, from
 *  the left of one under the line. */
export function entryPath(s: Station): ReadonlyArray<Point> {
  return s.below
    ? [{ x: s.x - RING - RAMP, y: s.y }, { x: s.x - RING, y: s.y }]
    : [{ x: s.x, y: s.y - RING - RAMP }, { x: s.x, y: s.y - RING }];
}

// ---------------------------------------------------------------------
// The sections: every served route, laid out.
// ---------------------------------------------------------------------

/** A border's key, in the arrow the world map and its specs use — and
 *  the key a drawn section answers to: `gates→track` for a section,
 *  `dock→` for the dock's exit, `→receiving` for receiving's entry. The
 *  moves feed (car M2) finds the path a move `(from, to)` travels by
 *  this key. */
export const sectionKey = (from: string | null, to: string | null): string => `${from ?? ''}→${to ?? ''}`;

/** A section of track, an exit or an entry: the route it draws, its
 *  line, and its path — `d` for the stroke, `walked` for anything that
 *  travels it (the waiting blocks, the moving blocks, and the moves of
 *  car M2, which call `pointAt(walked, t · walked.length)`). */
export type Section = Readonly<{
  key: string;
  kind: 'section' | 'exit' | 'entry';
  from: string | null;
  to: string | null;
  line: TransitLine;
  /** False: only the moves record supports it — drawn dashed red. */
  declared: boolean;
  route: Route;
  d: string;
  walked: Walked;
}>;

const lineOf = (a: Station | undefined, b: Station | undefined): TransitLine =>
  [a, b].find((s) => s !== undefined && s.line !== 'delivery')?.line ?? 'delivery';

/** EVERY SERVED ROUTE, LAID OUT — and the ones this layout cannot place,
 *  said rather than dropped: a route to a region the map has no station
 *  for is named at the foot of the map. */
export function sectionsOf(
  routes: Routes,
  stations: ReadonlyArray<Station> = STATIONS,
): Readonly<{ sections: ReadonlyArray<Section>; unplaced: ReadonlyArray<string> }> {
  const at = (name: string | null): Station | undefined => (name === null ? undefined : stations.find((s) => s.name === name));
  const served = new Set(routes.routes.map((r) => sectionKey(r.from, r.to)));
  const placed = routes.routes.map((route): Section | string => {
    const key = sectionKey(route.from, route.to);
    const a = at(route.from);
    const b = at(route.to);
    const unplacedEnd = (route.from !== null && a === undefined) || (route.to !== null && b === undefined);
    if (unplacedEnd) return key;
    const pts =
      a !== undefined && b !== undefined
        ? octilinear(a, b, stations, served.has(sectionKey(route.to, route.from)))
        : a !== undefined
          ? exitPath(a)
          : entryPath(b!);
    const d = pathOf(pts);
    return {
      key,
      kind: a !== undefined && b !== undefined ? 'section' : a !== undefined ? 'exit' : 'entry',
      from: route.from,
      to: route.to,
      line: lineOf(a, b),
      declared: route.declared,
      route,
      d,
      walked: walk(pathPoints(d)),
    };
  });
  return {
    sections: placed.filter((p): p is Section => typeof p !== 'string'),
    unplaced: placed.filter((p): p is string => typeof p === 'string'),
  };
}

/** The lines the served routes draw, in the key's order — the key names
 *  only what is on the map. */
export function linesOf(sections: ReadonlyArray<Section>): ReadonlyArray<TransitLine> {
  const drawn = new Set(sections.map((s) => s.line));
  return (Object.keys(LINE_TOKEN) as TransitLine[]).filter((l) => drawn.has(l));
}

// ---------------------------------------------------------------------
// The one lookup the moves (car M2) travel by.
// ---------------------------------------------------------------------

/** A drawn route's path: `d` for a stroke, `walked` for anything that
 *  travels it. */
export type DrawnPath = Readonly<{ key: string; kind: Section['kind']; d: string; walked: Walked }>;

/** THE PATH THE MOVE (from, to) TRAVELS ON THE MAP — a section between
 *  two stations, an exit (`to` null: the off-ramp its terminal takes),
 *  an entry (`from` null) — or null where the server serves no such
 *  route, or serves it to a region this layout has no station for.
 *  Laid out exactly as RouteLayer draws it: the same routes give the
 *  same path, lanes and arcs included. */
export function routePath(routes: Routes, from: string | null, to: string | null): DrawnPath | null {
  const key = sectionKey(from, to);
  const s = sectionsOf(routes).sections.find((x) => x.key === key);
  return s === undefined ? null : { key: s.key, kind: s.kind, d: s.d, walked: s.walked };
}
