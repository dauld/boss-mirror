// The Marshalling Yard's read model — the upstream third of the
// operator surface (packet babcb6cd; the design was settled on
// 2d4a5a8b and the prototype ran on real data).
//
// The Train Yard shows the last third of a car's life. This shows the
// first: what is waiting, on whom, for how long — and, the reason this
// exists rather than a depth board, WHETHER A BOTTLENECK IS FORMING.
//
// DEPTH IS NOT THE ANSWER, and the server says so in its own words.
// `GET /api/stations/load`: *"Depth is close to meaningless without a
// drain rate — ten role queues each read exactly 48 the day this was
// written and none was a bottleneck, they were a bug."* Forming is a
// rate question, so this module joins depth against
// `GET /api/stations/flow` — arrivals and departures over a wall-clock
// window, counted from the log's own `step.ready` / `step.done`
// transitions — and derives one reading from the pair: how long this
// queue would take to clear at the rate it is actually being worked.
//
// NO NUMBER THIS SURFACE MAKES UP. The yard's rule holds upstream. A
// station whose membership the log cannot be attributed to comes back
// `basis: "unavailable"` with the clause that blinded it, and it
// renders as that sentence — never as a zero, which on a queue board
// reads as "nothing is waiting". Such a station is also never named
// the constraint: naming it would be asserting a rate nobody counted.

import { fetchRemote, type Remote } from '../../data/remote';

// ---------------------------------------------------------------------
// Wire shapes — GET /api/stations/load and GET /api/stations/flow,
// parsed once here (house style: deserialize at the fetch call site).
// ---------------------------------------------------------------------

export type StationLoadRow = Readonly<{
  station: string;
  kind: string;
  depth: number;
  wipLimit: number | null;
  overLimit: boolean;
  /** Age of the oldest MEMBER PACKET, from its `opened_on` — packet
   *  age, not time-in-this-queue, which is what the server documents
   *  it to be. Over-reports rather than under-reports. */
  oldestAgeDays: number | null;
  capabilityRoles: ReadonlyArray<string> | null;
}>;

/** What the log could be made to say about one station's flow. */
export type StationFlowRow = Readonly<{
  station: string;
  basis: 'step-events' | 'unavailable';
  arrived: number | null;
  served: number | null;
  net: number | null;
  unavailableReason: string | null;
}>;

export type StationFlowEnvelope = Readonly<{
  rows: ReadonlyArray<StationFlowRow>;
  /** Read off the envelope, never assumed: the server clamps it. */
  windowHours: number | null;
  asOf: string | null;
}>;

const num = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null);
const str = (v: unknown): string | null => (typeof v === 'string' ? v : null);
const rows = (raw: unknown): ReadonlyArray<Record<string, unknown>> => {
  const data = (raw as { data?: unknown } | null)?.data;
  return Array.isArray(data) ? (data as ReadonlyArray<Record<string, unknown>>) : [];
};

export function parseStationLoad(raw: unknown): ReadonlyArray<StationLoadRow> {
  return rows(raw)
    .map((r) => ({
      station: str(r.station) ?? '',
      kind: str(r.kind) ?? '',
      depth: num(r.depth) ?? 0,
      wipLimit: num(r.wip_limit),
      overLimit: r.over_limit === true,
      oldestAgeDays: num(r.oldest_age_days),
      capabilityRoles: Array.isArray(r.capability_roles)
        ? (r.capability_roles.filter((x) => typeof x === 'string') as ReadonlyArray<string>)
        : null,
    }))
    .filter((r) => r.station !== '');
}

export function parseStationFlow(raw: unknown): StationFlowEnvelope {
  return {
    rows: rows(raw)
      .map((r) => ({
        station: str(r.station) ?? '',
        basis: r.basis === 'step-events' ? ('step-events' as const) : ('unavailable' as const),
        arrived: num(r.arrived),
        served: num(r.served),
        net: num(r.net),
        unavailableReason: str(r.unavailable_reason),
      }))
      .filter((r) => r.station !== ''),
    windowHours: num((raw as { window_hours?: unknown } | null)?.window_hours),
    asOf: str((raw as { as_of?: unknown } | null)?.as_of),
  };
}

// ---------------------------------------------------------------------
// The joined siding
// ---------------------------------------------------------------------

/** A queue's flow, or the honest reason there is none to show. */
export type SidingFlow =
  | { readonly kind: 'counted'; readonly arrived: number; readonly served: number; readonly net: number }
  | { readonly kind: 'unavailable'; readonly reason: string };

export type Siding = Readonly<{
  station: string;
  kind: string;
  depth: number;
  wipLimit: number | null;
  overLimit: boolean;
  oldestAgeDays: number | null;
  capabilityRoles: ReadonlyArray<string> | null;
  flow: SidingFlow;
}>;

/** The sentence a station gets when the flow read did not mention it
 *  at all — a station added between the two reads, or a flow read that
 *  failed. Still not a zero. */
const NOT_IN_FLOW_READ =
  'the flow read did not report this station, so its rate is unknown rather than nil';

/**
 * Depth (from `load`) joined to rate (from `flow`), keyed by station.
 * Sorted by pressure: the queues that are not draining first, then the
 * deepest, then the oldest. Empty queues sink to the bottom — they are
 * the answer to "where is the work NOT", which a board still wants to
 * be able to state, but never at the top.
 */
export function joinSidings(
  load: ReadonlyArray<StationLoadRow>,
  flow: StationFlowEnvelope,
): ReadonlyArray<Siding> {
  const byStation = new Map(flow.rows.map((r) => [r.station, r]));
  const joined: Siding[] = load.map((l) => {
    const f = byStation.get(l.station);
    const counted =
      f !== undefined && f.basis === 'step-events' && f.arrived !== null && f.served !== null;
    return {
      station: l.station,
      kind: l.kind,
      depth: l.depth,
      wipLimit: l.wipLimit,
      overLimit: l.overLimit,
      oldestAgeDays: l.oldestAgeDays,
      capabilityRoles: l.capabilityRoles,
      flow: counted
        ? {
            kind: 'counted',
            arrived: f!.arrived!,
            served: f!.served!,
            net: f!.net ?? f!.arrived! - f!.served!,
          }
        : { kind: 'unavailable', reason: f?.unavailableReason ?? NOT_IN_FLOW_READ },
    };
  });
  return [...joined].sort((a, b) => pressure(b) - pressure(a) || a.station.localeCompare(b.station));
}

/** Sort key only — never rendered. A stalled queue outranks a deep one
 *  outranks an old one, and an empty queue outranks nothing. */
function pressure(s: Siding): number {
  if (s.depth === 0) return -1;
  const stalled = s.flow.kind === 'counted' && s.flow.served === 0 ? 1_000_000 : 0;
  return stalled + s.depth * 10 + (s.oldestAgeDays ?? 0);
}

// ---------------------------------------------------------------------
// The rate reading
// ---------------------------------------------------------------------

/**
 * How long this queue would take to clear at the rate it was ACTUALLY
 * worked over the window: `depth / (served / windowHours)`.
 *
 * `null` means it is not draining — nothing left the queue in the
 * window, so there is no rate at which it clears. Deliberately null
 * rather than `Infinity` or some large number: a board that prints
 * "9999 h" invites the reader to compare it with a real figure, and a
 * queue nothing is leaving is a different KIND of fact from a slow one.
 */
export function drainHours(depth: number, served: number, windowHours: number): number | null {
  if (depth <= 0) return 0;
  if (served <= 0 || windowHours <= 0) return null;
  return (depth * windowHours) / served;
}

export type Constraint =
  | { readonly kind: 'none'; readonly because: string }
  | {
      readonly kind: 'station';
      readonly station: string;
      readonly depth: number;
      readonly drainHours: number | null;
      readonly because: string;
    };

/**
 * Which queue is the constraint right now. One always is; naming it is
 * the point of the page.
 *
 * The rule: among queues that hold work AND whose rate was counted,
 * the constraint is the one that would take longest to clear at its
 * observed service rate — with "not draining at all" ranking above
 * every finite time, because a queue nothing leaves never clears.
 *
 * A queue whose rate could not be counted is NOT eligible. Naming it
 * would be asserting a rate nobody measured; instead, when the only
 * queues holding work are uncountable, this says so and names them.
 */
export function constraintOf(
  sidings: ReadonlyArray<Siding>,
  windowHours: number,
): Constraint {
  const holding = sidings.filter((s) => s.depth > 0);
  if (holding.length === 0) {
    return { kind: 'none', because: 'Nothing is waiting at any watched station.' };
  }
  const countable = holding.filter((s) => s.flow.kind === 'counted');
  if (countable.length === 0) {
    const names = holding.map((s) => s.station).join(', ');
    return {
      kind: 'none',
      because:
        `Work is held at ${names}, but no watched queue's rate could be counted ` +
        `from the log, so no queue can honestly be named the constraint.`,
    };
  }
  const ranked = [...countable].sort((a, b) => rank(b, windowHours) - rank(a, windowHours));
  const worst = ranked[0]!;
  const served = worst.flow.kind === 'counted' ? worst.flow.served : 0;
  const arrived = worst.flow.kind === 'counted' ? worst.flow.arrived : 0;
  const hours = drainHours(worst.depth, served, windowHours);
  const because =
    hours === null
      ? `${worst.depth} waiting, ${arrived} arrived and nothing left in the last ` +
        `${windowHours}h — this queue is not draining, so it clears at no rate at all.`
      : `${worst.depth} waiting against ${served} served in the last ${windowHours}h: ` +
        `${hours.toFixed(0)}h to clear at the rate it is actually being worked, ` +
        `longer than any other counted queue.`;
  return { kind: 'station', station: worst.station, depth: worst.depth, drainHours: hours, because };
}

/** Sort key only. Not draining beats any finite clear time; ties break
 *  on depth so a stalled 41 outranks a stalled 2. */
function rank(s: Siding, windowHours: number): number {
  const served = s.flow.kind === 'counted' ? s.flow.served : 0;
  const hours = drainHours(s.depth, served, windowHours);
  return hours === null ? 1e12 + s.depth : hours;
}

/**
 * The Factorio grammar the floor uses, upstream: every queue states
 * why it is not moving, in a sentence, from measured facts only.
 */
export function whyNotMoving(s: Siding, windowHours: number): string {
  if (s.flow.kind === 'unavailable') {
    return s.depth === 0
      ? `Clear. Rate not counted — ${s.flow.reason}.`
      : `${s.depth} waiting; rate not counted — ${s.flow.reason}.`;
  }
  const { arrived, served, net } = s.flow;
  if (s.depth === 0) {
    return served > 0
      ? `Clear — ${served} served in the last ${windowHours}h.`
      : `Clear, and nothing arrived in the last ${windowHours}h.`;
  }
  if (served === 0) {
    // The oldest-item trend, stated from what was measured: with no
    // departure, the head of the queue is exactly the window older
    // than it was.
    return (
      `Nothing left this queue in the last ${windowHours}h (${arrived} arrived), ` +
      `so its oldest packet is ${windowHours}h older than it was.`
    );
  }
  const hours = drainHours(s.depth, served, windowHours);
  const clears = hours === null ? '' : ` — ${hours.toFixed(0)}h to clear at that rate`;
  if (net > 0) {
    return `${arrived} arrived, ${served} served: growing by ${net} a ${windowHours}h window${clears}.`;
  }
  if (net < 0) {
    return `${arrived} arrived, ${served} served: shrinking by ${-net} a ${windowHours}h window${clears}.`;
  }
  return `${arrived} arrived, ${served} served: holding level${clears}.`;
}

// ---------------------------------------------------------------------
// The waits — GET /api/jobs/queue-age
// ---------------------------------------------------------------------
//
// A station's `oldest_age_days` is the PACKET's age, from `opened_on`:
// a packet that spent eight days in review reads as eight days old the
// moment it arrives here. That is the honest per-station figure (a
// station predicate is packet-level) and it over-reports.
//
// The step-level answer — how long has THIS obligation waited, from
// the projection's `became_ready_at` stamp — is the queue-age lens, and
// it is what "on whom, since when" actually means. This page is its
// first reader.

export type Wait = Readonly<{
  jobId: string;
  jobKind: string;
  jobTitle: string;
  stepTitle: string;
  specSlug: string | null;
  status: string;
  assigneeId: string | null;
  waitingDays: number;
  /** False when the stamp is the `updated_at` fallback — a LOWER
   *  BOUND, and it has to render as one. */
  exact: boolean;
  simulated: boolean;
}>;

export function parseQueueAge(raw: unknown): Readonly<{
  waits: ReadonlyArray<Wait>;
  now: string | null;
}> {
  return {
    waits: rows(raw)
      .map((r) => ({
        jobId: str(r.job_id) ?? '',
        jobKind: str(r.job_kind) ?? '',
        jobTitle: str(r.job_title) ?? '',
        stepTitle: str(r.step_title) ?? '',
        specSlug: str(r.spec_slug),
        status: str(r.status) ?? '',
        assigneeId: str(r.assignee_id),
        waitingDays: num(r.waiting_days) ?? 0,
        exact: r.exact === true,
        simulated: r.simulated === true,
      }))
      .filter((w) => w.jobId !== ''),
    now: str((raw as { now?: unknown } | null)?.now),
  };
}

/**
 * The longest-waiting real obligations, longest first.
 *
 * Simulated packets are excluded, and not as a tidy-up: the brewery's
 * event time runs about a thousand times faster than the wall, so a
 * simulated wait measured against a real clock is a number with no
 * meaning that would sit permanently at the top of this table.
 */
export function longestWaits(waits: ReadonlyArray<Wait>, limit: number): ReadonlyArray<Wait> {
  return [...waits]
    .filter((w) => !w.simulated)
    .sort((a, b) => b.waitingDays - a.waitingDays)
    .slice(0, Math.max(0, limit));
}

/** How long it has waited — and, when the stamp is a fallback, that it
 *  is a floor rather than the figure. */
export function waitText(w: Wait): string {
  const magnitude =
    w.waitingDays >= 1 ? `${Math.floor(w.waitingDays)} d` : `${Math.round(w.waitingDays * 24)} h`;
  return w.exact ? magnitude : `at least ${magnitude}`;
}

// ---------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------

export function loadStations(): Promise<Exclude<Remote<ReadonlyArray<StationLoadRow>>, { kind: 'loading' }>> {
  return fetchRemote('/api/stations/load', parseStationLoad);
}

export function loadFlow(
  windowHours: number,
): Promise<Exclude<Remote<StationFlowEnvelope>, { kind: 'loading' }>> {
  return fetchRemote(`/api/stations/flow?window_hours=${windowHours}`, parseStationFlow);
}

export function loadWaits(): Promise<
  Exclude<Remote<ReturnType<typeof parseQueueAge>>, { kind: 'loading' }>
> {
  return fetchRemote('/api/jobs/queue-age', parseQueueAge);
}
