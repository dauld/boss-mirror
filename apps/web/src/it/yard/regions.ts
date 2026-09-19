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
// EVERY CARD IS A DOOR. A region's floor is a surface that already
// exists: the six yard regions open the Train Yard focused on their
// panel (the selection keys yard-floor.ts already speaks), receiving
// and marshalling open their own pages. Nothing is deleted; every
// panel is one click deeper.

import { fetchRemote, type Remote } from '../../data/remote';

// ---------------------------------------------------------------------
// Wire types — the shape of GET /api/yard/regions. Parsed once, below.
// ---------------------------------------------------------------------

/** The eight regions, in map order. The server's
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

/** The two regions whose floor is a page of its own. */
const PAGE_FLOORS: Readonly<Record<string, string>> = {
  receiving: '/it/operate/receiving',
  marshalling: '/it/operate/marshalling',
};

/** Where a card leads. A name this client does not know opens the
 *  yard itself — a door that opens somewhere, never a dead link. */
export function floorHref(name: string): string {
  const page = PAGE_FLOORS[name];
  if (page !== undefined) return page;
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
