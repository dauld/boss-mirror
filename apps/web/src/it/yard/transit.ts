// THE IT MAP AS A TRANSIT MONITOR — the layout and the words (design
// 16091dfb, answered by David 2026-09-25; backlog ced4ca8b). David,
// 2026-09-24: "I would actually love if our visuals more generally felt
// like a transit monitoring map. I think that is a good way to think
// about the network."
//
// NOTHING HERE DERIVES A JUDGEMENT. Every number is read from the
// server: a section is held because the server's `flowing` is false, a
// station is troubled because the server's state says so, an alarm
// carries the server's own `why`. What this owns is the drawing grammar
// — stations on schematic lines at fixed angles, sections of track
// between them in one colour per line, the waiting packets as blocks on
// the approach, a moving block at the section's real crossing rate, the
// headway a dispatcher reads, and an alarms board in place of a verdict
// written inside a territory.
//
// THE EDGES ARE SERVED, NOT DRAWN (design e765b3fc, car R3 on feedback
// 84cba7e2). Until this car the sections were a hand-written list here
// (PATHS: ten pairs, each with a hand-drawn SVG path), one of three
// copies pinned equal to each other and to nothing else — and it drew
// the train straight from the dock to the track, skipping the gates its
// train gate runs in, the one route David named. The sections are now
// exactly the routes `GET /api/yard/routes` serves (routes.ts): each is
// laid out by `routePath`, a small octilinear router between the two
// stations' positions, and each exit is an off-ramp. The planned tenant
// branch (design fd8b5143) went with PATHS: no route serves it, and
// nothing is drawn that the record cannot source.
//
// WHAT STAYS DATA HERE is the STATIONS: a layout is not a route. A
// station's position is where the map stands it; which stations a line
// joins is the server's answer. Pure functions of the reading, so `bun
// test` pins every rule without a DOM; TransitMap.svelte owns the strokes
// and the motion.

import type { Border } from './borders';
import type { Region, RegionState, Regions } from './regions';
import type { Route, Routes } from './routes';
import { type Point, type Walked, pathPoints, pointAt, walk } from './world-motion';

/** The drawing's own coordinate space. */
export const TRANSIT_VIEW = { width: 1000, height: 400 } as const;

/** The lines, each one colour (Q1, decided 2026-09-25). A route takes
 *  the line of the station it serves off the main line — the garage's
 *  sidings, the publish dock — and the delivery line otherwise. The
 *  tenant line's token stays in styles.css for the branch design
 *  fd8b5143 plans; nothing draws it until a route serves it. */
export type TransitLine = 'delivery' | 'publish' | 'siding' | 'tenant';

/** Each line's colour is a Design-department token in styles.css — never
 *  a literal here, so the Design department re-colours a line by editing
 *  one block (the a-colour-is-a-token lint, map-palette.test.ts). The
 *  state rings are the map's own ok / warn / bad tokens, kept separate
 *  from the line colours so a red line never reads as a troubled one. */
export const LINE_TOKEN: Readonly<Record<TransitLine, string>> = {
  delivery: '--map-line-delivery',
  publish: '--map-line-publish',
  siding: '--map-line-siding',
  tenant: '--map-line-tenant',
};

export const LINE_LABEL: Readonly<Record<TransitLine, string>> = {
  delivery: 'delivery line',
  publish: 'publish line (GitHub)',
  siding: 'garage sidings',
  tenant: 'tenant branch (planned)',
};

// ---------------------------------------------------------------------
// The layout: where each station stands. Data, not routes.
// ---------------------------------------------------------------------

/** A station: a region, where it stands, whether its words go under it
 *  (the stations off the main line) or around it, and the line it
 *  stands on. */
export type Station = Readonly<{ name: string; x: number; y: number; below: boolean; line: TransitLine }>;

const MAIN_Y = 200;

export const STATIONS: ReadonlyArray<Station> = [
  { name: 'receiving', x: 60, y: MAIN_Y, below: false, line: 'delivery' },
  { name: 'marshalling', x: 180, y: MAIN_Y, below: false, line: 'delivery' },
  { name: 'shop-floor', x: 300, y: MAIN_Y, below: false, line: 'delivery' },
  { name: 'gates', x: 420, y: MAIN_Y, below: false, line: 'delivery' },
  { name: 'dock', x: 540, y: MAIN_Y, below: false, line: 'delivery' },
  { name: 'track', x: 660, y: MAIN_Y, below: false, line: 'delivery' },
  { name: 'arrivals', x: 780, y: MAIN_Y, below: false, line: 'delivery' },
  { name: 'shed', x: 900, y: MAIN_Y, below: false, line: 'delivery' },
  { name: 'garage', x: 580, y: 320, below: true, line: 'siding' },
  { name: 'publish', x: 860, y: 330, below: true, line: 'publish' },
];

export const stationOf = (name: string): Station | undefined => STATIONS.find((s) => s.name === name);

/** Where a main-line station writes its name, above its ring — high
 *  enough that an entry's stub and the arcs over the line clear it. */
export const NAME_ABOVE = 38;

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
export function routePath(a: Station, b: Station, stations: ReadonlyArray<Station>, twoWay: boolean): ReadonlyArray<Point> {
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
        ? routePath(a, b, stations, served.has(sectionKey(route.to, route.from)))
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
// Stations: words.
// ---------------------------------------------------------------------

export const stationLabel = (name: string): string => name.replace(/-/g, ' ');

/** The count in its unit, against a capacity where the region has one.
 *  A threshold is not room ("6 / 1 cars parked" read as six in a space
 *  for one), so it is left to the title; an unread count is "no
 *  reading", never 0. */
export function stationCount(r: Region | undefined): string {
  if (r === undefined || r.count === null) return 'no reading';
  const unit = r.unit === '' ? '' : ` ${r.unit}`;
  return r.bound !== null && r.bound_kind !== 'threshold' ? `${r.count} / ${r.bound}${unit}` : `${r.count}${unit}`;
}

/** A station's ring: the region's state, and troubled for a region the
 *  read did not carry — the same rule the world map draws by. */
export const ringOf = (r: Region | undefined): RegionState => r?.state ?? 'troubled';

const NO_READING = 'the server answered no reading for this region';

// ---------------------------------------------------------------------
// Sections: the server's stillness, and the headway.
// ---------------------------------------------------------------------

/** How a section's track is drawn: in its line's colour while work
 *  crosses, RED where the server's `flowing` is false — the stall shown
 *  where it happens, as a signal would show it — and in its line's
 *  colour dotted where the server cannot tell. */
export type Ground = 'flowing' | 'held' | 'unknown';

export function sectionGround(b: Border | undefined): Ground {
  if (b === undefined || b.flowing === null) return 'unknown';
  return b.flowing ? 'flowing' : 'held';
}

const perDayText = (v: number): string => String(Math.round(v * 10) / 10);

/** The mean gap between crossings, from the day's rate. */
export function gapText(perDay: number): string {
  if (perDay <= 0) return 'none crossed';
  const minutes = 1440 / perDay;
  return minutes < 60 ? `${Math.round(minutes)}m gap` : `${(minutes / 60).toFixed(1)}h gap`;
}

/** The server's held_since as the stack's own clock (UTC). */
function heldSinceText(iso: string | null): string {
  if (iso === null) return 'held';
  const at = new Date(iso);
  return Number.isNaN(at.getTime()) ? 'held' : `held ${at.toISOString().slice(11, 16)}Z`;
}

/** The number a dispatcher reads on a section: crossings a day and the
 *  mean gap — or, on a held section, since when ("held 08:48Z"). A rate
 *  nobody measured is "no reading", never 0/d. Written in the section's
 *  panel since car N2 of design e765b3fc, not on the track: the map
 *  carries names, one number and states, and nothing else. */
export function headwayText(b: Border | undefined): string {
  const rate = b?.rate.current ?? null;
  if (b === undefined || rate === null) return 'no reading';
  const tail = b.flowing === false ? heldSinceText(b.held_since) : gapText(rate);
  return `${perDayText(rate)}/d · ${tail}`;
}

// ---------------------------------------------------------------------
// Waiting blocks: the server's count, on the approach.
// ---------------------------------------------------------------------

/** The most blocks one approach draws; the rest are a count. */
export const MAX_BLOCKS = 6;
const FIRST_BLOCK = 18;
const BLOCK_PITCH = 11;

/** The packets waiting to cross into the destination, as blocks on its
 *  approach — nearest the station first — and past MAX_BLOCKS a "+n" where
 *  the next block would stand. An unread count draws nothing: the
 *  headway already says "no reading". */
export function waitingBlocks(
  s: Section,
  b: Border | undefined,
): Readonly<{ blocks: ReadonlyArray<Point>; more: Readonly<{ at: Point; n: number }> | null }> {
  const waiting = b?.waiting ?? 0;
  if (waiting <= 0) return { blocks: [], more: null };
  const at = (i: number): Point => pointAt(s.walked, Math.max(0, s.walked.length - FIRST_BLOCK - i * BLOCK_PITCH));
  const shown = Math.min(waiting, MAX_BLOCKS);
  return {
    blocks: Array.from({ length: shown }, (_, i) => at(i)),
    more: waiting > MAX_BLOCKS ? { at: at(MAX_BLOCKS), n: waiting - MAX_BLOCKS } : null,
  };
}

// ---------------------------------------------------------------------
// The moving block: the real crossing rate, replayed ×60.
// ---------------------------------------------------------------------

/** The replay: one real minute is one second on screen. Stated on the
 *  page, because a moving block that did not say what it replays would
 *  read as an event. */
export const REPLAY = 60;
export const REPLAY_TEXT = 'a moving block is the section\'s measured 24h crossing rate, replayed ×60 — not an individual packet';
/** Slower than one departure in this many screen seconds, a section
 *  draws no train: a block that crosses twice a minute reads as a stall,
 *  and the headway already prints the rate. */
export const MAX_PERIOD_S = 30;
/** The shortest crossing a block makes. A section busier than one
 *  departure per MIN_TRAVEL_S runs several blocks, staggered, so the
 *  departures still keep the rate rather than a flicker standing in
 *  for it. */
export const MIN_TRAVEL_S = 1.2;
const MAX_TRAINS = 6;

/** A section's trains: each crosses in `dur` seconds, looping, and
 *  `begins` staggers them one period apart — so ONE departure every
 *  86400 / rate / REPLAY seconds, the section's real rate. Null where
 *  nothing should move: under reduced motion, and wherever the server
 *  does not say work crosses (held, cannot tell, unread, empty). */
export function trainsOf(
  b: Border | undefined,
  reduced: boolean,
): Readonly<{ dur: number; begins: ReadonlyArray<number> }> | null {
  if (reduced || b === undefined || b.flowing !== true) return null;
  const rate = b.rate.current;
  if (rate === null || rate <= 0) return null;
  const period = 86_400 / rate / REPLAY;
  if (period > MAX_PERIOD_S) return null;
  const k = Math.min(MAX_TRAINS, Math.ceil(MIN_TRAVEL_S / period));
  return { dur: k * period, begins: Array.from({ length: k }, (_, i) => (i === 0 ? 0 : -i * period)) };
}

// ---------------------------------------------------------------------
// The alarms board.
// ---------------------------------------------------------------------

/** The states that raise an alarm: not clear, and not full — a station
 *  at its capacity and moving is the good state (design e765b3fc §4a). */
type Alarming = Exclude<RegionState, 'clear' | 'full'>;

export type Alarm = Readonly<{ name: string; label: string; state: Alarming; why: string }>;

const SEVERITY: Readonly<Record<Alarming, number>> = { troubled: 0, attention: 1 };

/** Every station that is neither clear nor full, with the server's own
 *  why — troubled first, then attention, each in line order, then any
 *  region the payload carries that the map has no station for. A station
 *  the read did not carry is an alarm: an unread station is not a quiet
 *  one. */
export function alarmsOf(read: Regions): ReadonlyArray<Alarm> {
  const names = [...new Set([...STATIONS.map((s) => s.name), ...read.regions.map((r) => r.name)])];
  return names
    .flatMap((name): Alarm[] => {
      const r = read.regions.find((x) => x.name === name);
      const state = ringOf(r);
      if (state === 'clear' || state === 'full') return [];
      return [{ name, label: stationLabel(name), state, why: r?.why ?? NO_READING }];
    })
    .map((a, i) => ({ a, i }))
    .sort((x, y) => SEVERITY[x.a.state] - SEVERITY[y.a.state] || x.i - y.i)
    .map(({ a }) => a);
}
