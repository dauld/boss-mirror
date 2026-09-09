// The yard floor — every change as a wagon you can follow.
//
// David approved the rail-map redesign of the Train Yard (2026-09-08)
// with two ideas, both required: a car is a persistent token that
// keeps its identity through every station and SLIDES when its state
// changes, and the page looks like a rail yard — approach siding, gate
// sheds up a branch line, the dock and garage sidings, one mainline
// with semaphore signals, an arrivals yard. This module is the PURE
// half: from the two read models the page already polls (the server's
// `YardStatus` and the client board's `YardState`) it derives a `Scene`
// — where every wagon stands, which bay is busy, how far along the
// line each locomotive is, the order the departure board reads, and
// what each machine's label says. The Svelte map only draws it.
//
// Identity is the whole point. A wagon's id is the car's packet id
// wherever the page can name the car behind a branch (`YardState.cars`
// carries every open car), so the same keyed node moves from the gate
// bay to the dock to the train; only a branch with no car falls back
// to the gate-run or publish-request packet that names it. One branch,
// one wagon: a parked car whose re-gate is running stands in the bay,
// not on the dock and in the bay at once.
//
// Nothing here is invented. Every status line is a fact one of the two
// read models already states; the one number without a server reading
// (how long a gate usually takes) is a named constant, not a policy
// this page pretends to know.

import { formatDate } from '@boss/web-kit/ui/date';
import type { ClusterMachine, RunnerMachine } from './yard-machines';
export type { ClusterMachine, RunnerMachine } from './yard-machines';
import { failedChecks, stampAt, troubleLabel, type CarRow, type TrainRow, type WithSteps, type YardState } from './yard';
import {
  blockLabel,
  clockText,
  elapsedText,
  gateSlots,
  journeyText,
  queueLabel,
  type ConductorHealth,
  type QueuedGate,
  type YardStatus,
} from './yard-status';

// ---------------------------------------------------------------------
// The scene.
// ---------------------------------------------------------------------

/** The mainline's six stages, left to right. A locomotive's `stage`
 *  indexes this; a signal stands at each. */
export const STAGES = ['PR', 'CI', 'merge', 'deploy', 'converge', 'arrived'] as const;

export type Station =
  | 'approach'
  /** Waiting for a gate bay — the queue lane feeding the sheds. */
  | 'gate-queue'
  | 'gate'
  | 'limbo'
  | 'dock'
  | 'garage'
  | 'train'
  | 'arrivals';
export type Tone = 'ok' | 'warn' | 'red' | 'static';
export type Lamp = 'ok' | 'working' | 'warn' | 'err' | 'off';
export type Signal = 'ok' | 'now' | 'err' | 'off';

export type Wagon = Readonly<{
  /** The packet id — the car's wherever one is known (see the module
   *  comment), else the gate-run / publish-request, else `garage:<branch>`
   *  for a server-garaged branch no packet in the window names. Stable
   *  across polls, which is what makes a station change a slide. */
  id: string;
  /** The short name painted on the wagon, ≤ 11 chars, from the branch. */
  tag: string;
  title: string;
  branch: string;
  head: string | null;
  /** Protocol kind, for the packet hue the rest of the app uses. */
  kind: string;
  sim: boolean;
  station: Station;
  /** Position within the station: the bay index, the dock slot, the
   *  place in the consist, the stack position in the arrivals yard. */
  slot: number;
  trainId: string | null;
  tone: Tone;
  lamp: Lamp;
  /** One line: what the wagon is doing, in the read model's words. */
  status: string;
  /** When it reached this station — an RFC3339 instant or a bare date,
   *  whichever the record carries; null when it carries none. */
  since: string | null;
}>;

export type Loco = Readonly<{
  id: string;
  /** The PR number, read off the forge URL; null before the PR opens. */
  n: number | null;
  title: string;
  /** Index into `STAGES`. */
  stage: number;
  /** 0–1 within the stage, from the board's ETA leg where it has a
   *  reading (CI, deploying); 0 where nothing honest positions it. */
  progress: number;
  /** Why it is not moving — the server's block first, else the board's
   *  trouble badge; null when it is simply moving. */
  blocked: string | null;
  /** Wagon ids aboard, in consist order. */
  cars: readonly string[];
}>;

export type Bay = Readonly<{
  index: number;
  busy: boolean;
  branch: string | null;
  packetId: string | null;
  /** The wagon standing in this bay (the car's id when known). */
  wagonId: string | null;
  /** Its nameplate — the same tag the wagon wears, unique on the floor. */
  tag: string | null;
  since: string | null;
  elapsed: string | null;
  stale: boolean;
  /** Elapsed over the runner's usual duration, 0–1 — the shed's bar. */
  progress: number;
}>;

export type BoardRow = Readonly<{
  id: string;
  where: string;
  landed: boolean;
}>;

export type ConductorMachine = Readonly<{
  lamp: Lamp;
  silent: boolean;
  lastSeen: string | null;
  /** last_seen + the conductor's own declared heartbeat, RFC3339. */
  nextTick: string | null;
  label: string;
}>;

/** What the page reads from OUTSIDE the two yard read models: the
 *  deploy-runner shed (the converge ops-requests) and the cluster tower
 *  (/api/jobs/health) — both derived in yard-machines.ts and fed in.
 *  `NO_FEEDS` before the first read: the map draws both dark and says
 *  "no reading", never idle. */
export type Feeds = Readonly<{ runner: RunnerMachine; cluster: ClusterMachine }>;
export const NO_FEEDS: Feeds = { runner: { kind: 'unknown' }, cluster: { kind: 'unknown' } };

export type Machines = Readonly<{
  approach: Readonly<{ label: string; stranded: number; publishing: number; held: number }>;
  /** The queue lane feeding the gate bays. Neutral by construction: a
   *  queue is the system working to its bound, not an alarm. */
  queue: Readonly<{ label: string; count: number }>;
  dock: Readonly<{
    label: string;
    parked: number;
    held: string | null;
    next: string | null;
    cooldownMinutes: number | null;
    lamp: Lamp;
  }>;
  garage: Readonly<{ label: string; count: number }>;
  arrivals: Readonly<{ label: string; landed: number }>;
  conductor: ConductorMachine;
  runner: RunnerMachine;
  cluster: ClusterMachine;
}>;

export type Scene = Readonly<{
  /** The server's clock when the status served, else the local one. */
  now: string;
  wagons: readonly Wagon[];
  locos: readonly Loco[];
  bays: readonly Bay[];
  /** One lamp per stage, lit by the lead locomotive. */
  signals: readonly Signal[];
  boardRows: readonly BoardRow[];
  machines: Machines;
}>;

// ---------------------------------------------------------------------
// Small readers.
// ---------------------------------------------------------------------

const TAG_MAX = 11;

/** Words that carry no identity on a wagon's nameplate. */
const FILLER = new Set([
  'the', 'a', 'an', 'of', 'its', 'is', 'not', 'does', 'do', 'on', 'to', 'in', 'and',
  'has', 'have', 'over', 'with', 'for', 'by', 'at', 'that', 'this', 'it',
]);

/** The short name painted on a wagon: the branch minus its `feat/` /
 *  `fix/` / `docs/` prefix and its filler words, then the first word
 *  plus each following word that still fits in eleven characters, at
 *  most three words. A first word longer than eleven is cut. Pure and
 *  deterministic: the same branch always paints the same nameplate. */
export function wagonTag(branch: string): string {
  const words = tagWords(branch);
  const first = words[0];
  if (first === undefined) return '';
  let tag = first.slice(0, TAG_MAX);
  let used = 1;
  for (const w of words.slice(1)) {
    if (used >= 3) break;
    const next = `${tag}-${w}`;
    if (next.length > TAG_MAX) continue;
    tag = next;
    used += 1;
  }
  return tag;
}

/** The words of a branch that carry identity: the prefix and the
 *  filler dropped, or every word when nothing else is left. */
function tagWords(branch: string): readonly string[] {
  const bare = branch.replace(/^[^/]+\//, '');
  const all = bare.split(/[-_./]+/).filter(w => w !== '');
  const kept = all.filter(w => !FILLER.has(w.toLowerCase()));
  return kept.length > 0 ? kept : all;
}

/** The nameplates a branch can wear when its base tag is already
 *  taken, in the order they are tried: the first word cut to make room
 *  for each later word in turn — `conduc-says`, `con-honours` — so two
 *  branches that share a first word part on their second. A later word
 *  too long to leave three letters of the first is itself cut. */
function tagCandidates(branch: string): readonly string[] {
  const words = tagWords(branch);
  const first = words[0];
  if (first === undefined) return [''];
  const base = wagonTag(branch);
  const rest = words.slice(1).map(w => {
    const room = TAG_MAX - 1 - w.length;
    return room >= 3 ? `${first.slice(0, room)}-${w}` : `${first.slice(0, 3)}-${w.slice(0, TAG_MAX - 4)}`;
  });
  return [base, ...rest.filter(c => c !== base)];
}

/** One nameplate per branch, unique across the floor. On 2026-09-08
 *  `feat/the-conductor-says-why-it-is-not-boarding` and
 *  `feat/the-conductor-honours-a-cancel-request` both painted
 *  `conductor`. A base tag two branches share is replaced, for EVERY
 *  branch in the clash, by that branch's first free candidate, taken
 *  in branch order — so the same floor always paints the same names,
 *  and no clashing branch keeps the ambiguous base. Uncontested bases
 *  are settled first so a replacement never takes one. */
export function uniqueTags(branches: readonly string[]): ReadonlyMap<string, string> {
  const byBase = [...new Set(branches)].sort().reduce<Map<string, string[]>>((m, b) => {
    const base = wagonTag(b);
    return m.set(base, [...(m.get(base) ?? []), b]);
  }, new Map());
  const taken = new Set<string>();
  const out = new Map<string, string>();
  for (const [base, bs] of byBase) {
    const only = bs.length === 1 ? bs[0] : undefined;
    if (only !== undefined) {
      out.set(only, base);
      taken.add(base);
    }
  }
  for (const [base, bs] of byBase) {
    if (bs.length === 1) continue;
    for (const b of bs) {
      const candidates = tagCandidates(b);
      const pick = candidates.slice(1).find(c => !taken.has(c)) ?? candidates.find(c => !taken.has(c)) ?? base;
      out.set(b, pick);
      taken.add(pick);
    }
  }
  return out;
}

/** The PR number at the end of a forge URL (`/pulls/259`, `/pull/12`). */
export function prNumber(url: string | null | undefined): number | null {
  if (!url) return null;
  const m = /\/pulls?\/(\d+)\/?$/.exec(url);
  return m ? Number(m[1]) : null;
}

/** The conductor titles a train with its boarding minute in UTC —
 *  `PR train 2026-09-07 23:37`. Read as an instant; null when the
 *  title carries none. */
export function boardedAtFromTitle(title: string): string | null {
  const m = /(\d{4}-\d{2}-\d{2}) (\d{2}:\d{2})/.exec(title);
  return m ? `${m[1]}T${m[2]}:00.000Z` : null;
}

const isInstant = (s: string): boolean => s.includes('T');

/** A "since" for the board: an instant reads as elapsed (`15m`, `1.2h`),
 *  a bare date as the date, nothing as a dash — never an elapsed time
 *  invented from a day-granular stamp. */
export function sinceText(since: string | null | undefined, nowMs: number): string {
  if (!since) return '—';
  if (isInstant(since)) return elapsedText(since, nowMs) ?? '—';
  return formatDate(since);
}

/** How long a full gate usually takes — the runner's measured ~12 min
 *  (fmt · clippy · locomotive · web · test). The shed's progress bar
 *  runs over this; it is a drawing scale, not a threshold. The server's
 *  `stale` flag is the alarm. */
export const GATE_USUAL_MINUTES = 12;

/** An operator's hold marker often opens with its own "held:" — the
 *  wagon already says held, so the reason is what follows it. */
const holdReason = (hold: string | null): string =>
  (hold ?? 'no reason recorded').replace(/^held:\s*/i, '');

const shortSha = (v: unknown): string | null =>
  typeof v === 'string' && /^[0-9a-f]{7,40}$/i.test(v) ? v.slice(0, 7) : null;

/** How many landed wagons the map stacks before a "+N more" plate. On
 *  2026-09-08 eleven landed cars in two columns outgrew the map. */
export const ARRIVALS_DRAWN = 6;

/** The wagons the map draws: everything in flight, and the newest
 *  `ARRIVALS_DRAWN` landed; `hidden` is how many landed wagons the
 *  plate stands for. The departure board lists them all. */
export function drawnWagons(wagons: readonly Wagon[]): Readonly<{ drawn: readonly Wagon[]; hidden: number }> {
  const drawn = wagons.filter(w => w.station !== 'arrivals' || w.slot < ARRIVALS_DRAWN);
  return { drawn, hidden: wagons.length - drawn.length };
}

// ---------------------------------------------------------------------
// The selection — one string the map, the board and the alerts speak.
// ---------------------------------------------------------------------

export type Selection =
  | Readonly<{ kind: 'car'; id: string }>
  | Readonly<{ kind: 'train'; id: string }>
  | Readonly<{ kind: 'bay'; index: number }>
  | Readonly<{ kind: 'track' }>
  | Readonly<{ kind: 'dock' }>
  | Readonly<{ kind: 'garage' }>
  | Readonly<{ kind: 'approach' }>
  | Readonly<{ kind: 'gate-queue' }>
  | Readonly<{ kind: 'arrivals' }>
  | Readonly<{ kind: 'conductor' }>
  | Readonly<{ kind: 'runner' }>
  | Readonly<{ kind: 'cluster' }>;

const PLAIN_SELECTIONS = [
  'track', 'dock', 'garage', 'approach', 'gate-queue', 'arrivals', 'conductor', 'runner', 'cluster',
] as const;
type PlainSelection = (typeof PLAIN_SELECTIONS)[number];

/** `car:<id>` / `train:<id>` / `bay:<n>` / a machine name → the
 *  selection. Anything else falls back to the track: a key from a
 *  future map must not throw the page. */
export function parseSelection(key: string): Selection {
  if (key.startsWith('car:')) return { kind: 'car', id: key.slice(4) };
  if (key.startsWith('train:')) return { kind: 'train', id: key.slice(6) };
  if (key.startsWith('bay:')) {
    const n = Number(key.slice(4));
    return Number.isInteger(n) && n >= 0 ? { kind: 'bay', index: n } : { kind: 'track' };
  }
  return (PLAIN_SELECTIONS as readonly string[]).includes(key)
    ? { kind: key as PlainSelection }
    : { kind: 'track' };
}

// ---------------------------------------------------------------------
// A packet's journey — its completed steps, with the gate receipt.
// ---------------------------------------------------------------------

export type JourneyStop = Readonly<{
  lamp: Lamp;
  what: string;
  when: string | null;
  note: string | null;
}>;

/** The receipt a gate step carries, as one line: verdict · head · what failed. */
function receiptNote(md: Record<string, unknown> | null | undefined): Readonly<{
  note: string;
  verdict: string;
}> | null {
  const raw = md?.receipt;
  if (typeof raw !== 'string') return null;
  try {
    const r = JSON.parse(raw) as { verdict?: unknown; head?: unknown };
    const verdict = typeof r.verdict === 'string' ? r.verdict : 'unknown';
    const head = shortSha(r.head);
    const fails = failedChecks(r);
    const parts = [verdict, ...(head ? [head] : []), ...(fails.length > 0 ? [fails.join(', ')] : [])];
    return { note: parts.join(' · '), verdict };
  } catch {
    return null;
  }
}

/** Every step the packet has completed or is at, as stops: a completed
 *  step with its stamp, the step under way pulsing, the gate step
 *  carrying its receipt (red when the receipt was). Pending and skipped
 *  steps are not stops — they have not happened. Data-keyed: nothing
 *  here knows a protocol, only step statuses and a `receipt` key. */
export function journeyStops(job: WithSteps | null): readonly JourneyStop[] {
  if (!job) return [];
  return (job.steps ?? []).flatMap((s): JourneyStop[] => {
    if (s.status === 'completed') {
      const receipt = receiptNote(s.metadata);
      const lamp: Lamp =
        receipt === null ? 'ok' : receipt.verdict === 'green' ? 'ok' : receipt.verdict === 'lost' ? 'warn' : 'err';
      return [{ lamp, what: s.title, when: stampAt(s), note: receipt?.note ?? null }];
    }
    if (s.status === 'active' || s.status === 'ready') {
      return [{ lamp: 'working', what: s.title, when: null, note: null }];
    }
    return [];
  });
}

// ---------------------------------------------------------------------
// The scene, derived.
// ---------------------------------------------------------------------

const STATION_RANK: Readonly<Record<Station, number>> = {
  approach: 0,
  'gate-queue': 1,
  gate: 2,
  limbo: 3,
  dock: 4,
  garage: 5,
  train: 6,
  arrivals: 7,
};

function stageOf(t: TrainRow): number {
  switch (t.status) {
    case 'BOARDING':
      return 0;
    case 'BOARDED':
      return t.lamp === 'green' ? 2 : 1;
    case 'DEPARTED':
      return 3;
    case 'CONVERGING':
      return 4;
    case 'ARRIVED':
      return 5;
  }
}

/** The ETA leg positions the locomotive only where the leg IS the
 *  stage: CI (the board→merge leg under way) and deploying (the
 *  merge→deploy leg). Awaiting merge sits at the merge signal — a green
 *  lamp says nothing about how close the merge is — and converging has
 *  an elapsed but no median, so it sits at its signal too. */
function progressOf(t: TrainRow, stage: number): number {
  if (t.eta.kind !== 'eta') return 0;
  return stage === 1 || stage === 3 ? t.eta.progress : 0;
}

const DAY_MS = 24 * 60 * 60 * 1000;

/** The lane's own line: how many wait, and what the FRONT of the line
 *  expects — the number an operator actually wants, since everyone
 *  behind it waits at least that long. "clear" when nobody waits; the
 *  count alone when the server measured no gate to estimate from. */
function queueLaneLabel(queue: readonly QueuedGate[]): string {
  const front = queue[0];
  if (!front) return 'clear';
  const waiting = `${queue.length} waiting`;
  if (front.estimated_wait_seconds === null) return waiting;
  return front.estimated_wait_seconds === 0
    ? `${waiting} · a bay is free now`
    : `${waiting} · next ~${journeyText(front.estimated_wait_seconds)}`;
}

function conductorMachine(c: ConductorHealth | null): ConductorMachine {
  if (!c) return { lamp: 'off', silent: false, lastSeen: null, nextTick: null, label: 'no reading' };
  if (c.silent) {
    const since =
      c.silent_for_minutes !== null
        ? `${c.silent_for_minutes}m since it last fired`
        : 'past its declared heartbeat';
    return { lamp: 'err', silent: true, lastSeen: c.last_seen, nextTick: null, label: `SILENT · ${since}` };
  }
  const lastMs = c.last_seen !== null ? Date.parse(c.last_seen) : Number.NaN;
  const nextTick =
    !Number.isNaN(lastMs) && c.expected_every_minutes !== null
      ? new Date(lastMs + c.expected_every_minutes * 60_000).toISOString()
      : null;
  const clock = clockText(nextTick);
  const label =
    clock !== null
      ? `next tick ≤ ${clock}`
      : c.silent_for_minutes !== null
        ? `last seen ${c.silent_for_minutes}m ago`
        : 'no firing on record';
  return { lamp: c.last_seen !== null ? 'ok' : 'warn', silent: false, lastSeen: c.last_seen, nextTick, label };
}

/** The floor, from the two read models the page already holds. Pure:
 *  same inputs, same scene. `status` is null when the status endpoint
 *  has not served — then there are no bays (unknown, not zero) and the
 *  machines say so. */
export function scene(yard: YardState, status: YardStatus | null, nowMs: number, feeds: Feeds = NO_FEEDS): Scene {
  const carByBranch = new Map(yard.cars.map(c => [c.branch, c]));
  // Every branch that can stand on the floor names its wagon once.
  const tags = uniqueTags([
    ...yard.cars.map(c => c.branch),
    ...yard.dock.map(c => c.branch),
    ...yard.inFlight.flatMap(t => t.cars.map(c => c.branch)),
    ...yard.arrivals.flatMap(t => t.cars.map(c => c.branch)),
    ...(status?.gates.active ?? []).map(g => g.branch),
    ...(status?.gates.queued ?? []).map(g => g.branch),
    ...yard.approach.map(r => r.branch),
    ...(status?.garage ?? []).map(g => g.branch),
  ]);
  const tagOf = (branch: string): string => tags.get(branch) ?? wagonTag(branch);
  const claimedIds = new Set<string>();
  const claimedBranches = new Set<string>();
  const wagons: Wagon[] = [];
  const place = (w: Wagon): void => {
    wagons.push(w);
    claimedIds.add(w.id);
    if (w.branch !== '') claimedBranches.add(w.branch);
  };
  const base = (c: CarRow) => ({
    tag: tagOf(c.branch),
    title: c.title,
    branch: c.branch,
    head: c.head,
    kind: c.kind,
    sim: c.sim,
  });

  // THE TRACK — open trains as locomotives, their cars coupled behind.
  const serverTrain = new Map((status?.trains ?? []).map(t => [t.id, t]));
  const locos: Loco[] = yard.inFlight.map(t => {
    const st = serverTrain.get(t.id) ?? null;
    const blocked = blockLabel(st?.block ?? null) ?? (t.trouble ? troubleLabel(t.trouble) : null);
    const stage = stageOf(t);
    return {
      id: t.id,
      n: prNumber(t.prUrl),
      title: t.title,
      stage,
      progress: progressOf(t, stage),
      blocked,
      cars: t.cars.map(c => c.id),
    };
  });
  const locoName = (l: Loco): string => (l.n !== null ? `#${l.n}` : l.title);
  const locoById = new Map(locos.map(l => [l.id, l]));
  yard.inFlight.forEach(t => {
    const l = locoById.get(t.id);
    if (!l) return;
    // The server's boarded_at when it sends one, else the boarding
    // minute the conductor wrote into the title.
    const boardedAt = serverTrain.get(t.id)?.boarded_at ?? boardedAtFromTitle(t.title);
    t.cars.forEach((c, i) => {
      if (claimedIds.has(c.id)) return;
      place({
        id: c.id,
        ...base(c),
        station: 'train',
        slot: i,
        trainId: t.id,
        tone: l.blocked ? 'red' : 'ok',
        lamp: l.blocked ? 'err' : 'working',
        status: l.blocked
          ? `aboard ${locoName(l)} · blocked at ${STAGES[l.stage]}`
          : `aboard ${locoName(l)} · ${STAGES[l.stage]}`,
        since: boardedAt,
      });
    });
  });

  // THE ARRIVALS YARD — landed cars stack newest first.
  const landed = [...yard.arrivals].sort((a, b) => b.arrivedAt.ms - a.arrivedAt.ms);
  let landedSlot = 0;
  let landedRecently = 0;
  landed.forEach(t => {
    const recent = t.arrivedAt.ms > 0 && nowMs - t.arrivedAt.ms <= DAY_MS;
    t.cars.forEach(c => {
      if (claimedIds.has(c.id)) return;
      if (recent) landedRecently += 1;
      place({
        id: c.id,
        ...base(c),
        station: 'arrivals',
        slot: landedSlot,
        trainId: t.id,
        tone: 'ok',
        lamp: 'ok',
        status: t.mergeRef ? `landed in ${t.mergeRef} · arrived` : 'landed · arrived',
        since: t.arrivedAt.at !== '' ? t.arrivedAt.at : null,
      });
      landedSlot += 1;
    });
  });

  // THE GATE BAYS — the server's capacity, each empty or working a
  // branch. The wagon in a bay is the branch's car when the page can
  // name one; a re-gate of a parked car therefore stands HERE, once.
  const slots = status ? gateSlots(status.gates) : [];
  const bays: Bay[] = slots.map((s, i) => {
    if (s.kind !== 'occupied') {
      return { index: i, busy: false, branch: null, packetId: null, wagonId: null, tag: null, since: null, elapsed: null, stale: false, progress: 0 };
    }
    const g = s.gate;
    const car = carByBranch.get(g.branch);
    const id = car && !claimedIds.has(car.id) ? car.id : g.packet_id;
    const elapsed = sinceText(g.since, nowMs);
    const startedMs = isInstant(g.since) ? Date.parse(g.since) : Number.NaN;
    const progress = Number.isNaN(startedMs)
      ? 0
      : Math.min(Math.max(nowMs - startedMs, 0) / (GATE_USUAL_MINUTES * 60_000), 1);
    if (!claimedBranches.has(g.branch)) {
      place({
        id,
        tag: tagOf(g.branch),
        title: car?.title ?? g.branch,
        branch: g.branch,
        head: car?.head ?? null,
        kind: car?.kind ?? 'gate-run',
        sim: car?.sim ?? false,
        station: 'gate',
        slot: i,
        trainId: null,
        tone: g.stale ? 'warn' : 'ok',
        lamp: g.stale ? 'warn' : 'working',
        status: g.stale
          ? `gating · ${elapsed} · STALE — past the runner's usual; the verdict may never reach the packet, re-gate`
          : `gating · ${elapsed}`,
        since: g.since,
      });
    }
    return {
      index: i,
      busy: true,
      branch: g.branch,
      packetId: g.packet_id,
      wagonId: id,
      tag: tagOf(g.branch),
      since: g.since,
      elapsed,
      stale: g.stale,
      progress,
    };
  });

  // THE GATE QUEUE — the lane feeding the bays. A run here has taken a
  // place in line (the server's `queued_at` order) and holds no bay: it
  // has no Job, no pod, no workspace. Drawn in the NEUTRAL tone, because
  // a queue is the pipeline working to its bound, not an alarm — the
  // distinction the floor learned from held greens.
  const queue = status?.gates.queued ?? [];
  queue.forEach(q => {
    if (claimedBranches.has(q.branch)) return;
    const car = carByBranch.get(q.branch);
    const id = car && !claimedIds.has(car.id) ? car.id : q.packet_id;
    place({
      id,
      tag: tagOf(q.branch),
      title: car?.title ?? q.branch,
      branch: q.branch,
      head: car?.head ?? null,
      kind: car?.kind ?? 'gate-run',
      sim: car?.sim ?? false,
      station: 'gate-queue',
      slot: q.position > 0 ? q.position - 1 : 0,
      trainId: null,
      tone: 'static',
      lamp: 'off',
      status: `queued · ${queueLabel(q)}`,
      since: q.queued_at,
    });
  });

  // THE LOADING DOCK — parked cars in the station's own order.
  const parkedSince = new Map((status?.dock ?? []).map(d => [d.id, d.parked_since]));
  let dockSlot = 0;
  yard.dock.forEach(c => {
    if (claimedIds.has(c.id) || claimedBranches.has(c.branch)) return;
    place({
      id: c.id,
      ...base(c),
      station: 'dock',
      slot: dockSlot,
      trainId: null,
      tone: 'ok',
      lamp: 'ok',
      status: c.skipReason ? `parked · held: ${c.skipReason}` : 'parked · gated green, waiting to board',
      since: parkedSince.get(c.id) ?? null,
    });
    dockSlot += 1;
  });

  // THE APPROACH, THE GATE EXIT AND THE GARAGE — the rows upstream of
  // the dock, each on the siding its verdict puts it on.
  const garageByBranch = new Map((status?.garage ?? []).map(g => [g.branch, g]));
  let approachSlot = 0;
  let limboSlot = 0;
  let garageSlot = 0;
  let publishing = 0;
  let held = 0;
  let strandedRows = 0;
  yard.approach.forEach(row => {
    if (claimedBranches.has(row.branch)) return;
    const car = carByBranch.get(row.branch);
    const id = car && !claimedIds.has(car.id) ? car.id : row.id;
    const shared = {
      id,
      tag: tagOf(row.branch),
      title: car?.title ?? row.branch,
      branch: row.branch,
      head: car?.head ?? shortSha(row.sha),
      kind: car?.kind ?? (row.state === 'publishing' ? 'publish-request' : 'gate-run'),
      sim: car?.sim ?? false,
      trainId: null,
    };
    switch (row.state) {
      case 'publishing':
        publishing += 1;
        place({
          ...shared,
          station: 'approach',
          slot: approachSlot++,
          tone: 'static',
          lamp: 'working',
          status: row.note ? `publishing · requested by ${row.note}` : 'publishing',
          since: row.opened_on,
        });
        return;
      case 'gated-red': {
        if (row.verdict === 'lost') {
          place({
            ...shared,
            station: 'limbo',
            slot: limboSlot++,
            tone: 'warn',
            lamp: 'warn',
            status: 'gate lost — the environment died before a verdict; re-gate',
            since: row.opened_on,
          });
          return;
        }
        const g = garageByBranch.get(row.branch);
        place({
          ...shared,
          station: 'garage',
          slot: garageSlot++,
          tone: 'red',
          lamp: 'err',
          status: `garaged · red gate (${g?.failed_check ?? 'run died outside a check'}) — rework`,
          since: g?.since ?? row.opened_on,
        });
        return;
      }
      case 'gated-green':
        strandedRows += 1;
        place({
          ...shared,
          station: 'approach',
          slot: approachSlot++,
          tone: 'warn',
          lamp: 'warn',
          status: 'stranded green · gated, never parked — rebase + re-gate',
          since: row.opened_on,
        });
        return;
      case 'held':
        held += 1;
        place({
          ...shared,
          station: 'approach',
          slot: approachSlot++,
          tone: 'ok',
          lamp: 'off',
          status: `held — ${holdReason(row.hold)}`,
          since: row.opened_on,
        });
        return;
    }
  });
  // A branch the server garages that the approach no longer lists (its
  // verdict aged out of the window): still red, still in the garage.
  for (const g of status?.garage ?? []) {
    if (claimedBranches.has(g.branch)) continue;
    place({
      id: `garage:${g.branch}`,
      tag: tagOf(g.branch),
      title: g.branch,
      branch: g.branch,
      head: null,
      kind: 'gate-run',
      sim: false,
      station: 'garage',
      slot: garageSlot++,
      trainId: null,
      tone: 'red',
      lamp: 'err',
      status: `garaged · red gate (${g.failed_check ?? 'run died outside a check'}) — rework`,
      since: g.since,
    });
  }

  // THE SIGNALS — lit by the lead locomotive, the one furthest along.
  const lead = locos.reduce<Loco | null>((best, l) => (best === null || l.stage > best.stage ? l : best), null);
  const signals: Signal[] = STAGES.map((_, i) => {
    if (!lead) return 'off';
    if (i < lead.stage) return 'ok';
    if (i === lead.stage) return lead.blocked ? 'err' : 'now';
    return 'off';
  });

  // THE DEPARTURE BOARD — in flight along the line, then landed.
  const whereOf = (w: Wagon): string => {
    switch (w.station) {
      case 'approach':
        return 'Approach';
      case 'gate-queue':
        return `Gate queue · #${w.slot + 1}`;
      case 'gate':
        return `Gate bay ${w.slot + 1}`;
      case 'limbo':
        return 'Gate exit';
      case 'dock':
        return `Dock · slot ${w.slot + 1}`;
      case 'garage':
        return 'Garage';
      case 'train': {
        const l = w.trainId ? locoById.get(w.trainId) : undefined;
        return l ? `Track · ${locoName(l)} at ${STAGES[l.stage]}` : 'Track';
      }
      case 'arrivals':
        return 'Arrivals';
    }
  };
  const sinceMs = (w: Wagon): number => {
    const ms = w.since ? Date.parse(w.since) : Number.NaN;
    return Number.isNaN(ms) ? Number.POSITIVE_INFINITY : ms;
  };
  const inFlight = wagons
    .filter(w => w.station !== 'arrivals')
    .sort(
      (a, b) =>
        STATION_RANK[a.station] - STATION_RANK[b.station] || a.slot - b.slot || sinceMs(a) - sinceMs(b),
    );
  const landedRows = wagons.filter(w => w.station === 'arrivals').sort((a, b) => a.slot - b.slot);
  const boardRows: BoardRow[] = [
    ...inFlight.map(w => ({ id: w.id, where: whereOf(w), landed: false })),
    ...landedRows.map(w => ({ id: w.id, where: whereOf(w), landed: true })),
  ];

  // THE MACHINES — each label a fact the read model states.
  const stranded = status ? status.stranded.length : strandedRows;
  const approachParts = [
    ...(stranded > 0 ? [`${stranded} stranded`] : []),
    ...(publishing > 0 ? [`${publishing} publishing`] : []),
    ...(held > 0 ? [`${held} held`] : []),
  ];
  const b = status?.boarding ?? null;
  const parked = yard.dock.length;
  const cooldown = b?.cooldown_remaining_minutes ?? null;
  // An empty dock is not held — there is nothing to hold — so it says
  // what the next car would meet; a dock with cars says what keeps
  // them, in the server's words, or that they depart on the next tick.
  const dockHeld = b?.held_because ?? null;
  const cooling = cooldown !== null && cooldown > 0;
  const dockLabel =
    parked === 0
      ? cooling
        ? `empty · cooldown ${cooldown} min`
        : dockHeld === null && b?.next_board
          ? 'empty · boards on the next tick'
          : 'empty'
      : dockHeld !== null
        ? `${parked} parked · held: ${dockHeld}`
        : b?.next_board
          ? `${parked} parked · departs next tick`
          : cooling
            ? `${parked} parked · cooldown ${cooldown} min`
            : `${parked} parked`;
  const garageCount = wagons.filter(w => w.station === 'garage').length;

  return {
    now: status?.now ?? new Date(nowMs).toISOString(),
    wagons,
    locos,
    bays,
    signals,
    boardRows,
    machines: {
      approach: {
        label: approachParts.length > 0 ? approachParts.join(' · ') : 'clear',
        stranded,
        publishing,
        held,
      },
      dock: {
        label: dockLabel,
        parked,
        held: dockHeld,
        next: b?.next_board ?? null,
        cooldownMinutes: cooldown,
        lamp: parked > 0 ? 'ok' : 'off',
      },
      queue: { label: queueLaneLabel(queue), count: queue.length },
      garage: { label: garageCount > 0 ? `${garageCount} gated red` : 'empty', count: garageCount },
      arrivals: { label: `${landedRecently} landed · 24h`, landed: landedRecently },
      conductor: conductorMachine(status?.conductor ?? null),
      runner: feeds.runner,
      cluster: feeds.cluster,
    },
  };
}
