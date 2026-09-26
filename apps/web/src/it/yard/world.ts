// THE IT WORLD — the layout of the system map as data (design
// d2154293, decided by David 2026-09-19; this is car 1). One SVG, one
// coordinate space: the regions are TERRITORIES laid out along the
// real packet flow, left to right the way a car travels —
//
//     receiving -> marshalling -> shop-floor -> gates -> dock -> track -> arrivals -> shed
//                                                  \-> garage (a siding under gates .. track)
//                                                                          \-> publish (a siding off arrivals)
//
// — and WorldMap.svelte draws a rail between two territories for each
// border the server answers (never a pair declared here; car R3 of
// design e765b3fc). Car 2 hangs the
// crossing rate, what waits to cross and the machinery that moves it on
// those rails. The 4x2 card grid this replaced (0524fc95 car 2) had no
// shared space, no adjacency and no flow — "a world map in a video game
// where the physical layout and connections make sense" (feedback
// c3105b2a).
//
// THE ORDER IS THE ONE A CAR WALKS (design 62de32ae decision 3, decided
// 2026-09-24): built on the shop floor, gated, parked on the dock,
// boarded, arrived, proven. The line put the dock BEFORE the gates until
// then, and the rails' own crossings said otherwise — a car followed
// across the map walked backwards through two regions.
//
// THE RAILS RUN ABOVE THE LINE (decision 6: borders drawn, not
// tooltipped). Eight territories wide enough to hold their own numbers
// leave about 24 units between two of them, which is where the rails
// used to run — room for a circled count and nothing else, so the
// crossing, the machine and its last firing lived in hover titles. Each
// rail now leaves the top of its territory, runs along a main line
// above the row and drops into the next one: a run of most of a slot,
// with a band above it to write the machine's name and lamp on and the
// rate beneath. The sidings drop straight down from the line to the
// garage and the publish dock, with their writing beside them.
//
// NOTHING HERE IS A PIXEL SOMEONE PLACED BY HAND: the line is one rule
// (a slot per hop), the heights are the bands stacked, the garage spans
// the territories it hangs off, and a rail is a function of the two
// territories it joins. A further region is a row in this file, pinned
// by world.test.ts against the server's REGIONS.

import type { RegionName } from './regions';
import { MACHINERY_STRIP_H } from './world-machines';

/** A rectangle in world coordinates. */
export type Box = Readonly<{ x: number; y: number; w: number; h: number }>;

export type Territory = Readonly<{
  name: RegionName;
  x: number;
  y: number;
  w: number;
  h: number;
}>;

/** The line, in the order a car walks it. */
export const LINE: ReadonlyArray<RegionName> = [
  'receiving',
  'marshalling',
  'shop-floor',
  'gates',
  'dock',
  'track',
  'arrivals',
  'shed',
];

/** As wide as the yard map (YardMap's VIEW_W), so the two scale the
 *  same way on a page. */
const WIDTH = 1240;
const MARGIN = 20;
/** The gap between two territories on the line. The rails no longer
 *  run in it (they run above), so it is only air between two boxes. */
const GAP = 24;
// The slot WIDTH is derived from how many there are — the line always
// spans the world between its margins — so adding a territory (the
// shop floor did, backlog 94c6ffd0) narrows every slot by the same rule
// instead of running the last one off the right edge.
const SLOT = Math.floor((WIDTH - 2 * MARGIN) / LINE.length);
const LINE_W = SLOT - GAP;

/** The type the rails are written in: 12 units a line (the map's own
 *  line height), ~5.8 units a character of the 9px mono the status and
 *  rate use, ~6.1 of the 10px the machine's NAME is written in — the
 *  one word on a rail an operator has to be able to read at a glance
 *  (review of 2026-09-24, finding 5: "legible type"). */
const LINE_STEP = 12;
const CHAR_W = 5.8;
const NAME_CHAR_W = 6.1;

/** THE BAND ABOVE THE LINE, top to bottom: up to two lines of the
 *  machine's name and one of its lamp and status (the label), the
 *  main line itself, the rate under it, and the drop into the
 *  territory. */
const LABEL_TOP = 8;
const LABEL_LINES_ABOVE = 3;
export const RAIL_Y = LABEL_TOP + LABEL_LINES_ABOVE * LINE_STEP + 18;
const RATE_ROOM = 18;
const LINE_Y = RAIL_Y + RATE_ROOM + 16;
/** A line territory's height. The review of 2026-09-24 measured the
 *  boxes about 60% empty at 200 and asked them to shrink so the rails
 *  had length; the rails now get their length from the main line
 *  instead, and car A (decisions 1, 5, 9) filled the room with the
 *  KPI and the band that decided the state — its text runs to within
 *  a line of the machinery strip at 200, so 200 it stays
 *  (`territoryText`, pinned in world.test.ts). */
const LINE_H = 200;

/** The drop from the line to the sidings — where the siding rails run
 *  and their writing sits beside them: four lines of the map's type. */
const SIDING_GAP = 4 * LINE_STEP + 18;
const SIDING_Y = LINE_Y + LINE_H + SIDING_GAP;
const SIDING_H = 120;

/** The world's extent — the SVG viewBox. Its height is the bands
 *  stacked, plus room at the foot for the map's "not on the map" notes. */
export const WORLD = { width: WIDTH, height: SIDING_Y + SIDING_H + 2 * LINE_STEP + 4 } as const;

const slotX = (i: number): number => MARGIN + i * SLOT;

const line: ReadonlyArray<Territory> = LINE.map((name, i) => ({ name, x: slotX(i), y: LINE_Y, w: LINE_W, h: LINE_H }));

/** A siding under the line, spanning the slots from `first` to `last`. */
function siding(name: RegionName, first: RegionName, last: RegionName): Territory {
  const a = LINE.indexOf(first);
  const b = LINE.indexOf(last);
  return { name, x: slotX(a), y: SIDING_Y, w: slotX(b) + LINE_W - slotX(a), h: SIDING_H };
}

// The garage: a siding under the gates and the track, spanning both and
// the dock between — a red gate sends a car there, and so does a red
// train. The publish dock: the crossing OUT of this world (design
// cb38d806, backlog eee42416) — what arrived on main is what a publish
// proposes to the public mirror, so it hangs under arrivals and the shed.
const garage = siding('garage', 'gates', 'track');
const publish = siding('publish', 'arrivals', 'shed');

export const TERRITORIES: ReadonlyArray<Territory> = [...line, garage, publish];

// THE BORDERS ARE NOT HERE (design e765b3fc, car R3). This file declared
// the ten pairs WorldMap drew rails for, one of three hand copies (with
// boss_jobs::borders::BORDERS and transit.ts PATHS) pinned equal to each
// other and to nothing else. The world map now draws a rail for each
// border the server's borders read answers, and the transit map the
// routes the server derives from the protocols; the lint
// `a-map-edge-is-served-not-drawn` refuses a pair written back in here.

export function territoryOf(name: string): Territory | undefined {
  return TERRITORIES.find((t) => t.name === name);
}

/** One rail as the map draws it: its path, where it leaves and
 *  arrives, the point ON it where the waiting badge stands, the box its
 *  writing goes in (the machine, its lamp and the rate — never over a
 *  territory or another rail's writing, world.test.ts), and how long
 *  its run is. */
export type Rail = Readonly<{
  kind: 'line' | 'siding';
  d: string;
  from: Readonly<{ x: number; y: number }>;
  to: Readonly<{ x: number; y: number }>;
  mid: Readonly<{ x: number; y: number }>;
  label: Box;
  length: number;
}>;

/** How far off a territory's centre its rails meet its top edge: the
 *  one coming in on the left, the one going out on the right, so the
 *  direction of travel reads at every station. */
const STUB = 12;
/** The turnout's radius where a rail leaves or joins the main line. */
const TURN = 8;
/** Half the widest waiting badge a rail carries (a four-digit count). */
export const BADGE_HALF = 16;

/** The rail between two territories. Side by side on the line, it
 *  climbs out of the top of `a` right of its centre, runs along the
 *  main line and drops into `b` left of its centre, its writing in the
 *  band above. One below the other, it drops straight from `a`'s bottom
 *  to `b`'s top, in the middle of the width the two share, with its
 *  writing beside it. */
export function railOf(a: Territory, b: Territory): Rail {
  if (b.y >= a.y + a.h) {
    const lo = Math.max(a.x, b.x);
    const hi = Math.min(a.x + a.w, b.x + b.w);
    const x = lo < hi ? (lo + hi) / 2 : a.x + a.w / 2;
    const y1 = a.y + a.h;
    const y2 = b.y;
    return {
      kind: 'siding',
      d: `M${x} ${y1} V${y2}`,
      from: { x, y: y1 },
      to: { x, y: y2 },
      mid: { x, y: (y1 + y2) / 2 },
      // Beside the rail, clear of the badge standing on it, and short
      // of the next siding a slot along.
      label: { x: x + BADGE_HALF + 4, y: y1 + 4, w: SLOT - 2 * (BADGE_HALF + 4), h: y2 - y1 - 8 },
      length: y2 - y1,
    };
  }
  const x1 = a.x + a.w / 2 + STUB;
  const x2 = b.x + b.w / 2 - STUB;
  const y = RAIL_Y;
  const mx = (x1 + x2) / 2;
  return {
    kind: 'line',
    d: [
      `M${x1} ${a.y}`,
      `V${y + TURN}`,
      `Q${x1} ${y} ${x1 + TURN} ${y}`,
      `H${x2 - TURN}`,
      `Q${x2} ${y} ${x2} ${y + TURN}`,
      `V${b.y}`,
    ].join(' '),
    from: { x: x1, y: a.y },
    to: { x: x2, y: b.y },
    mid: { x: mx, y },
    // The whole band from the top of the world to just above the drop,
    // a slot wide less a margin either side, centred on the border.
    label: { x: mx - (SLOT - 8) / 2, y: LABEL_TOP - 4, w: SLOT - 8, h: y + RATE_ROOM - LABEL_TOP + 4 },
    length: x2 - x1,
  };
}

/** Where a territory's words go (text baselines), from car A's reading
 *  of a region: its name, its count, the lamp and the state, the KPI in
 *  up to `kpiLines` lines, the trend's metric and its parts in
 *  `trendLines`, and the verdict — the band that decided the state and,
 *  for trouble, the why — in `verdictLines`. `chars` is how many
 *  characters of the 9px type fit across the left column,
 *  `verdictChars` across the column the verdict is in.
 *
 *  A territory on the line is one column, top to bottom. A SIDING is
 *  wide and short, so its trend and verdict move to a second column a
 *  slot in, beside the KPI rather than under it: a verdict printed
 *  below the outline is a verdict nobody reads. */
export type TerritoryText = Readonly<{
  chars: number;
  verdictChars: number;
  name: At;
  count: At;
  lamp: At;
  state: At;
  kpi: At;
  kpiLines: number;
  metric: At;
  trend: At;
  trendLines: number;
  verdict: At;
  verdictLines: number;
}>;

/** The room a column of the 9px type leaves in `w` units. */
const columnChars = (w: number): number => Math.floor((w - 16) / CHAR_W);

export function territoryText(t: Territory): TerritoryText {
  const x = t.x + 8;
  // The last baseline a line may sit on: above the machinery strip.
  const floor = t.y + t.h - MACHINERY_STRIP_H - 2;
  const fit = (y: number, most: number): number => Math.max(0, Math.min(most, Math.floor((floor - y) / LINE_STEP) + 1));
  const top = {
    name: { x, y: t.y + 18 },
    count: { x, y: t.y + 44 },
    lamp: { x: t.x + 12, y: t.y + 58 },
    state: { x: t.x + 22, y: t.y + 62 },
    kpi: { x, y: t.y + 80 },
  };
  const trendLines = 2;
  if (t.w <= SLOT) {
    const metric = { x, y: t.y + 108 };
    const trend = { x, y: metric.y + LINE_STEP };
    const verdict = { x, y: trend.y + trendLines * LINE_STEP + 4 };
    return {
      ...top,
      chars: columnChars(t.w),
      verdictChars: columnChars(t.w),
      kpiLines: 2,
      metric,
      trend,
      trendLines,
      verdict,
      verdictLines: fit(verdict.y, 4),
    };
  }
  const x2 = t.x + SLOT;
  const metric = { x: x2, y: t.y + 18 };
  const trend = { x: x2, y: metric.y + LINE_STEP };
  const verdict = { x: x2, y: trend.y + trendLines * LINE_STEP + 4 };
  return {
    ...top,
    chars: columnChars(SLOT - 8),
    verdictChars: columnChars(t.x + t.w - x2 + 8),
    kpiLines: fit(top.kpi.y, 2),
    metric,
    trend,
    trendLines,
    verdict,
    verdictLines: fit(verdict.y, 4),
  };
}

/** How many characters of a machine's name fit across a label. */
export const labelChars = (label: Box): number => Math.floor(label.w / NAME_CHAR_W);

type At = Readonly<{ x: number; y: number }>;

/** Where a rail's words go (text baselines, and the lamp's centre),
 *  inside its label: the machine's name in `nameLines` lines, the
 *  lamp with the machine's status (`statusChars` long) beside it, and
 *  the rate. On a line rail the name and lamp sit ABOVE the main line,
 *  centred on the border, and the rate under it; on a siding they
 *  stack beside the drop. `anchor` is how the name and the rate hang
 *  off their point; the lamp and status always start where they are
 *  placed, so the pair reads as one. */
export type Writing = Readonly<{
  anchor: 'middle' | 'start';
  names: ReadonlyArray<At>;
  lamp: At;
  status: At;
  rate: At;
}>;

export function railWriting(rail: Rail, nameLines: number, statusChars: number): Writing {
  const l = rail.label;
  if (rail.kind === 'siding') {
    const x = l.x + 2;
    const top = l.y + LINE_STEP - 2;
    const names = Array.from({ length: nameLines }, (_, i) => ({ x, y: top + i * LINE_STEP }));
    const statusY = top + nameLines * LINE_STEP;
    return {
      anchor: 'start',
      names,
      lamp: { x: x + 3, y: statusY - 3 },
      status: { x: x + 9, y: statusY },
      rate: { x, y: statusY + LINE_STEP },
    };
  }
  const cx = l.x + l.w / 2;
  const statusY = rail.mid.y - 14;
  const names = Array.from({ length: nameLines }, (_, i) => ({
    x: cx,
    y: statusY - (nameLines - i) * LINE_STEP,
  }));
  // The lamp and its status as one centred pair: a lamp's width and a
  // space, then the words.
  const pair = 9 + statusChars * CHAR_W;
  const left = Math.max(l.x, cx - pair / 2);
  return {
    anchor: 'middle',
    names,
    lamp: { x: left + 3, y: statusY - 3 },
    status: { x: left + 9, y: statusY },
    rate: { x: cx, y: rail.mid.y + 16 },
  };
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

/** A machine's name onto a rail: like `wrapWords`, but a registry name
 *  (`publish-github-pr-on-open-pr-ready`) also breaks AFTER a hyphen,
 *  so each line still reads as part of one name rather than as two
 *  words. At most `lines` lines; the rest is cut with an ellipsis. */
export function wrapName(text: string, width: number, lines: number): ReadonlyArray<string> {
  // Each piece carries what joins it to the next: nothing after a
  // hyphen, a space after a word.
  const pieces = text.match(/[^\s-]*-|[^\s-]+/g) ?? [];
  const joined = (a: string, b: string): string => (a.endsWith('-') ? `${a}${b}` : `${a} ${b}`);
  const wrapped = pieces.reduce<ReadonlyArray<string>>((acc, piece) => {
    const last = acc[acc.length - 1];
    if (last !== undefined && joined(last, piece).length <= width) return [...acc.slice(0, -1), joined(last, piece)];
    return [...acc, piece];
  }, []);
  if (wrapped.length <= lines) return wrapped;
  const rest = wrapped.slice(lines - 1).reduce((a, b) => joined(a, b));
  return [...wrapped.slice(0, lines - 1), `${rest.slice(0, width - 1)}…`];
}
