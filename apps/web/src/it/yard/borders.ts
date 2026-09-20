// THE WORLD MAP'S RAILS — the client of `GET /api/yard/borders`
// (design d2154293, decided 2026-09-19; this is car 2). Car 1 drew the
// eight regions as territories with a rail between each pair, and the
// rails carried nothing: "I want to see the interactivity between
// regions at the border" (feedback c3105b2a). A border in this system
// is a real thing — packets cross it at a rate, packets stand at it
// waiting, and some machine moves them — and this module is the client
// half of the read that answers all three.
//
// NOTHING HERE DERIVES A JUDGEMENT. The rate, the queue, each hold's
// reason, the machine's silence and the border's clear/busy/troubled
// state are all the server's (boss_jobs::borders); this module parses
// them ONCE and turns them into words. The only thing it decides is how
// THICK to draw a rail (`densityOf`), which is presentation — the
// number itself is always printed beside it.
//
// AND THE RULE UNDER ALL OF IT: a border whose flow the server could
// not compute renders as UNKNOWN, never as zero. `waiting: null` prints
// "no reading", an unmeasured rate prints "no reading", and an
// unmeasured rate gets its own density band so it cannot be mistaken
// for an empty rail.

import { fetchRemote, type Remote } from '../../data/remote';
import type { RegionState, Trend } from './regions';

/** What moves packets across a border, and what records that it fired
 *  — the server's `MachineKind`, kebab-cased on the wire. */
export type MachineKind = 'cadence' | 'dispatcher-rule' | 'gate-runner' | 'actors';

export type Machine = Readonly<{
  name: string;
  kind: string;
  /** RFC3339, or null when nothing records this machine's firings. */
  last_fired: string | null;
  silent_for_minutes: number | null;
  expected_every_minutes: number | null;
  /** `null` is "cannot tell" — never drawn as "fine". Only a machine
   *  that declares a heartbeat (a cadence rule) can be judged silent. */
  silent: boolean | null;
  why: string;
}>;

/** One packet standing at a border, with the record's own reason. */
export type Hold = Readonly<{ what: string; why: string }>;

export type Border = Readonly<{
  from: string;
  to: string;
  /** What ONE crossing of this border is, in words. */
  crossing: string;
  rate: Trend;
  last_crossed: string | null;
  /** `null` when the read that would answer it failed — never 0. */
  waiting: number | null;
  holds: ReadonlyArray<Hold>;
  machine: Machine;
  state: RegionState;
  why: string;
}>;

export type Borders = Readonly<{
  window_hours: number;
  borders: ReadonlyArray<Border>;
  now: string;
}>;

const STATES: ReadonlyArray<RegionState> = ['clear', 'busy', 'troubled'];

function asObject(raw: unknown, where: string): Record<string, unknown> {
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) {
    throw new Error(`${where}: expected an object`);
  }
  return raw as Record<string, unknown>;
}

const numberOrNull = (v: unknown): number | null =>
  typeof v === 'number' && Number.isFinite(v) ? v : null;
const stringOrNull = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);
const boolOrNull = (v: unknown): boolean | null => (typeof v === 'boolean' ? v : null);

function parseTrend(raw: unknown): Trend {
  const o = asObject(raw, 'rate');
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
  return {
    name: String(o.name ?? ''),
    kind: String(o.kind ?? ''),
    last_fired: stringOrNull(o.last_fired),
    silent_for_minutes: numberOrNull(o.silent_for_minutes),
    expected_every_minutes: numberOrNull(o.expected_every_minutes),
    silent: boolOrNull(o.silent),
    why: String(o.why ?? ''),
  };
}

function parseBorder(raw: unknown): Border {
  const o = asObject(raw, 'border');
  const state = String(o.state ?? '');
  if (!(STATES as ReadonlyArray<string>).includes(state)) {
    throw new Error(`border ${String(o.from)} → ${String(o.to)}: unknown state ${JSON.stringify(state)}`);
  }
  const holds = Array.isArray(o.holds) ? o.holds : [];
  return {
    from: String(o.from ?? ''),
    to: String(o.to ?? ''),
    crossing: String(o.crossing ?? ''),
    rate: parseTrend(o.rate),
    last_crossed: stringOrNull(o.last_crossed),
    waiting: numberOrNull(o.waiting),
    holds: holds.map((h) => {
      const held = asObject(h, 'hold');
      return { what: String(held.what ?? ''), why: String(held.why ?? '') };
    }),
    machine: parseMachine(o.machine),
    state: state as RegionState,
    why: String(o.why ?? ''),
  };
}

/** The whole set, or a throw — a payload without `borders` is a wrong
 *  server, not a map with no rails on it. */
export function parseBorders(raw: unknown): Borders {
  const o = asObject(raw, 'yard borders');
  if (!Array.isArray(o.borders)) throw new Error('yard borders: expected a borders list');
  return {
    window_hours: numberOrNull(o.window_hours) ?? 0,
    borders: o.borders.map(parseBorder),
    now: String(o.now ?? ''),
  };
}

export async function fetchBorders(): Promise<Remote<Borders>> {
  return fetchRemote('/api/yard/borders', parseBorders);
}

// ---------------------------------------------------------------------
// Words — pure, testable without a DOM.
// ---------------------------------------------------------------------

const num = (v: number | null): string => (v === null ? '—' : String(Math.round(v * 10) / 10));

/** `3 vs 5 /day · n=3 / 5`, and "no reading" when neither half was
 *  measured — the same shape a region's trend prints, for the same
 *  reason: a rate nobody measured is not zero. */
export function rateText(rate: Trend): string {
  if (rate.current === null && rate.previous === null) return 'no reading';
  return `${num(rate.current)} vs ${num(rate.previous)} /day · n=${rate.samples} / ${rate.previous_samples}`;
}

/** How many stand at the border — or the admission that it could not be
 *  counted. An unread queue is not an empty one. */
export function waitingText(b: Border): string {
  if (b.waiting === null) return 'waiting: no reading';
  if (b.waiting === 0) return 'nothing waiting';
  return `${b.waiting} waiting`;
}

/** The machine with how long it has been quiet: `SILENT 180m` past its
 *  own declared cadence, `fired 4m ago` inside it, and "no firing
 *  recorded" where nothing records this machine at all. */
export function machineText(m: Machine): string {
  if (m.silent === true) return `${m.name} · SILENT ${m.silent_for_minutes ?? '?'}m`;
  if (m.silent_for_minutes !== null) return `${m.name} · fired ${m.silent_for_minutes}m ago`;
  return `${m.name} · no firing recorded`;
}

/** How thick the rail draws. PRESENTATION ONLY — the rate itself is
 *  always printed beside it. `unknown` is its own band, deliberately
 *  distinct from `none`: an unmeasured rate must not be drawn as an
 *  empty one. */
export type Density = 'unknown' | 'none' | 'light' | 'steady' | 'heavy';

export function densityOf(perDay: number | null): Density {
  if (perDay === null) return 'unknown';
  if (perDay <= 0) return 'none';
  if (perDay < 4) return 'light';
  if (perDay < 20) return 'steady';
  return 'heavy';
}

/** The whole map's activity in one line — the summary the high-level
 *  view bubbles up: how much crossed the world this window, what is
 *  standing, and which rails are troubled (named, because a verdict
 *  must name what it is about). A border the server could not read is
 *  COUNTED AS UNREAD rather than left out of the total, so the line
 *  cannot quietly under-report. */
export function summaryLine(borders: Borders): string {
  const rows = borders.borders;
  const unread = rows.filter((b) => b.rate.current === null || b.waiting === null);
  const crossings = rows.reduce((n, b) => n + b.rate.samples, 0);
  const waiting = rows.reduce((n, b) => n + (b.waiting ?? 0), 0);
  const troubled = rows.filter((b) => b.state === 'troubled').map((b) => `${b.from} → ${b.to}`);
  const parts = [
    `${crossings} ${crossings === 1 ? 'crossing' : 'crossings'} in ${borders.window_hours}h`,
    `${waiting} waiting at the borders`,
  ];
  if (unread.length > 0) {
    parts.push(`${unread.length} ${unread.length === 1 ? 'border unread' : 'borders unread'}`);
  }
  if (troubled.length > 0) parts.push(`troubled: ${troubled.join(', ')}`);
  return parts.join(' · ');
}
