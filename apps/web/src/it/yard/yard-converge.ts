// THE CONVERGE CARD — the least visible step gets a surface.
//
// The converge is the one delivery step with NO surface of its own: its
// image build is a systemd run on the forge host, so there is no
// Forgejo Actions page and, until this module, no BOSS page either. On
// 2026-09-11 two converge rolls happened (60 s and 31 s) and the only
// reason anyone knew they were healthy was a monitor tailing the forge
// journal by hand (backlog 84c47438, built on the four answers folded
// onto design-doc bed355ce: POLL not stream; THE TAIL not the archive;
// the GATEWAY is the single authenticated reader; the CONVERGE FIRST).
//
// WHAT THIS READS. Not the ops-request that asked for a converge — the
// deploy-runner shed in yard-machines.ts already reads that, and it can
// only prove the unit was STARTED. This reads the run's own packet:
// `cluster-deploy-runner.service` opens a `maintenance-cluster-converge`
// packet from ExecStartPre (infra/boss-maintenance-wrap.sh) and closes it
// from ExecStopPost (infra/boss-step.sh), which merges the run's summary
// file onto the `run` step. So the packet carries, as step metadata and
// as STRINGS (run-summary.sh: "every field is authored as a string"):
//
//   unchanged   — the head forge main was already on; the run did nothing
//   build_head  — the head it built, and
//   build_s / push_s / roll_s / verify_s — how long each stage took,
//                 stamped by the stage the moment it ended (a run that
//                 dies keeps what it had recorded)
//   result      — `ok`, or systemd's word for how the run died, with
//                 `exit_status` beside it
//   summary_absent — boss-step.sh's own sentence when the run left no
//                 summary file: a finding, not silence
//
// An OPEN packet is a run in flight. Its stamps land only when it ends,
// so while it runs the card can say since when and for how long, and
// must say that the stages are not yet on the record — not draw four
// empty stages as if they were.
//
// WHAT THIS DOES NOT READ, AND SAYS SO. Answer (2) wants the live
// journal tail of the unit beside these facts. The door exists
// (systemd-journal-gatewayd on the forge, fronted by
// infra/forge/journal-read.sh with its FRESH / STALE / UNREACHABLE
// vocabulary), but answer (3) routes it through the gateway, and the
// gateway has no journal route today — grep `journal` under
// crates/core/boss-gateway/src and nothing answers. This module does
// not invent a transport: `JOURNAL_TAIL` is the card's own statement
// that the tail is not wired, naming the shell door that reads it, so
// the card never shows an empty tail as a quiet converge.
//
// SILENCE IS A READING. The timer fires every ten minutes
// (cluster-deploy-runner.timer, OnUnitActiveSec=10min), so while the
// forge is up a fresh packet arrives at least that often. A newest
// packet older than the threshold means the forge, its timer, or the
// path to the record is down — and the card says so with both clocks
// (CLAUDE.md §Diagnosis: a troubled packet must look troubled). The
// threshold is the one number BOSS already uses for a silent loop.

import type { JobLite, StepLite } from './yard';
import type { RunnerMachine } from './yard-machines';
import { clockText } from './yard-status';

/** The timer's cadence (cluster-deploy-runner.timer). */
export const CONVERGE_TIMER_MINUTES = 10;

/** Past this, the newest packet's age is SILENCE. 30 minutes: three
 *  timer ticks, and the SAME threshold as the convergence arm
 *  (CLAUDE.md §Diagnosis) and infra/forge/journal-read.sh's
 *  freshness assertion — one number, not a second one to remember. */
export const CONVERGE_SILENCE_MAX_S = 1800;

/** What the run stamped on its `run` step, typed. `null` is "not on
 *  the record" — a stage the run never reached, or a tick that did not
 *  build. */
export type ConvergeStages = Readonly<{
  unchanged: string | null;
  buildHead: string | null;
  buildS: number | null;
  pushS: number | null;
  rollS: number | null;
  verifyS: number | null;
}>;

export type ConvergeRun =
  /** The rows were not read (fetch failed, or not yet). Not idle. */
  | Readonly<{ kind: 'unknown' }>
  /** Read fine; no converge packet in the window. */
  | Readonly<{ kind: 'none' }>
  /** An open packet: the unit is running. No stamps yet. */
  | Readonly<{ kind: 'running'; id: string; since: string | null }>
  /** The newest closed packet. */
  | Readonly<{
      kind: 'ended';
      id: string;
      at: string | null;
      outcome: 'completed' | 'failed';
      result: string | null;
      exitStatus: string | null;
      stages: ConvergeStages;
      summaryAbsent: string | null;
    }>;

const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);

/** A stage's seconds, as the string run-summary.sh wrote; anything that
 *  is not a whole number is "not on the record". */
const secs = (v: unknown): number | null => {
  const s = str(v);
  if (s === null || !/^\d+$/.test(s)) return null;
  return Number(s);
};

const md = (j: JobLite): Record<string, unknown> => j.metadata ?? {};

const ms = (s: string | null): number => {
  const n = s ? Date.parse(s) : Number.NaN;
  return Number.isNaN(n) ? Number.NEGATIVE_INFINITY : n;
};

const openedAt = (j: JobLite): string | null => str(md(j).opened_at) ?? str(j.opened_on);
const closedAt = (j: JobLite): string | null => str(md(j).closed_at);

const runStep = (j: JobLite): StepLite | null =>
  (j.steps ?? []).find(s => s.spec_slug === 'run') ?? (j.steps ?? []).find(s => s.title === 'run') ?? null;

function stagesOf(run: StepLite | null): ConvergeStages {
  const m = run?.metadata ?? {};
  return {
    unchanged: str(m.unchanged),
    buildHead: str(m.build_head),
    buildS: secs(m.build_s),
    pushS: secs(m.push_s),
    rollS: secs(m.roll_s),
    verifyS: secs(m.verify_s),
  };
}

/** The converge packets in the rows, newest-opened first. Filtered on
 *  kind so a mixed fetch cannot feed a stray packet in. */
function convergePackets(rows: readonly JobLite[]): readonly JobLite[] {
  return rows
    .filter(j => j.kind === 'maintenance-cluster-converge')
    .sort((a, b) => ms(openedAt(b)) - ms(openedAt(a)));
}

/** The converge reading from the rows the page fetched (null before
 *  the first read). An OPEN packet outranks every closed one: the live
 *  fact is the run in flight. */
export function convergeRun(rows: readonly JobLite[] | null, _nowMs: number): ConvergeRun {
  if (rows === null) return { kind: 'unknown' };
  const packets = convergePackets(rows);
  const open = packets.find(j => j.status === 'open');
  if (open !== undefined) return { kind: 'running', id: open.id, since: openedAt(open) };
  const j = packets[0];
  if (!j) return { kind: 'none' };
  const run = runStep(j);
  const m = run?.metadata ?? {};
  const result = str(m.result);
  return {
    kind: 'ended',
    id: j.id,
    at: closedAt(j),
    // The workflow's own terminals: `completed` when result = "ok",
    // `failed` otherwise (maintenance-cluster-converge.toml). Read the
    // packet's outcome when stamped, else derive it the same way.
    outcome: md(j).outcome === 'failed' || (md(j).outcome !== 'completed' && result !== 'ok') ? 'failed' : 'completed',
    result,
    exitStatus: str(m.exit_status),
    stages: stagesOf(run),
    summaryAbsent: str(m.summary_absent),
  };
}

export type StageName = 'build' | 'push' | 'roll' | 'verify';
export type StageRow = Readonly<{ stage: StageName; seconds: number }>;

/** The stages a run stamped, in the order the runner executes them.
 *  Only stages ON THE RECORD appear: a run that died after its push
 *  shows build and push and stops, an unchanged tick shows none. */
export function stageRows(s: ConvergeStages): readonly StageRow[] {
  const all: readonly (readonly [StageName, number | null])[] = [
    ['build', s.buildS],
    ['push', s.pushS],
    ['roll', s.rollS],
    ['verify', s.verifyS],
  ];
  return all.flatMap(([stage, seconds]) => (seconds === null ? [] : [{ stage, seconds }]));
}

/** Seconds, the way an operator reads a stage: 6s · 5m29s · 1h02m. */
export function durText(seconds: number): string {
  const s = Math.max(Math.round(seconds), 0);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m${String(s % 60).padStart(2, '0')}s`;
  return `${Math.floor(s / 3600)}h${String(Math.floor((s % 3600) / 60)).padStart(2, '0')}m`;
}

const elapsedS = (since: string | null, nowMs: number): number | null => {
  const t = ms(since);
  return t === Number.NEGATIVE_INFINITY ? null : Math.max(nowMs - t, 0) / 1000;
};

/** The last stage a run reached, for "FAILED after push". */
const lastStage = (s: ConvergeStages): StageName | null => stageRows(s).at(-1)?.stage ?? null;

/** How a failed run died, in one phrase: systemd's result word, the
 *  exit status, and the last stage on the record. */
export function failureText(r: Extract<ConvergeRun, { kind: 'ended' }>): string {
  const how = `${r.result ?? 'no result recorded'}${r.exitStatus !== null ? ` (exit ${r.exitStatus})` : ''}`;
  const reached = lastStage(r.stages);
  return reached !== null ? `${how} after ${reached}` : how;
}

/** The card's one line. */
export function convergeLabel(r: ConvergeRun, nowMs: number): string {
  switch (r.kind) {
    case 'unknown':
      return 'no reading — the converge packets did not answer';
    case 'none':
      return 'no converge packet in the window';
    case 'running': {
      const clock = clockText(r.since);
      const for_ = elapsedS(r.since, nowMs);
      const started = clock !== null ? ` · started ${clock}` : '';
      const dur = for_ !== null ? ` · ${durText(for_)}` : '';
      return `converging${started}${dur} — stages land when the run ends`;
    }
    case 'ended': {
      const clock = clockText(r.at);
      const lead = clock !== null ? `${clock} · ` : '';
      if (r.outcome === 'failed') return `${lead}FAILED · ${failureText(r)}`;
      if (r.stages.unchanged !== null) return `${lead}forge main unchanged at ${r.stages.unchanged}`;
      const rows = stageRows(r.stages);
      if (rows.length === 0) {
        return r.summaryAbsent !== null ? `${lead}ok, but the run left no summary` : `${lead}ok, no stages on the record`;
      }
      const total = rows.reduce((n, s) => n + s.seconds, 0);
      const head = r.stages.buildHead !== null ? ` ${r.stages.buildHead}` : '';
      const build = r.stages.buildS !== null ? ` (build ${durText(r.stages.buildS)})` : '';
      return `${lead}converged${head} in ${durText(total)}${build}`;
    }
  }
}

/** Has the timer left a packet recently enough? */
export type ConvergeSilence =
  /** No rows, or none of this kind: nothing to measure — never fresh. */
  | Readonly<{ kind: 'unread' }>
  | Readonly<{ kind: 'fresh'; newest: string; ageS: number }>
  | Readonly<{ kind: 'silent'; newest: string; ageS: number; maxS: number }>;

/** The age of the newest packet's OPENING, against the threshold. An
 *  open packet is a run that has not ended — not silence, whatever its
 *  age; the card's running label carries its own duration. */
export function convergeSilence(rows: readonly JobLite[] | null, nowMs: number): ConvergeSilence {
  if (rows === null) return { kind: 'unread' };
  const packets = convergePackets(rows);
  const j = packets[0];
  if (!j) return { kind: 'unread' };
  const newest = openedAt(j);
  if (newest === null) return { kind: 'unread' };
  const ageS = Math.round(Math.max(nowMs - ms(newest), 0) / 1000);
  if (j.status === 'open' || ageS <= CONVERGE_SILENCE_MAX_S) return { kind: 'fresh', newest, ageS };
  return { kind: 'silent', newest, ageS, maxS: CONVERGE_SILENCE_MAX_S };
}

/** The shed reads the run itself over the request. The ops-request
 *  proves the unit was started; the maintenance packet IS the run. An
 *  open run makes the shed RUNNING from the packet's own clock; a run
 *  that died makes it FAILED, unless the request side already holds a
 *  newer stamp (a later request has been answered, so a later run is
 *  the live fact). Everything else leaves the shed's reading alone. */
export function withConverge(shed: RunnerMachine, run: ConvergeRun): RunnerMachine {
  if (run.kind === 'running') return { kind: 'running', id: run.id, since: run.since, host: 'forge' };
  if (run.kind !== 'ended' || run.outcome !== 'failed') return shed;
  const shedAt =
    shed.kind === 'idle' ? (shed.last?.at ?? null) : shed.kind === 'unknown' ? null : 'at' in shed ? shed.at : shed.since;
  if (ms(shedAt) > ms(run.at)) return shed;
  return { kind: 'failed', id: run.id, at: run.at, reason: failureText(run) };
}

/** The live journal tail — answer (2) — is NOT WIRED. The gateway has
 *  no journal route, and answer (3) forbids any other transport to the
 *  browser. The card says so in these words and names the door that
 *  does read it, rather than drawing an empty tail as calm. */
export const JOURNAL_TAIL = {
  kind: 'unwired',
  unit: 'cluster-deploy-runner.service',
  why: 'the gateway has no journal route yet; the browser holds no credential for the forge door, by design',
  command: 'infra/forge/journal-read.sh _SYSTEMD_UNIT=cluster-deploy-runner.service --count 50',
} as const;

/** How many converge packets the page reads: one lands every ten
 *  minutes, so six is an hour — enough to hold the last real build
 *  beside the unchanged ticks after it. A window, not a filter: the
 *  card reads the newest, never counts. */
const CONVERGE_WINDOW = 6;

export async function fetchConverges(): Promise<readonly JobLite[] | null> {
  try {
    const r = await fetch(`/api/jobs?kind=maintenance-cluster-converge&limit=${CONVERGE_WINDOW}`);
    if (!r.ok) return null;
    const body = (await r.json()) as { data?: JobLite[] };
    return Array.isArray(body.data) ? body.data : null;
  } catch {
    return null;
  }
}
