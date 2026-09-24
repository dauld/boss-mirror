// THE IT MAP MOVES — the arithmetic of the motion layer (design
// 31bade8f, decided by David 2026-09-24; this is car M2 on backlog
// d220022f). David, after car D's region renders: "we need to get more
// ambitious with our real-time animations / movements to better
// capture the actual dynamism of what is going on and more easily see
// the stalled areas."
//
// MOTION MEANS WORK, AND ONLY WORK (decision 1). A thing moves on the
// map only where the server's reading says work crosses: a token is a
// crossing replayed at the rail's measured 24h rate, evenly spaced
// (the rate is an average, so no burst is invented), at ONE speed on
// every rail — density carries the rate, never a second channel. So
// stillness can mean stopped, which is the stall signal (decision 4).
//
// NOTHING HERE DECIDES THAT A RAIL HAS STALLED. `railStill` reads the
// server's judgement — the border's state, its `flowing` (car M1, from
// the rail's own mean gap) and its measured rate — the rule
// `borders.ts` already states: "nothing here derives a judgement". A
// number drawn here is a number the server sent: the pile is its
// `waiting` shaded by its `holds_by_class`, the held clock counts from
// its `held_since` against its own `now`.
//
// Pure functions of the reading and the clock, so `bun test` pins
// every rule without a canvas (decision 10: "the mocked spec pins the
// arithmetic ... it never snapshots pixels"). MotionLayer.svelte owns
// the frames and the strokes.

import type { Border, HoldsByClass } from './borders';
import type { RegionState } from './regions';
import { BADGE_HALF, type Rail, type Territory } from './world';

export type Point = Readonly<{ x: number; y: number }>;

// ---------------------------------------------------------------------
// The stated rules (decision 10), each a number someone can read.
// ---------------------------------------------------------------------

/** Time compression: pause, ×60, ×600, ×3600. */
export const COMPRESSIONS = [0, 60, 600, 3600] as const;
export type Compression = (typeof COMPRESSIONS)[number];
/** ×600 by default, stated on screen (Q1, decided 2026-09-24): one real
 *  hour of crossings passes in six seconds. */
export const DEFAULT_COMPRESSION: Compression = 600;
/** Past this many tokens a second on one rail, a token stands for k
 *  crossings — drawn hollow, with "●=k" beside the rate. */
export const MAX_EMIT_PER_SEC = 5;
/** And never more than this many in flight on one rail. */
export const MAX_IN_FLIGHT = 60;
/** World units a token travels per real second — the same on every
 *  rail, because the server reports no crossing time to encode. */
export const SPEED = 40;
/** The map's poll (MapPage's tick). */
export const READ_EVERY_MS = 10_000;
/** Past this many missed reads the whole map greys and stops. */
export const STALE_AFTER_MISSED = 3;
/** A pile's squares drop in or fade out over this long after a read. */
export const TWEEN_MS = 400;
/** The one ring on entering troubled (Q2): this long, once. */
export const RING_MS = 900;
/** The draw budget per frame at 500 tokens. */
export const FRAME_BUDGET_MS = 2;

/** What the header says the tokens are — always on screen (decision 2). */
export const REPLAY_TEXT = "tokens replay each rail's measured 24h rate; they are not individual events";

/** The compression, stated: `×600 — 1 real hour = 6 s on screen`. */
export function compressionText(c: number): string {
  if (c <= 0) return 'paused — nothing travels';
  const seconds = 3600 / c;
  const span = seconds >= 60 ? `${Math.round(seconds / 60)} min` : `${Math.round(seconds * 10) / 10} s`;
  return `×${c} — 1 real hour = ${span} on screen`;
}

// ---------------------------------------------------------------------
// The server's judgement, read.
// ---------------------------------------------------------------------

/** Why a rail does not move, or null when it does:
 *  - `unknown` — no reading of its rate or of whether it flows: still
 *    and grey, never mistaken for flowing (decision 5);
 *  - `troubled` — the server marks the border troubled;
 *  - `held` — the server says it is not flowing (M1), or it measured
 *    nothing crossing. */
export type Stillness = 'unknown' | 'troubled' | 'held' | null;

export function railStill(b: Border | undefined): Stillness {
  if (b === undefined || b.rate.current === null || b.flowing === null) return 'unknown';
  if (b.state === 'troubled') return 'troubled';
  if (b.flowing === false || b.rate.current <= 0) return 'held';
  return null;
}

/** How often a rail emits a token, per REAL second, and how many
 *  crossings one token stands for. `k` rises past MAX_EMIT_PER_SEC and
 *  past MAX_IN_FLIGHT tokens on a rail `length` long — the cap stated,
 *  never hidden. Nothing is emitted for an unmeasured or empty rate,
 *  or while paused. */
export type Emission = Readonly<{ per: number; k: number }>;

export function emitPerSec(perDay: number | null, compression: number, length = 0): Emission {
  if (perDay === null || perDay <= 0 || compression <= 0) return { per: 0, k: 1 };
  const raw = (perDay / 86_400) * compression;
  const byRate = Math.ceil(raw / MAX_EMIT_PER_SEC);
  const byFlight = length > 0 ? Math.ceil((raw * length) / SPEED / MAX_IN_FLIGHT) : 1;
  const k = Math.max(1, byRate, byFlight);
  return { per: raw / k, k };
}

/** World units between two tokens: speed over rate. */
export const spacingOf = (e: Emission): number => (e.per > 0 ? SPEED / e.per : Number.POSITIVE_INFINITY);

// ---------------------------------------------------------------------
// Tokens on a rail.
// ---------------------------------------------------------------------

/** One token: how far along its rail, and how many crossings it is. */
export type Token = Readonly<{ d: number; k: number }>;
/** A rail's tokens and its emission accumulator (fractions of a token). */
export type Flow = Readonly<{ tokens: ReadonlyArray<Token>; acc: number }>;

/** The rail as it stands after running a while — it never starts empty
 *  and fills (decision 2, prefill). The same positions are the static
 *  density a reduced-motion reader sees (decision 9). */
export function prefill(length: number, e: Emission): Flow {
  if (e.per <= 0 || length <= 0) return { tokens: [], acc: 0.5 };
  const s = spacingOf(e);
  const n = Math.min(MAX_IN_FLIGHT, Math.max(0, Math.ceil(length / s - 0.5)));
  return { tokens: Array.from({ length: n }, (_, i) => ({ d: (i + 0.5) * s, k: e.k })), acc: 0.5 };
}

/** `dt` real seconds later: every token travels, a token past the end
 *  has crossed and leaves, and — while the rail is `emitting` — new
 *  ones are born at the head, evenly spaced. A rail that stops
 *  emitting keeps its tokens until they finish their trip (decision 4). */
export function advance(flow: Flow, dt: number, length: number, e: Emission, emitting: boolean): Flow {
  const moved = flow.tokens.map((t) => ({ d: t.d + SPEED * dt, k: t.k })).filter((t) => t.d < length);
  if (!emitting || e.per <= 0) return { tokens: moved, acc: flow.acc };
  const acc = flow.acc + e.per * dt;
  const n = Math.floor(acc);
  const rest = acc - n;
  // The j-th newest was born (rest + j) / per seconds ago.
  const born = Array.from({ length: n }, (_, j) => ({ d: (SPEED * (rest + j)) / e.per, k: e.k })).filter(
    (t) => t.d < length,
  );
  return { tokens: [...moved, ...born].slice(0, MAX_IN_FLIGHT), acc: rest };
}

// ---------------------------------------------------------------------
// Walking a rail's path at constant speed.
// ---------------------------------------------------------------------

/** The points of an SVG path in the absolute commands world.ts writes
 *  (M, L, H, V, Q), the quadratic turns flattened — so a token walks the
 *  very line the SVG strokes. */
export function pathPoints(d: string): ReadonlyArray<Point> {
  const tokens = d.match(/[MLHVQ]|-?\d*\.?\d+(?:e-?\d+)?/gi) ?? [];
  type Walk = Readonly<{ points: ReadonlyArray<Point>; at: Point; cmd: string; args: ReadonlyArray<number> }>;
  const flush = (w: Walk): Walk => {
    const a = w.args;
    const need = { M: 2, L: 2, H: 1, V: 1, Q: 4 }[w.cmd.toUpperCase()] ?? 0;
    if (need === 0 || a.length < need) return w;
    const cmd = w.cmd.toUpperCase();
    const next: Walk = { ...w, args: a.slice(need) };
    if (cmd === 'M' || cmd === 'L') {
      const at = { x: a[0]!, y: a[1]! };
      return flush({ ...next, at, points: [...w.points, at] });
    }
    if (cmd === 'H') {
      const at = { x: a[0]!, y: w.at.y };
      return flush({ ...next, at, points: [...w.points, at] });
    }
    if (cmd === 'V') {
      const at = { x: w.at.x, y: a[0]! };
      return flush({ ...next, at, points: [...w.points, at] });
    }
    const c = { x: a[0]!, y: a[1]! };
    const end = { x: a[2]!, y: a[3]! };
    const curve = Array.from({ length: 8 }, (_, i) => {
      const t = (i + 1) / 8;
      const u = 1 - t;
      return { x: u * u * w.at.x + 2 * u * t * c.x + t * t * end.x, y: u * u * w.at.y + 2 * u * t * c.y + t * t * end.y };
    });
    return flush({ ...next, at: end, points: [...w.points, ...curve] });
  };
  const start: Walk = { points: [], at: { x: 0, y: 0 }, cmd: '', args: [] };
  const done = tokens.reduce<Walk>(
    (w, tok) => (/^[a-z]$/i.test(tok) ? { ...flush(w), cmd: tok, args: [] } : flush({ ...w, args: [...w.args, Number(tok)] })),
    start,
  );
  return flush(done).points;
}

/** A path with its cumulative lengths, for walking it by distance. */
export type Walked = Readonly<{ points: ReadonlyArray<Point>; at: ReadonlyArray<number>; length: number }>;

export function walk(points: ReadonlyArray<Point>): Walked {
  const at = points.reduce<ReadonlyArray<number>>(
    (acc, p, i) => (i === 0 ? [0] : [...acc, acc[i - 1]! + Math.hypot(p.x - points[i - 1]!.x, p.y - points[i - 1]!.y)]),
    [],
  );
  return { points, at, length: at[at.length - 1] ?? 0 };
}

/** The point `d` along a walked path (clamped to its ends). */
export function pointAt(w: Walked, d: number): Point {
  if (w.points.length === 0) return { x: 0, y: 0 };
  if (d <= 0) return w.points[0]!;
  if (d >= w.length) return w.points[w.points.length - 1]!;
  let lo = 0;
  let hi = w.at.length - 1;
  while (hi - lo > 1) {
    const mid = (lo + hi) >> 1;
    if (w.at[mid]! < d) lo = mid;
    else hi = mid;
  }
  const a = w.points[lo]!;
  const b = w.points[hi]!;
  const span = w.at[hi]! - w.at[lo]!;
  const f = span === 0 ? 0 : (d - w.at[lo]!) / span;
  return { x: a.x + (b.x - a.x) * f, y: a.y + (b.y - a.y) * f };
}

// ---------------------------------------------------------------------
// Piles: the server's count, shaded by whom it waits on (decision 3).
// ---------------------------------------------------------------------

/** One square's shade: red sediment, solid ink in line for a machine,
 *  hollow ink waiting on a person or the world, grey the server cannot
 *  tell. */
export type PileClass = 'stuck' | 'machine' | 'person' | 'unknown';

/** A pile nobody could count is its own thing — a dashed "?" box, never
 *  an empty pile. */
export type Pile =
  | Readonly<{ kind: 'unknown' }>
  | Readonly<{ kind: 'counted'; total: number; classes: HoldsByClass }>;

export function pileOf(b: Border | undefined): Pile {
  if (b === undefined || b.waiting === null) return { kind: 'unknown' };
  // A count with no classes (an older server, a half-read block): every
  // square is one the server cannot tell, not a guess from the words.
  const classes = b.holds_by_class ?? { machine: 0, person: 0, unknown: b.waiting, stuck: 0 };
  return { kind: 'counted', total: b.waiting, classes };
}

/** Sediment first — at the base, next to the rail's head — then the
 *  machine's line, then the people's, then the unknown. */
const PILE_ORDER: ReadonlyArray<PileClass> = ['stuck', 'machine', 'person', 'unknown'];

/** The squares a pile draws in `capacity` cells, and how many more it
 *  holds than it draws ("+N"). The count printed is always the
 *  server's `waiting`, never a count of squares. Classes that sum short
 *  of the count leave the rest grey; the squares never exceed it. */
export function pileSquares(p: Pile, capacity: number): Readonly<{ squares: ReadonlyArray<PileClass>; more: number }> {
  if (p.kind === 'unknown') return { squares: [], more: 0 };
  const cap = Math.max(0, Math.min(capacity, p.total));
  const named = PILE_ORDER.reduce<ReadonlyArray<PileClass>>(
    (acc, k) => [...acc, ...Array.from({ length: Math.max(0, Math.min(p.classes[k], cap - acc.length)) }, () => k)],
    [],
  );
  const squares = [...named, ...Array.from({ length: cap - named.length }, (): PileClass => 'unknown')];
  return { squares, more: p.total - squares.length };
}

/** A pile's grid: cells of `PILE_PITCH`, from its base row down. */
export const PILE_PITCH = 6;
export const PILE_SQUARE = 5;
export type PileBox = Readonly<{ x: number; y: number; cols: number; rows: number; more: Point }>;

/** Where a rail's pile stands, at its HEAD: on the line, inside the
 *  territory it leaves, in the corner the rail climbs out of — clear of
 *  the name and the count, which sit on the left; on a siding, a
 *  column beside the drop, clear of the waiting badge standing on it.
 *  `more` is where "+N" is written, right-aligned. */
export function pileBox(rail: Rail, from: Territory): PileBox {
  if (rail.kind === 'siding') {
    const cols = 2;
    const rows = Math.max(1, Math.floor((rail.to.y - rail.from.y - 8 - 10) / PILE_PITCH));
    const x = rail.from.x - BADGE_HALF - 2 - cols * PILE_PITCH;
    const y = rail.from.y + 4;
    return { x, y, cols, rows, more: { x: x + cols * PILE_PITCH, y: y + rows * PILE_PITCH + 8 } };
  }
  const cols = 6;
  const rows = 4;
  const x = from.x + from.w - 4 - cols * PILE_PITCH;
  const y = from.y + 4;
  return { x, y, cols, rows, more: { x: x + cols * PILE_PITCH, y: y + rows * PILE_PITCH + 8 } };
}

export const capacityOf = (b: PileBox): number => b.cols * b.rows;

/** The top-left of the `n`th square, row by row from the base. */
export function squareAt(b: PileBox, n: number): Point {
  return { x: b.x + (n % b.cols) * PILE_PITCH, y: b.y + Math.floor(n / b.cols) * PILE_PITCH };
}

/** How opaque the `n`th square is `ageMs` after a read moved the pile
 *  from `before` squares to `after`: arrivals drop in, departures fade,
 *  the rest stand — only ever between two reads' counts (decision 6).
 *  Squares never ride off down a rail: a smaller pile is not proof of a
 *  crossing. */
export function squareAlpha(n: number, before: number, after: number, ageMs: number): number {
  const p = Math.min(1, Math.max(0, ageMs / TWEEN_MS));
  if (n < Math.min(before, after)) return 1;
  if (n < after) return p;
  if (n < before) return 1 - p;
  return 0;
}

// ---------------------------------------------------------------------
// Clocks: arithmetic on a server value, never a new value (decision 6).
// ---------------------------------------------------------------------

/** How long the rail has been held: the server's `now` less its
 *  `held_since`, plus the time since that read on this page's clock —
 *  so a skewed client clock cannot move it. Null when the server named
 *  no onset. */
export function heldMs(b: Border, serverNow: string, readAtMs: number, nowMs: number): number | null {
  if (b.held_since === null) return null;
  const since = Date.parse(b.held_since);
  const at = Date.parse(serverNow);
  if (!Number.isFinite(since) || !Number.isFinite(at)) return null;
  return Math.max(0, at - since) + Math.max(0, nowMs - readAtMs);
}

/** `40s`, `12m`, `3h 12m`, `2d 4h`. */
export function clockText(ms: number): string {
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h ${String(m % 60).padStart(2, '0')}m`;
  return `${Math.floor(h / 24)}d ${h % 24}h`;
}

/** The rate under a rail on the moving map: `24/d`, `501/d ●=3` when a
 *  token stands for several crossings, `0 vs 4/d` when nothing arrives
 *  where work used to (decision 4), `?` unmeasured. */
export function motionRateText(b: Border | undefined, e: Emission): string {
  if (b === undefined || b.rate.current === null) return '?';
  const n = (v: number): string => String(Math.round(v * 10) / 10);
  if (b.rate.current <= 0 && (b.rate.previous ?? 0) > 0) return `0 vs ${n(b.rate.previous!)}/d`;
  return `${n(b.rate.current)}/d${e.k > 1 ? ` ●=${e.k}` : ''}`;
}

/** What a still rail says beneath its rate — the one moving thing left
 *  on it is this clock. Empty for a moving or an unknown rail, and for a
 *  troubled rail that is still flowing (its state and why say it). */
export function heldText(b: Border | undefined, still: Stillness, held: number | null): string {
  if (b === undefined || still === null || still === 'unknown') return '';
  if (held !== null) return `held ${clockText(held)}`;
  if (b.flowing === false) return 'held — nothing crossed in the read';
  return '';
}

/** Seconds since the last good read once past STALE_AFTER_MISSED missed
 *  reads — the map greys and stops, because a frozen map must never
 *  keep moving as if it were live — or null while it is live. */
export function staleFor(readAtMs: number | null, nowMs: number): number | null {
  if (readAtMs === null) return null;
  const age = nowMs - readAtMs;
  return age >= (STALE_AFTER_MISSED + 1) * READ_EVERY_MS ? Math.floor(age / 1000) : null;
}

// ---------------------------------------------------------------------
// The one ring (Q2): once, on crossing INTO troubled.
// ---------------------------------------------------------------------

/** The keys whose state crossed INTO troubled between two reads. The
 *  first read is not a transition, and neither is a key the previous
 *  read did not hold: nothing rings for a state it did not see change. */
export function newlyTroubled(
  prev: ReadonlyMap<string, RegionState> | null,
  next: ReadonlyMap<string, RegionState>,
): ReadonlyArray<string> {
  if (prev === null) return [];
  return [...next.entries()]
    .filter(([k, s]) => s === 'troubled' && prev.has(k) && prev.get(k) !== 'troubled')
    .map(([k]) => k);
}

/** The ring `ageMs` into its one expansion: how far out it has grown
 *  and how faint it is — null once it is over, and it never repeats. */
export function ringAt(ageMs: number): Readonly<{ grow: number; alpha: number }> | null {
  if (ageMs < 0 || ageMs >= RING_MS) return null;
  const p = ageMs / RING_MS;
  return { grow: 28 * (1 - (1 - p) * (1 - p)), alpha: 1 - p };
}
