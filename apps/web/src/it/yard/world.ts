// THE IT WORLD — the layout of the system map as data (design
// d2154293, decided by David 2026-09-19; this is car 1). One SVG, one
// coordinate space: the regions are TERRITORIES laid out along the
// real packet flow, left to right the way packets travel —
//
//     receiving -> marshalling -> shop-floor -> dock -> gates -> track -> arrivals -> shed
//                                                         \-> garage (a siding off gates / track)
//                                                                          \-> publish (a siding off arrivals)
//
// — and the BORDERS between them are declared here, each a track
// segment WorldMap.svelte draws between two territories. Car 2 hangs
// the crossing rate, what waits to cross and the machinery that moves
// it on those segments; car 3 zooms the viewBox into a territory. The
// 4x2 card grid this replaced (0524fc95 car 2) had no shared space, no
// adjacency and no flow — "a world map in a video game where the
// physical layout and connections make sense" (feedback c3105b2a).
//
// NOTHING HERE IS A PIXEL SOMEONE PLACED BY HAND: the line is one rule
// (a slot per hop), the garage spans the two territories it hangs off,
// and a further region is a row in this file plus a border, pinned by
// world.test.ts against the server's REGIONS.

import type { RegionName } from './regions';

export type Territory = Readonly<{
  name: RegionName;
  x: number;
  y: number;
  w: number;
  h: number;
}>;

/** A hop of the flow: packets cross from `from` into `to`. */
export type Border = Readonly<{ from: RegionName; to: RegionName }>;

/** The world's extent — the SVG viewBox. As wide as the yard map
 *  (YardMap's VIEW_W), so the two scale the same way on a page; the
 *  line's slots divide the width between the margins, leaving `GAP`
 *  between each pair for the border rails to run in. */
export const WORLD = { width: 1240, height: 400 } as const;

// The line: one slot each, at the yard's margin. The slot WIDTH is
// derived from how many there are — the line always spans the world
// between its margins — so adding a territory (the shop floor did,
// backlog 94c6ffd0) narrows every slot by the same rule instead of
// running the last one off the right edge. Nothing here is a pixel
// someone placed by hand.
const LINE: ReadonlyArray<RegionName> = [
  'receiving',
  'marshalling',
  'shop-floor',
  'dock',
  'gates',
  'track',
  'arrivals',
  'shed',
];
const MARGIN = 20;
/** The gap between two slots, which is the room the border rails run
 *  in: a 10-unit gap hid them (2026-09-19). */
const GAP = 24;
const SLOT = Math.floor((WORLD.width - 2 * MARGIN) / LINE.length);
const LINE_W = SLOT - GAP;
const LINE_Y = 24;
const LINE_H = 200;
const slotX = (i: number): number => MARGIN + i * SLOT;

const line: ReadonlyArray<Territory> = LINE.map((name, i) => ({ name, x: slotX(i), y: LINE_Y, w: LINE_W, h: LINE_H }));

// The garage: a siding under the gates and the track, spanning both —
// a red gate sends a car there, and so does a red train.
const gatesSlot = LINE.indexOf('gates');
const trackSlot = LINE.indexOf('track');
const garage: Territory = {
  name: 'garage',
  x: slotX(gatesSlot),
  y: LINE_Y + LINE_H + 30,
  w: slotX(trackSlot) + LINE_W - slotX(gatesSlot),
  h: 120,
};

// The publish dock: the crossing OUT of this world (design cb38d806,
// backlog eee42416). What arrived on main is what a publish proposes to
// the public mirror, so it is a siding under arrivals and the shed —
// the same row as the garage, at the far end of the line, because a
// packet that reaches it has left the yard's own loop.
const arrivalsSlot = LINE.indexOf('arrivals');
const shedSlot = LINE.indexOf('shed');
const publish: Territory = {
  name: 'publish',
  x: slotX(arrivalsSlot),
  y: garage.y,
  w: slotX(shedSlot) + LINE_W - slotX(arrivalsSlot),
  h: garage.h,
};

export const TERRITORIES: ReadonlyArray<Territory> = [...line, garage, publish];

/** The line's hops, in flow order, then the crossing out to the mirror
 *  and the garage's two feeders — the same set and order as the
 *  server's `boss_jobs::borders::BORDERS`, pinned by borders.test.ts. */
export const BORDERS: ReadonlyArray<Border> = [
  ...LINE.slice(1).map((to, i): Border => ({ from: LINE[i]!, to })),
  { from: 'arrivals', to: 'publish' },
  { from: 'gates', to: 'garage' },
  { from: 'track', to: 'garage' },
];

export function territoryOf(name: string): Territory | undefined {
  return TERRITORIES.find((t) => t.name === name);
}

/** A border's segment: it leaves the facing edge of `a` and arrives at
 *  the facing edge of `b`, at the middle of the width (or height) the
 *  two share — a straight run when they share one, a turnout's curve
 *  (the yard's own `C` shape) when they do not. */
export function borderPath(a: Territory, b: Territory): Readonly<{ x1: number; y1: number; x2: number; y2: number; d: string }> {
  const right = b.x >= a.x + a.w;
  const left = b.x + b.w <= a.x;
  if (right || left) {
    const lo = Math.max(a.y, b.y);
    const hi = Math.min(a.y + a.h, b.y + b.h);
    const [y1, y2] = lo < hi ? [(lo + hi) / 2, (lo + hi) / 2] : [a.y + a.h / 2, b.y + b.h / 2];
    const x1 = right ? a.x + a.w : a.x;
    const x2 = right ? b.x : b.x + b.w;
    const mx = (x1 + x2) / 2;
    return { x1, y1, x2, y2, d: `M${x1} ${y1} C ${mx} ${y1}, ${mx} ${y2}, ${x2} ${y2}` };
  }
  const below = b.y >= a.y + a.h;
  const lo = Math.max(a.x, b.x);
  const hi = Math.min(a.x + a.w, b.x + b.w);
  const [x1, x2] = lo < hi ? [(lo + hi) / 2, (lo + hi) / 2] : [a.x + a.w / 2, b.x + b.w / 2];
  const y1 = below ? a.y + a.h : a.y;
  const y2 = below ? b.y : b.y + b.h;
  const my = (y1 + y2) / 2;
  return { x1, y1, x2, y2, d: `M${x1} ${y1} C ${x1} ${my}, ${x2} ${my}, ${x2} ${y2}` };
}

/** Words onto lines of at most `width` characters (a word longer than
 *  the width stands alone, whole), at most `lines` of them — the rest
 *  is cut with an ellipsis, and the whole text rides the element's
 *  `<title>`. SVG text does not wrap; this is the map's own wrap. */
export function wrapWords(text: string, width: number, lines: number): ReadonlyArray<string> {
  const words = text.split(/\s+/).filter((w) => w.length > 0);
  const wrapped = words.reduce<ReadonlyArray<string>>((acc, word) => {
    const last = acc[acc.length - 1];
    if (last !== undefined && `${last} ${word}`.length <= width) return [...acc.slice(0, -1), `${last} ${word}`];
    return [...acc, word];
  }, []);
  if (wrapped.length <= lines) return wrapped;
  const rest = wrapped.slice(lines - 1).join(' ');
  return [...wrapped.slice(0, lines - 1), `${rest.slice(0, width - 1)}…`];
}
