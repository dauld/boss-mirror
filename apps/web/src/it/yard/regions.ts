// The IT system map's regions — the client of `GET /api/yard/regions`
// (design 0524fc95, decided 2026-09-19; car 1 is the server read, this
// is car 2, the map page). Eight regions in map order — dock, gates,
// track, shed, arrivals, garage, receiving, marshalling — each a card
// with a count, a clear / attention / troubled state, one sentence of why,
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

/** ONE VOCABULARY, regions and borders alike (design 62de32ae,
 *  decision 1; the server's `boss_jobs::region_states` is the
 *  definition). Clear: flowing within its declared bounds — busy and
 *  healthy included, so a dock with a train due is clear. Attention: a
 *  declared band crossed. Troubled: ours and not moving, or a reading
 *  that could not be taken, refused like a failure rather than drawn as
 *  clear. `busy` is gone rather than aliased: it meant three things on
 *  six of ten regions, and a server still sending it fails this parse
 *  loudly instead of drawing a new meaning under an old word. */
export type RegionState = 'clear' | 'attention' | 'troubled';
const STATES: ReadonlyArray<RegionState> = ['clear', 'attention', 'troubled'];

/** What a bound IS (decision 5): the most a place holds, or the depth at
 *  which something is due. "DOCK 6 / 1" read as six cars in a space for
 *  one; it was six cars against a boarding threshold of one. */
export type BoundKind = 'capacity' | 'threshold';
const BOUND_KINDS: ReadonlyArray<BoundKind> = ['capacity', 'threshold'];

/** One number of a region's KPI, with its unit (decisions 5 and 9).
 *  `text` is the server's sentence — the map prints it, never builds
 *  one; `value` is null where nothing could be measured. */
export type Measure = Readonly<{ name: string; value: number | null; unit: string; text: string }>;

/** THE BAND THAT DECIDED A NON-CLEAR STATE (decisions 1 and 2): the
 *  reading against the declared line ("oldest 5d > the 3-day triage
 *  band"), the period its condition had to hold, and how long the
 *  record says it has held — `held` is the server's own "16m", so the
 *  map and `boss orient` say "troubled for 16m" in one voice. */
export type Decided = Readonly<{
  id: string;
  reads: string;
  hold_minutes: number;
  since: string | null;
  held_minutes: number | null;
  held: string | null;
}>;

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
  /** What the bound is; null with the bound, and on an older server —
   *  which `countText` then reads as the capacity it always meant. */
  bound_kind: BoundKind | null;
  /** What the count counts: `cars parked`. Empty on an older server. */
  unit: string;
  state: RegionState;
  why: string;
  /** The band that decided a non-clear state; null when clear. */
  band: Decided | null;
  trend: Trend;
  /** The region's KPI, primary first. Empty on an older server. */
  kpi: ReadonlyArray<Measure>;
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

function parseMeasure(raw: unknown): Measure {
  const o = asObject(raw, 'measure');
  return {
    name: String(o.name ?? ''),
    value: numberOrNull(o.value),
    unit: String(o.unit ?? ''),
    text: String(o.text ?? ''),
  };
}

function parseDecided(raw: unknown): Decided | null {
  if (raw === null || raw === undefined) return null;
  const o = asObject(raw, 'band');
  return {
    id: String(o.id ?? ''),
    reads: String(o.reads ?? ''),
    hold_minutes: numberOrNull(o.hold_minutes) ?? 0,
    since: typeof o.since === 'string' ? o.since : null,
    held_minutes: numberOrNull(o.held_minutes),
    held: typeof o.held === 'string' ? o.held : null,
  };
}

function parseRegion(raw: unknown): Region {
  const o = asObject(raw, 'region');
  const name = String(o.name ?? '?');
  const state = String(o.state ?? '');
  if (!(STATES as ReadonlyArray<string>).includes(state)) {
    throw new Error(`region ${name}: unknown state ${JSON.stringify(state)}`);
  }
  // Absent is an older server; present and unknown is a newer one whose
  // bound this client cannot read — refused, like an unknown state.
  const kind = o.bound_kind;
  if (kind !== undefined && kind !== null && !(BOUND_KINDS as ReadonlyArray<unknown>).includes(kind)) {
    throw new Error(`region ${name}: unknown bound kind ${JSON.stringify(kind)}`);
  }
  return {
    name: String(o.name ?? ''),
    count: numberOrNull(o.count),
    bound: numberOrNull(o.bound),
    bound_kind: (kind ?? null) as BoundKind | null,
    unit: String(o.unit ?? ''),
    state: state as RegionState,
    why: String(o.why ?? ''),
    band: parseDecided(o.band),
    trend: parseTrend(o.trend),
    kpi: Array.isArray(o.kpi) ? o.kpi.map(parseMeasure) : [],
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
  return state === 'clear' ? 'ok' : state === 'attention' ? 'warn' : 'err';
}

/** What is here, in its unit, against its bound — "3 / 3 bays in use"
 *  for a capacity, "6 cars parked · threshold 1" for a threshold, so a
 *  trigger never reads as room (decision 5). "no reading" for a count
 *  the server could not take — never 0. */
export function countText(r: Region): string {
  if (r.count === null) return 'no reading';
  const unit = r.unit === '' ? '' : ` ${r.unit}`;
  if (r.bound === null) return `${r.count}${unit}`;
  return r.bound_kind === 'threshold'
    ? `${r.count}${unit} · threshold ${r.bound}`
    : `${r.count} / ${r.bound}${unit}`;
}

/** The count as a territory has room for it: the number against its
 *  bound, with the threshold still named — the unit rides the KPI line
 *  and the title. */
export function compactCountText(r: Region): string {
  if (r.count === null) return 'no reading';
  if (r.bound === null) return String(r.count);
  return r.bound_kind === 'threshold' ? `${r.count} · threshold ${r.bound}` : `${r.count} / ${r.bound}`;
}

/** The state with how long the record says it has held — "troubled
 *  for 16m" (decision 2). A clear state, or one whose onset the record
 *  does not hold, is the bare word. */
export function stateText(r: Region | undefined): string {
  if (r === undefined) return 'troubled';
  return r.band?.held ? `${r.state} for ${r.band.held}` : r.state;
}

/** The band that decided a non-clear state, read against its number —
 *  "oldest 5d > the 3-day triage band"; null when clear. */
export const bandText = (r: Region | undefined): string | null => r?.band?.reads ?? null;

/** The region's KPI as one line, each measure in the server's words. */
export const kpiText = (r: Region): string => r.kpi.map((m) => m.text).join(' · ');

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
