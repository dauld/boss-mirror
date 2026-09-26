// REAL PACKETS MOVING ON THE MAP — the rules (design e765b3fc §3, car M2
// on feedback 84cba7e2; behind the flight `it-map-live`). David,
// 2026-09-25: "visualize the actual jobs/cars moving as the animation
// rather than a representative ... a ping and a slow-mo transit between
// the stations to show the jobs moving."
//
// NOTHING MOVES ON THE MAP THAT DID NOT MOVE IN THE RECORD. Every dot is
// one row of `yard_moves` as GET /api/yard/moves/stream sends it (car
// M1, crates/core/boss-jobs/src/moves.rs `Recorded`): one packet whose
// place changed, stamped with the event that changed it. This file does
// not place anything — the server's one placement function did — it only
// decides how a recorded move is DRAWN: a dot along its route over a
// fixed 6 s, a ping when it lands, a burst as one ping "×k", past forty
// dots a ping with no transit and "+n" on the section, a hand-off as one
// continuous stroke only where the record links the two identities, and
// nothing at all while the tab is hidden.
//
// THE GEOMETRY IS A PARAMETER, not an import. Which line a route is drawn
// on is the map's business (car R3 serves the routes and routes them
// octilinearly); this reads it through `Geometry`, so the motion rides
// whatever lines the map draws and never keeps a copy of them (CLAUDE.md
// §9a). live-geometry.ts wires it to the transit map.
//
// Pure functions of (state, move, clock) so `bun test` pins every rule;
// LiveMotion.svelte owns the stream, the frames and the strokes.

import { type Point, type Walked, pathPoints, pointAt, walk } from './world-motion';

/** One recorded move, as the stream's `move` frame carries it: the
 *  server's `Recorded` with its `Move` flattened in. `from` / `to` are
 *  null off the map — a packet filed onto it, or leaving by an off-ramp.
 *  `declared` is whether a derived route supports it (null: unjudged). */
export type Move = Readonly<{
  seq: number;
  at: string;
  packet: string;
  kind: string;
  label: string;
  from: string | null;
  to: string | null;
  declared: boolean | null;
  cause_event_id: string;
  cause_seq: number;
  cause_kind: string;
  handoff_from: string | null;
  lineage: string | null;
  aboard: ReadonlyArray<string>;
}>;

/** A drawn route: `d` for its stroke, `walked` for what travels it
 *  (route-layout.ts `DrawnPath` carries both). */
export type RouteLine = Readonly<{ d: string; walked: Walked }>;

/** The map's lines as the motion reads them. `path` answers the route a
 *  move travels — between two stations, an exit (`to` null: the off-ramp
 *  a packet leaves the map by) or an entry (`from` null) — or null where
 *  the map draws no such route, which is a move no served route supports. */
export type RoutePath = (from: string | null, to: string | null) => RouteLine | null;

export type Geometry = Readonly<{
  path: RoutePath;
  /** Where a station stands, or null for a name the map does not draw. */
  stationAt: (name: string) => Point | null;
  /** The line a route belongs to, for the colour its dot and ping wear. */
  lineOf?: (from: string | null, to: string | null) => string | null;
}>;

/** Every crossing takes this long on screen, whatever the section's
 *  length. The record's move is instantaneous; LIVE_TEXT says so. */
export const TRANSIT_MS = 6_000;
/** A ping's one swell. */
export const PING_MS = 700;
/** Arrivals at one station inside this window are one ping, "×k". */
export const BURST_MS = 1_000;
/** The most dots in flight; past it an arrival pings with no transit and
 *  its section shows "+n" (the 2 ms frame budget is measured at this). */
export const MAX_DOTS = 40;
/** How far past its station an off-ramp without a drawn line runs: off
 *  the bottom of the transit map's 400-unit view. */
const OFF_MAP_Y = 440;

export const LIVE_TEXT =
  'each dot is one packet that moved at the time on its label; the crossing is shown over 6 s';

/** A dot in flight: one move, its route walked, its window on screen. */
export type Dot = Readonly<{
  id: number;
  move: Move;
  d: string;
  walked: Walked;
  start: number;
  end: number;
  /** The route no derived route supports: drawn dashed red under it. */
  undeclared: boolean;
  /** Leaving by an off-ramp: it fades off the map and pings nowhere. */
  off: boolean;
  /** Where the identity it continues faded (a hand-off), else null. */
  handoff: Point | null;
  /** One per car aboard a train; 0 for anything else. */
  pips: number;
  line: string;
}>;

export type Ping = Readonly<{
  key: string;
  station: string;
  at: Point;
  start: number;
  k: number;
  line: string;
  /** Reduced motion: a count flash that does not swell. */
  still: boolean;
}>;

/** An arrival that got no dot: counted on its section for as long as its
 *  crossing would have taken. */
type Ghost = Readonly<{ key: string; at: Point; until: number }>;

export type Motion = Readonly<{
  dots: ReadonlyArray<Dot>;
  pings: ReadonlyArray<Ping>;
  ghosts: ReadonlyArray<Ghost>;
  /** Moves while the tab was hidden, by the station each touched last. */
  away: Readonly<Record<string, number>>;
}>;

export const EMPTY: Motion = { dots: [], pings: [], ghosts: [], away: {} };

// ---------------------------------------------------------------------
// The wire.
// ---------------------------------------------------------------------

const str = (v: unknown): v is string => typeof v === 'string';
const strOrNull = (v: unknown): v is string | null => v === null || typeof v === 'string';
const num = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);

function json(data: string): Record<string, unknown> | null {
  try {
    const v: unknown = JSON.parse(data);
    return v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

/** A `move` frame's data, or null for anything that is not a whole move —
 *  half a move is never drawn. */
export function parseMove(data: string): Move | null {
  const o = json(data);
  if (o === null) return null;
  const { seq, at, packet, kind, label, from, to, declared, cause_event_id, cause_seq, cause_kind, handoff_from, lineage, aboard } = o;
  const ok =
    num(seq) && str(at) && str(packet) && str(kind) && str(label) && strOrNull(from) && strOrNull(to) &&
    (declared === null || declared === undefined || typeof declared === 'boolean') &&
    str(cause_event_id) && num(cause_seq) && str(cause_kind) &&
    (handoff_from === undefined || strOrNull(handoff_from)) && (lineage === undefined || strOrNull(lineage)) &&
    (aboard === undefined || (Array.isArray(aboard) && aboard.every(str)));
  if (!ok) return null;
  return {
    seq, at, packet, kind, label, from, to,
    declared: declared ?? null,
    cause_event_id, cause_seq, cause_kind,
    handoff_from: handoff_from ?? null,
    lineage: lineage ?? null,
    aboard: aboard ?? [],
  };
}

/** A `resync` frame: take the current placement as the baseline. */
export function parseResync(data: string): Readonly<{ seq: number; reason: string }> | null {
  const o = json(data);
  return o !== null && num(o.seq) && str(o.reason) ? { seq: o.seq, reason: o.reason } : null;
}

// ---------------------------------------------------------------------
// Admitting a move.
// ---------------------------------------------------------------------

export const routeKey = (from: string | null, to: string | null): string => `${from ?? '∅'}→${to ?? '∅'}`;

/** A pair the map draws no line for: straight, station to station. */
const straight = (a: Point, b: Point): string => `M${a.x} ${a.y} L${b.x} ${b.y}`;
/** An off-ramp the map draws no line for: 45° away and down, off the map. */
const offRamp = (a: Point): string => `M${a.x} ${a.y} L${a.x + 30} ${a.y + 30} V${OFF_MAP_Y}`;

/** A ping at `station`, merged into one already swelling there inside
 *  the burst window. */
function pinged(pings: ReadonlyArray<Ping>, station: string, at: Point, line: string, now: number, still: boolean): ReadonlyArray<Ping> {
  const open = pings.findIndex((p) => p.station === station && now - p.start < BURST_MS);
  if (open >= 0) return pings.map((p, i) => (i === open ? { ...p, k: p.k + 1 } : p));
  return [...pings, { key: `${station}@${now}`, station, at, start: now, k: 1, line, still }];
}

/** Admit one recorded move at `now` (the page's clock). */
export function admit(
  m: Motion,
  mv: Move,
  g: Geometry,
  o: Readonly<{ now: number; hidden: boolean; reduced: boolean }>,
): Motion {
  // HIDDEN: a count, and nothing drawn — no backlog is replayed on return.
  if (o.hidden) {
    const where = mv.to ?? mv.from;
    return where === null ? m : { ...m, away: { ...m.away, [where]: (m.away[where] ?? 0) + 1 } };
  }
  const from = mv.from === null ? null : g.stationAt(mv.from);
  const to = mv.to === null ? null : g.stationAt(mv.to);
  const line = g.lineOf?.(mv.from, mv.to) ?? 'delivery';
  const land = (ms: Motion): Motion =>
    mv.to !== null && to !== null ? { ...ms, pings: pinged(ms.pings, mv.to, to, line, o.now, o.reduced) } : ms;

  // A station the map cannot place draws nothing — never a dot at the
  // origin.
  if ((mv.from !== null && from === null) || (mv.to !== null && to === null)) return land(m);
  if (o.reduced) return land(m);
  const drawn = g.path(mv.from, mv.to);
  // Filed onto the map: it travels the entry the server serves (car R3),
  // and with none it pings where it lands and nothing travels — a new
  // identity is never drawn as travel it did not make (design §3).
  if (from === null && drawn === null) return land(m);

  const key = routeKey(mv.from, mv.to);
  // A route the map does not draw is one no served route supports: it is
  // drawn anyway, straight (or off the map for an exit), dashed red.
  const fallback = (): string => (from === null ? '' : to === null ? offRamp(from) : straight(from, to));
  const d = drawn?.d ?? fallback();
  const walked = drawn?.walked ?? walk(pathPoints(d));
  if (m.dots.length >= MAX_DOTS) {
    // Past the cap: arrive at once, and count on the section.
    const ghost: Ghost = { key, at: pointAt(walked, walked.length / 2), until: o.now + TRANSIT_MS };
    return land({ ...m, ghosts: [...m.ghosts, ghost] });
  }
  const dot: Dot = {
    id: mv.seq,
    move: mv,
    d,
    walked,
    start: o.now,
    end: o.now + TRANSIT_MS,
    undeclared: mv.declared === false || drawn === null,
    off: mv.to === null,
    handoff: mv.handoff_from !== null && from !== null ? from : null,
    pips: mv.aboard.length,
    line,
  };
  return { ...m, dots: [...m.dots, dot] };
}

// ---------------------------------------------------------------------
// Advancing the clock.
// ---------------------------------------------------------------------

/** The clock at `now`: dots that have landed ping, pings that have
 *  swelled and "+n"s whose crossing is over are dropped. Hands back the
 *  SAME value when nothing changed, so a frame that only moves dots
 *  re-renders nothing. */
export function step(m: Motion, now: number): Motion {
  const landed = m.dots.filter((d) => now >= d.end);
  const pingsLeft = m.pings.filter((p) => now - p.start < PING_MS);
  const ghostsLeft = m.ghosts.filter((gh) => now < gh.until);
  if (landed.length === 0 && pingsLeft.length === m.pings.length && ghostsLeft.length === m.ghosts.length) return m;
  const pings = landed.reduce<ReadonlyArray<Ping>>((ps, d) => {
    if (d.off || d.move.to === null) return ps;
    const end = d.walked.points[d.walked.points.length - 1];
    return end === undefined ? ps : pinged(ps, d.move.to, end, d.line, d.end, false);
  }, pingsLeft);
  return {
    ...m,
    dots: landed.length === 0 ? m.dots : m.dots.filter((d) => now < d.end),
    pings,
    ghosts: ghostsLeft,
  };
}

/** Anything left to draw: the page's frame loop runs only while true. */
export const busy = (m: Motion): boolean => m.dots.length + m.pings.length + m.ghosts.length > 0;

/** Where a dot stands at `now`, and how visible — an off-ramp dot fades
 *  over its last third as it leaves the map. */
export function dotAt(d: Dot, now: number): Readonly<{ x: number; y: number; opacity: number }> {
  const f = Math.min(1, Math.max(0, (now - d.start) / (d.end - d.start)));
  const p = pointAt(d.walked, f * d.walked.length);
  const opacity = d.off ? Math.min(1, Math.max(0, (1 - f) * 3)) : 1;
  return { x: p.x, y: p.y, opacity };
}

/** The "+n" each section shows for arrivals that got no dot. */
export function moreOf(m: Motion): ReadonlyArray<Readonly<{ key: string; at: Point; n: number }>> {
  const by = m.ghosts.reduce<ReadonlyMap<string, Readonly<{ at: Point; n: number }>>>(
    (acc, gh) => new Map(acc).set(gh.key, { at: acc.get(gh.key)?.at ?? gh.at, n: (acc.get(gh.key)?.n ?? 0) + 1 }),
    new Map(),
  );
  return [...by].map(([key, v]) => ({ key, ...v }));
}

/** The tab is visible again: one ping per station for what moved while
 *  it was away, carrying the count — never the moves replayed. Dots that
 *  were in flight are dropped; the positions redraw from the next move. */
export function returned(m: Motion, g: Geometry, now: number): Readonly<{ motion: Motion; away: number }> {
  const entries = Object.entries(m.away);
  const away = entries.reduce((n, [, k]) => n + k, 0);
  const pings = entries.reduce<ReadonlyArray<Ping>>((ps, [station, k]) => {
    const at = g.stationAt(station);
    return at === null ? ps : [...ps, { key: `${station}@${now}`, station, at, start: now, k, line: 'delivery', still: false }];
  }, []);
  return { motion: { dots: [], pings, ghosts: [], away: {} }, away };
}

/** A dot's hover: the packet, the time it moved and the event that moved
 *  it — the three facts that make it a record rather than a picture. */
export function dotTitle(mv: Move): string {
  const t = new Date(mv.at);
  const when = Number.isNaN(t.getTime()) ? mv.at : `${t.toISOString().slice(11, 19)}Z`;
  const route = `${mv.from ?? 'filed'} → ${mv.to ?? 'off the map'}`;
  const train = mv.aboard.length > 0 ? ` · ${mv.aboard.length} aboard` : '';
  const handoff = mv.handoff_from !== null ? ` · handed off from ${mv.handoff_from}` : '';
  const undeclared = mv.declared === false ? ' · on no declared route' : '';
  return `${mv.label} (${mv.kind}) · ${route} · moved ${when} · ${mv.cause_kind} ${mv.cause_event_id}${train}${handoff}${undeclared}`;
}
