// THE IT MAP AS A TRANSIT MONITOR — the layout and the words (design
// 16091dfb, answered by David 2026-09-25; backlog ced4ca8b). David,
// 2026-09-24: "I would actually love if our visuals more generally felt
// like a transit monitoring map. I think that is a good way to think
// about the network."
//
// THE DATA DOES NOT CHANGE. Every number here is read from the same two
// endpoints the world map reads, /api/yard/regions and /api/yard/borders,
// and nothing here derives a judgement: a section is held because the
// server's `flowing` is false, a station is troubled because the
// server's state says so, an alarm carries the server's own `why`. What
// changes is the drawing grammar — stations on schematic lines at fixed
// angles, sections of track between them in one colour per route, the
// waiting packets as blocks on the approach, a moving block at the
// section's real crossing rate, the headway a dispatcher reads, and an
// alarms board in place of a verdict written inside a territory.
//
// The layout is the exhibit E1 reading's (the 22:15Z rendering of
// 2026-09-24), and it is held to world.ts by transit.test.ts: one station
// per territory and one section per border, so a region added to the
// world without a station here fails a test rather than vanishing from
// this map. Pure functions of the reading, so `bun test` pins every rule
// without a DOM; TransitMap.svelte owns the strokes and the motion.

import type { Border } from './borders';
import type { Region, RegionState, Regions } from './regions';
import { type Point, type Walked, pathPoints, pointAt, walk } from './world-motion';

/** The drawing's own coordinate space. */
export const TRANSIT_VIEW = { width: 1000, height: 400 } as const;

/** The routes, each one colour (Q1, decided 2026-09-25). The tenant
 *  branch is planned (design fd8b5143) and is drawn dashed. */
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
// The layout.
// ---------------------------------------------------------------------

/** A station: a region, where it stands, and whether its words go under
 *  it (the stations off the main line) or around it. */
export type Station = Readonly<{ name: string; x: number; y: number; below: boolean }>;

const MAIN_Y = 200;

export const STATIONS: ReadonlyArray<Station> = [
  { name: 'receiving', x: 60, y: MAIN_Y, below: false },
  { name: 'marshalling', x: 180, y: MAIN_Y, below: false },
  { name: 'shop-floor', x: 300, y: MAIN_Y, below: false },
  { name: 'gates', x: 420, y: MAIN_Y, below: false },
  { name: 'dock', x: 540, y: MAIN_Y, below: false },
  { name: 'track', x: 660, y: MAIN_Y, below: false },
  { name: 'arrivals', x: 780, y: MAIN_Y, below: false },
  { name: 'shed', x: 930, y: MAIN_Y, below: false },
  { name: 'garage', x: 540, y: 310, below: true },
  { name: 'publish', x: 900, y: 320, below: true },
];

export const stationOf = (name: string): Station | undefined => STATIONS.find((s) => s.name === name);

/** A section of track: a border, the line it belongs to, and its path. */
export type Section = Readonly<{ key: string; from: string; to: string; line: TransitLine; d: string; walked: Walked }>;

/** Every leg horizontal or at 45° — the schematic's fixed angles. */
const PATHS: ReadonlyArray<Readonly<{ from: string; to: string; line: TransitLine; d: string }>> = [
  { from: 'receiving', to: 'marshalling', line: 'delivery', d: 'M60 200 H180' },
  { from: 'marshalling', to: 'shop-floor', line: 'delivery', d: 'M180 200 H300' },
  { from: 'shop-floor', to: 'gates', line: 'delivery', d: 'M300 200 H420' },
  { from: 'gates', to: 'dock', line: 'delivery', d: 'M420 200 H540' },
  { from: 'dock', to: 'track', line: 'delivery', d: 'M540 200 H660' },
  { from: 'track', to: 'arrivals', line: 'delivery', d: 'M660 200 H780' },
  { from: 'arrivals', to: 'shed', line: 'delivery', d: 'M780 200 H930' },
  { from: 'arrivals', to: 'publish', line: 'publish', d: 'M780 200 L900 320' },
  { from: 'gates', to: 'garage', line: 'siding', d: 'M420 200 L530 310 H540' },
  { from: 'track', to: 'garage', line: 'siding', d: 'M660 200 L550 310 H540' },
];

/** A border's key, in the arrow the world map and its specs use. */
export const sectionKey = (from: string, to: string): string => `${from}→${to}`;

export const SECTIONS: ReadonlyArray<Section> = PATHS.map((p) => ({
  ...p,
  key: sectionKey(p.from, p.to),
  walked: walk(pathPoints(p.d)),
}));

/** THE PLANNED TENANT BRANCH (design fd8b5143): a tenant's change leaves
 *  the shop floor, is checked, waits on David's approval — a signed plan
 *  hash, so that station is marked as his — and joins at the shed. Drawn
 *  dashed and read from nothing: it is a plan, not a reading, and no
 *  endpoint serves it yet. */
export type BranchStation = Readonly<{ name: string; x: number; y: number; owner: string | null }>;

const BRANCH_D = 'M300 200 L390 110 H810 L900 200 H930';

export const TENANT_BRANCH: Readonly<{
  d: string;
  walked: Walked;
  stations: ReadonlyArray<BranchStation>;
  note: Readonly<{ text: string; x: number; y: number }>;
}> = {
  d: BRANCH_D,
  walked: walk(pathPoints(BRANCH_D)),
  stations: [
    { name: 'tenant check', x: 480, y: 110, owner: null },
    { name: 'awaiting approval', x: 610, y: 110, owner: 'David' },
    { name: 'tenant main', x: 740, y: 110, owner: null },
  ],
  note: { text: "David's station: a signed plan hash", x: 610, y: 138 },
};

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

export type Alarm = Readonly<{ name: string; label: string; state: Exclude<RegionState, 'clear'>; why: string }>;

const SEVERITY: Readonly<Record<Exclude<RegionState, 'clear'>, number>> = { troubled: 0, attention: 1 };

/** Every station that is not clear, with the server's own why —
 *  troubled first, then attention, each in line order, then any region
 *  the payload carries that the map has no station for. A station the
 *  read did not carry is an alarm: an unread station is not a quiet one. */
export function alarmsOf(read: Regions): ReadonlyArray<Alarm> {
  const names = [...new Set([...STATIONS.map((s) => s.name), ...read.regions.map((r) => r.name)])];
  return names
    .flatMap((name): Alarm[] => {
      const r = read.regions.find((x) => x.name === name);
      const state = ringOf(r);
      if (state === 'clear') return [];
      return [{ name, label: stationLabel(name), state, why: r?.why ?? NO_READING }];
    })
    .map((a, i) => ({ a, i }))
    .sort((x, y) => SEVERITY[x.a.state] - SEVERITY[y.a.state] || x.i - y.i)
    .map(({ a }) => a);
}
