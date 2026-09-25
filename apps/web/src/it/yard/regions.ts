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
 *  loudly instead of drawing a new meaning under an old word.
 *
 *  Full (design e765b3fc §4a, car F1): a CAPACITY region at its bound
 *  while its out-route keeps moving — the gates 3/3 with verdicts
 *  landing. A GOOD state, drawn as a solid disk and never as an alarm;
 *  it replaced the amber the gates wore for being used. No border is
 *  ever full (borders.ts keeps the other three). */
export type RegionState = 'clear' | 'full' | 'attention' | 'troubled';
const STATES: ReadonlyArray<RegionState> = ['clear', 'full', 'attention', 'troubled'];

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
  /** How many of the region's OWN members stand at each of its places
   *  — marshalling's stations, the shed's three places (design
   *  62de32ae, the rest of decision 5). The partition is the server's,
   *  so an interior draws the head's count from these rather than a
   *  count of its own. Empty for a region with no places, and on an
   *  older server. */
  places: ReadonlyArray<Place>;
}>;

export type Place = Readonly<{ name: string; count: number }>;

/** A third's STUCK reading (design cf820810), exactly as the server
 *  gives it: stuck and waiting side by side and never summed; a
 *  non-empty `unknown` makes `stuck` a FLOOR. */
export type ThirdStuck = Readonly<{
  stuck: number | null;
  waiting: number | null;
  unknown: ReadonlyArray<string>;
  oldest_hours: number | null;
  regions: ReadonlyArray<string>;
}>;

/** IS THE THIRD TAKING WORK IN FASTER THAN IT LETS WORK OUT? (design
 *  00774ca8, decision 2) — per day, in ONE unit per third, each packet
 *  counted once, the net written by the server. A half whose read
 *  failed is null with the failed read in `why`. */
export type Balance = Readonly<{
  unit: string;
  in_means: string;
  out_means: string;
  in: number | null;
  out: number | null;
  net: number | null;
  in_count: number | null;
  out_count: number | null;
  why: string | null;
}>;

/** One HUD row: a third, its regions (the row's membership), its
 *  balance and its stuck reading — `boss_jobs::thirds::Third`. */
export type Third = Readonly<{
  third: string;
  regions: ReadonlyArray<string>;
  balance: Balance;
  stuck: ThirdStuck;
}>;

/** A failed or unjudged machine with the region whose map draws it. */
export type MachineAt = Readonly<{ region: string; id: string; name: string; state: MachineState; why: string }>;

/** The whole system's machines, counted once by state (decision 3). */
export type MachineSummary = Readonly<{
  running: number;
  idle: number;
  failed: number;
  unknown: number;
  total: number;
  failed_or_unknown: ReadonlyArray<MachineAt>;
}>;

export type Regions = Readonly<{
  window_hours: number;
  regions: ReadonlyArray<Region>;
  now: string;
  /** The HUD's rows, in the server's order. Empty on a server older
   *  than the block — which the HUD draws as unanswered, never as a
   *  balanced system. */
  thirds: ReadonlyArray<Third>;
  /** The machine cell. Null on an older server: not answered, never
   *  "no machines". */
  machines: MachineSummary | null;
  /** THE PLANT (decision 11): machinery that serves every region — the
   *  host runners — drawn as a strip along the map's edge rather than
   *  filed under one region. Empty on an older server, which draws no
   *  strip rather than inventing idle machines. The HUD's machine cell
   *  counts these too, under the region `plant`. */
  plant: ReadonlyArray<Machine>;
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
    places: Array.isArray(o.places) ? o.places.map(parsePlace) : [],
  };
}

function parsePlace(raw: unknown): Place {
  const o = asObject(raw, 'place');
  return { name: String(o.name ?? ''), count: numberOrNull(o.count) ?? 0 };
}

const strings =(v: unknown): ReadonlyArray<string> => (Array.isArray(v) ? v.map(String) : []);

function parseThird(raw: unknown): Third {
  const o = asObject(raw, 'third');
  const b = asObject(o.balance, `third ${String(o.third ?? '?')} balance`);
  const s = asObject(o.stuck, `third ${String(o.third ?? '?')} stuck`);
  return {
    third: String(o.third ?? ''),
    regions: strings(o.regions),
    balance: {
      unit: String(b.unit ?? ''),
      in_means: String(b.in_means ?? ''),
      out_means: String(b.out_means ?? ''),
      in: numberOrNull(b.in),
      out: numberOrNull(b.out),
      net: numberOrNull(b.net),
      in_count: numberOrNull(b.in_count),
      out_count: numberOrNull(b.out_count),
      why: typeof b.why === 'string' ? b.why : null,
    },
    stuck: {
      // A count the payload does not carry is null — unanswered — never 0.
      stuck: numberOrNull(s.stuck),
      waiting: numberOrNull(s.waiting),
      unknown: strings(s.unknown),
      oldest_hours: numberOrNull(s.oldest_hours),
      regions: strings(s.regions),
    },
  };
}

function parseMachineSummary(raw: unknown): MachineSummary | null {
  if (raw === null || raw === undefined) return null;
  const o = asObject(raw, 'machines');
  const count = (k: string): number => {
    const n = numberOrNull(o[k]);
    if (n === null) throw new Error(`machines: expected a count for ${k}`);
    return n;
  };
  return {
    running: count('running'),
    idle: count('idle'),
    failed: count('failed'),
    unknown: count('unknown'),
    total: count('total'),
    failed_or_unknown: Array.isArray(o.failed_or_unknown)
      ? o.failed_or_unknown.map((m) => {
          const x = asObject(m, 'machine');
          return { ...parseMachine(x), region: String(x.region ?? '') };
        })
      : [],
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
    thirds: Array.isArray(o.thirds) ? o.thirds.map(parseThird) : [],
    machines: parseMachineSummary(o.machines),
    plant: Array.isArray(o.plant) ? o.plant.map(parseMachine) : [],
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

const isRegion = (name: string): boolean => (REGION_NAMES as ReadonlyArray<string>).includes(name);

/** Where a link to a region leads — its SELECTION on the Department
 *  Map, `/it?at=<name>`, for every region the server serves: the map
 *  stays on top and the station's detail opens below it (design
 *  e765b3fc, car N1). Until that car it opened the region's own page,
 *  which swapped the map away.
 *  THE ONE DOOR (backlog 594ffe96, 2026-09-25): the world map's old
 *  floor link listed the six yard regions and the three boards and
 *  sent anything else to /it/yard, which the router reads as the
 *  TRACK — so the publish territory opened the track's page — while
 *  the transit map's `regionHref` and the HUD's `machineHref` each
 *  built the path themselves. A name that is not a region (the plant,
 *  or a newer server's eleventh) opens the map with nothing selected:
 *  somewhere, and never another region. */
export function regionHref(name: string): string {
  return isRegion(name) ? `/it?at=${encodeURIComponent(name)}` : '/it';
}

/** The link a SECTION selects itself by (design e765b3fc, car N2) — the
 *  Department Map with the border in the query, `/it?at=dock->track`,
 *  built here beside `regionHref` so the map's links are built in one
 *  place. selection.ts reads it back. */
export function sectionHref(from: string, to: string): string {
  return `/it?at=${encodeURIComponent(`${from}->${to}`)}`;
}

// `floorHref` — a region's floor page, `/it/yard/<name>` — retired with
// the page (design e765b3fc, car N3): the station's panel carries the
// floor itself.

/** The selection a station's floor deck opens on; the track for
 *  anything else, as parseSelection falls back. */
export function floorSelection(region: string): string {
  return YARD_SELECTION[region] ?? 'track';
}

// ---------------------------------------------------------------------
// Words — pure, testable without a DOM.
// ---------------------------------------------------------------------

/** The yard's lamp for a state (`.yard-lamp-dot.<lamp>`). Full is a good
 *  state, so it lights the same lamp as clear. */
export function lampOf(state: RegionState): 'ok' | 'warn' | 'err' {
  return state === 'clear' || state === 'full' ? 'ok' : state === 'attention' ? 'warn' : 'err';
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
