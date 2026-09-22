// The IT system map's regions — the client of `GET /api/yard/regions`
// (design 0524fc95, decided 2026-09-19; car 1 is the server read, this
// is car 2, the map page). Eight regions in map order — dock, gates,
// track, shed, arrivals, garage, receiving, marshalling — each a card
// with a count, a clear / busy / troubled state, one sentence of why,
// and a trend (this window against the previous). Every number on a
// card is the server's: this module parses the payload ONCE and turns
// it into words, and nothing here derives a count or a state of its
// own — that is what the map is for, one definition per number, where
// yard.ts and boss orient used to hold two.
//
// EVERY CARD IS A DOOR, and since car 4 of design d2154293 every door
// is a ZOOM: the six yard regions open the Train Yard focused on their
// panel (the selection keys yard-floor.ts already speaks), receiving
// and marshalling open their queue board under the same zoomed world.
// Nothing is deleted; every panel is one click deeper.

import { fetchRemote, type Remote } from '../../data/remote';

// ---------------------------------------------------------------------
// Wire types — the shape of GET /api/yard/regions. Parsed once, below.
// ---------------------------------------------------------------------

/** The ten regions, in map order. The server's
 *  `boss_jobs::regions::REGIONS` is the decision (0524fc95 Q2); this
 *  is the client's copy, pinned equal by regions.test.ts. */
export const REGION_NAMES = [
  'dock',
  'gates',
  'track',
  'shed',
  'arrivals',
  'garage',
  'receiving',
  'marshalling',
  'shop-floor',
  'publish',
] as const;
export type RegionName = (typeof REGION_NAMES)[number];

/** Clear: room to spare and nothing wrong. Busy: at a bound or holding
 *  work that waits on the machine. Troubled: a threshold the yard or an
 *  alarm already enforces, crossed — or a reading that could not be
 *  taken, refused like a failure rather than drawn as clear. */
export type RegionState = 'clear' | 'busy' | 'troubled';
const STATES: ReadonlyArray<RegionState> = ['clear', 'busy', 'troubled'];

/** This window against the previous one. A half nobody measured is
 *  `null` — a rate nobody measured is not zero. */
export type Trend = Readonly<{
  metric: string;
  unit: string;
  current: number | null;
  previous: number | null;
  samples: number;
  previous_samples: number;
}>;

/** WHAT A MACHINE IS DOING (design d2154293, car 5) — the server's
 *  judgement, parsed, never re-derived here. A CLOSED set: an unknown
 *  value throws rather than falling through to a glyph that reads as
 *  calm, exactly as an unknown region state does.
 *
 *  `unknown` is not idle. Idle is a reading — the machine is here and
 *  has no work — and the server states it only where presence is a
 *  fact it holds. Everything else is "cannot tell", and the map draws
 *  the two differently. */
export type MachineState = 'running' | 'idle' | 'failed' | 'unknown';
const MACHINE_STATES: ReadonlyArray<MachineState> = ['running', 'idle', 'failed', 'unknown'];

export type Machine = Readonly<{
  /** Stable within the region, so a glyph keeps its place between
   *  reads: `gate-bay-1`, `conductor`, `station:design-review`. */
  id: string;
  name: string;
  state: MachineState;
  /** What the state was read from — the glyph's whole tooltip. */
  why: string;
}>;

export type Region = Readonly<{
  name: string;
  /** What is here. `null` when the region could not be read — the
   *  state is then `troubled` and `why` names the read. */
  count: number | null;
  /** The bound the count is read against, where the region has one. */
  bound: number | null;
  state: RegionState;
  why: string;
  trend: Trend;
  /** The machinery standing in this region. Empty for a region no
   *  machine of ours works in, and empty on an older server — which
   *  draws no glyphs rather than inventing idle ones. */
  machines: ReadonlyArray<Machine>;
}>;

export type Regions = Readonly<{
  window_hours: number;
  regions: ReadonlyArray<Region>;
  now: string;
}>;

function asObject(raw: unknown, where: string): Record<string, unknown> {
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) {
    throw new Error(`${where}: expected an object`);
  }
  return raw as Record<string, unknown>;
}

const numberOrNull = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null);

function parseTrend(raw: unknown): Trend {
  const o = asObject(raw, 'trend');
  return {
    metric: String(o.metric ?? ''),
    unit: String(o.unit ?? ''),
    current: numberOrNull(o.current),
    previous: numberOrNull(o.previous),
    samples: numberOrNull(o.samples) ?? 0,
    previous_samples: numberOrNull(o.previous_samples) ?? 0,
  };
}

function parseMachine(raw: unknown): Machine {
  const o = asObject(raw, 'machine');
  const state = String(o.state ?? '');
  if (!(MACHINE_STATES as ReadonlyArray<string>).includes(state)) {
    throw new Error(`machine ${String(o.id ?? '?')}: unknown state ${JSON.stringify(state)}`);
  }
  return {
    id: String(o.id ?? ''),
    name: String(o.name ?? ''),
    state: state as MachineState,
    why: String(o.why ?? ''),
  };
}

function parseRegion(raw: unknown): Region {
  const o = asObject(raw, 'region');
  const state = String(o.state ?? '');
  if (!(STATES as ReadonlyArray<string>).includes(state)) {
    throw new Error(`region ${String(o.name ?? '?')}: unknown state ${JSON.stringify(state)}`);
  }
  return {
    name: String(o.name ?? ''),
    count: numberOrNull(o.count),
    bound: numberOrNull(o.bound),
    state: state as RegionState,
    why: String(o.why ?? ''),
    trend: parseTrend(o.trend),
    machines: Array.isArray(o.machines) ? o.machines.map(parseMachine) : [],
  };
}

/** The whole map, or a throw — a payload without `regions` is a wrong
 *  server, not an empty map. */
export function parseRegions(raw: unknown): Regions {
  const o = asObject(raw, 'yard regions');
  if (!Array.isArray(o.regions)) throw new Error('yard regions: expected a regions list');
  return {
    window_hours: numberOrNull(o.window_hours) ?? 0,
    regions: o.regions.map(parseRegion),
    now: String(o.now ?? ''),
  };
}

export async function fetchRegions(): Promise<Remote<Regions>> {
  return fetchRemote('/api/yard/regions', parseRegions);
}

// ---------------------------------------------------------------------
// The doors — where each card leads.
// ---------------------------------------------------------------------

/** The yard panel each yard region's floor opens on: the selection
 *  keys yard-floor.ts `parseSelection` already speaks, so the floor IS
 *  the existing panel. Gates open the approach — the bays, the queue
 *  for a bay and the branches publishing towards one, on one panel. */
const YARD_SELECTION: Readonly<Record<string, string>> = {
  dock: 'dock',
  gates: 'approach',
  track: 'track',
  shed: 'inspection-shed',
  arrivals: 'arrivals',
  garage: 'garage',
};

/** The regions whose floor is a BOARD rather than the yard's own
 *  rolling stock. Since car 4 of design d2154293 they are zooms like
 *  every other territory: the board mounts UNDER the zoomed world, and
 *  /it/operate/receiving and /it/operate/marshalling — the pages they
 *  used to be — resolve to the same route. The shop floor joined them
 *  on backlog 94c6ffd0: its board is the crew board, which was the
 *  floor before the region existed. */
const BOARD_FLOORS: ReadonlyArray<string> = ['receiving', 'marshalling', 'shop-floor'];

/** Where a card leads. A name this client does not know opens the
 *  yard itself — a door that opens somewhere, never a dead link. */
export function floorHref(name: string): string {
  if (BOARD_FLOORS.includes(name)) return `/it/yard/${name}`;
  return name in YARD_SELECTION ? `/it/yard/${name}` : '/it/yard';
}

/** The selection a `/it/yard/<region>` floor opens the yard on; the
 *  track for anything else, as parseSelection falls back. */
export function floorSelection(region: string): string {
  return YARD_SELECTION[region] ?? 'track';
}

// ---------------------------------------------------------------------
// Words — pure, testable without a DOM.
// ---------------------------------------------------------------------

/** The yard's lamp for a state (`.yard-lamp-dot.<lamp>`). */
export function lampOf(state: RegionState): 'ok' | 'warn' | 'err' {
  return state === 'clear' ? 'ok' : state === 'busy' ? 'warn' : 'err';
}

/** What is here, over its bound when it has one; "no reading" for a
 *  count the server could not take — never 0. */
export function countText(r: Region): string {
  if (r.count === null) return 'no reading';
  return r.bound === null ? String(r.count) : `${r.count} / ${r.bound}`;
}

/** A number as the card prints it: one decimal at most, and no
 *  trailing `.0`. */
const num = (v: number | null): string => (v === null ? '—' : String(Math.round(v * 10) / 10));

/** This window against the previous, in the unit, with how many
 *  observations each half rests on (a median of one is an anecdote).
 *  Two unmeasured halves are "no reading", never "0 vs 0". */
export function trendText(t: Trend): string {
  if (t.current === null && t.previous === null) return 'no reading';
  return `${num(t.current)} vs ${num(t.previous)} ${t.unit} · n=${t.samples} / ${t.previous_samples}`;
}
