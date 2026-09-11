// The two machines the floor reads from OUTSIDE the yard status: the
// deploy-runner shed and the cluster tower (Car 2a of the rail-map
// redesign). Both are pure derivations the page feeds; neither invents
// a state the record does not hold.
//
// THE SHED reads the converge ops-request packets. On a merge the
// dispatcher's `converge-on-merge` rule files an ops-request
// (`metadata.verb = "converge"`, `metadata.host = "forge"`); the
// forge's ops-runner polls every minute, runs `systemctl start
// --no-block cluster-deploy-runner.service`, and completes `execute`
// with `disposition`, `exit_code` and `output`. Read off live rows
// (2026-09-08): no STEP carries an instant — only the day-granular
// `completed_on` — so the packet's own `metadata.opened_at` and
// `metadata.closed_at` are the clocks; the runner completes `execute`
// in one write, so an `active` step is possible but never seen. The
// packet therefore distinguishes: requested (open, unanswered),
// running (claimed, or answered and the unit just started), failed
// (refused, or a non-zero exit), and idle (the last converge on
// record). "Done" is not a separate state the packet can tell from
// idle, so it is not one here.
//
// THE TOWER reads /api/jobs/health from outside — the same field the
// conductor verifies convergence against (`capabilities.commit`). A
// fetch that fails or answers non-2xx is DARK; that is the reading the
// 2026-09-05 outage wanted a page to show.

import type { JobLite, StepLite } from './yard';
import { clockText, elapsedText } from './yard-status';

// ---------------------------------------------------------------------
// The deploy-runner shed.
// ---------------------------------------------------------------------

/** How long a converge usually takes end to end — build, push, roll,
 *  the Ready wait — measured on 2026-09-08: #260 merged 00:40:43 →
 *  verified 00:50:41, #262 02:20:56 → 02:30:24 (~10 min). The shed
 *  smokes for this long after the unit is started; it is a drawing
 *  scale, not a fact the packet holds — the tower says whether the
 *  cluster actually moved. */
export const CONVERGE_USUAL_MINUTES = 12;

export type RunnerLast = Readonly<{ id: string; at: string; host: string | null }>;

export type RunnerMachine =
  | Readonly<{ kind: 'unknown' }>
  | Readonly<{ kind: 'idle'; last: RunnerLast | null }>
  | Readonly<{ kind: 'requested'; id: string; at: string | null }>
  | Readonly<{ kind: 'running'; id: string; since: string | null; host: string | null }>
  | Readonly<{ kind: 'failed'; id: string; at: string | null; reason: string }>;

const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);

const md = (j: JobLite): Record<string, unknown> => j.metadata ?? {};

const stepBySlug = (j: JobLite, slug: string): StepLite | null =>
  (j.steps ?? []).find(s => s.spec_slug === slug) ?? (j.steps ?? []).find(s => s.title === slug) ?? null;

const openedAt = (j: JobLite): string | null => str(md(j).opened_at) ?? str(j.opened_on);
const closedAt = (j: JobLite): string | null => str(md(j).closed_at);

const ms = (s: string | null): number => {
  const n = s ? Date.parse(s) : Number.NaN;
  return Number.isNaN(n) ? Number.NEGATIVE_INFINITY : n;
};

/** The reason a converge failed, from the execute step's own record:
 *  a refusal names the allowlist unless the runner wrote a reason; a
 *  non-zero exit carries its code and the first line of output. */
function failure(execute: StepLite | null): string | null {
  const m = execute?.metadata ?? {};
  const output = (str(m.output) ?? '').split('\n')[0]?.trim() ?? '';
  if (m.disposition === 'refused') return output !== '' ? output : 'refused — outside the allowlist';
  const code = str(m.exit_code);
  if (code !== null && code !== '0') return output !== '' ? `exit ${code} — ${output}` : `exit ${code}`;
  return null;
}

/** The shed, from the ops-request rows the page fetched (null before
 *  the first read). Only `verb = converge` packets count; the newest by
 *  `opened_at` is the reading. */
export function runnerMachine(rows: readonly JobLite[] | null, nowMs: number): RunnerMachine {
  if (rows === null) return { kind: 'unknown' };
  const converge = rows
    .filter(j => md(j).verb === 'converge')
    .sort((a, b) => ms(openedAt(b)) - ms(openedAt(a)));
  const j = converge[0];
  if (!j) return { kind: 'idle', last: null };
  const execute = stepBySlug(j, 'execute');
  if (j.status === 'open') {
    return execute?.status === 'active'
      ? { kind: 'running', id: j.id, since: openedAt(j), host: null }
      : { kind: 'requested', id: j.id, at: openedAt(j) };
  }
  const at = closedAt(j);
  const reason = failure(execute);
  if (reason !== null) return { kind: 'failed', id: j.id, at, reason };
  const host = str(execute?.metadata?.runner_host);
  const answered = execute?.metadata?.disposition === 'answered';
  const startedMs = ms(at);
  if (answered && nowMs - startedMs < CONVERGE_USUAL_MINUTES * 60_000) {
    return { kind: 'running', id: j.id, since: at, host };
  }
  return { kind: 'idle', last: at !== null ? { id: j.id, at, host } : null };
}

/** The packet behind the shed's reading, for the entity panel; null
 *  when there is none. */
export function runnerPacketId(r: RunnerMachine): string | null {
  switch (r.kind) {
    case 'unknown':
      return null;
    case 'idle':
      return r.last?.id ?? null;
    default:
      return r.id;
  }
}

/** The shed's one line — short enough for the map's 130px shed; the
 *  entity panel carries the rest. */
export function runnerLabel(r: RunnerMachine, nowMs: number): string {
  switch (r.kind) {
    case 'unknown':
      return 'no reading';
    case 'idle': {
      const clock = clockText(r.last?.at ?? null);
      return clock !== null ? `idle · last ${clock}` : 'idle · no converge on record';
    }
    case 'requested': {
      const ago = elapsedText(r.at, nowMs);
      return ago !== null ? `requested · ${ago} ago` : 'requested';
    }
    case 'running': {
      const clock = clockText(r.since);
      const for_ = elapsedText(r.since, nowMs);
      return clock !== null ? `started ${clock}${for_ !== null ? ` · ${for_}` : ''}` : 'converging';
    }
    case 'failed':
      return `FAILED · ${r.reason}`;
  }
}

/** Elapsed over the usual duration, 0–1 — the shed's bar while it runs. */
export function runnerProgress(r: RunnerMachine, nowMs: number): number {
  if (r.kind !== 'running' || r.since === null) return 0;
  const started = Date.parse(r.since);
  if (Number.isNaN(started)) return 0;
  return Math.min(Math.max(nowMs - started, 0) / (CONVERGE_USUAL_MINUTES * 60_000), 1);
}

/** How many ops-requests the page reads. Three readers share this one
 *  fetch — the deploy-runner shed (newest converge), the signals panel,
 *  and the inspection shed's per-car `run-car-probe` lookup. The arrival
 *  rule files ONE request per probed car, so an arriving six-car train
 *  fills six rows at once and a window of twenty covered barely three
 *  trains; sixty keeps a day of probe runs findable. Still a window, not
 *  a filter: the shed says "no probe run in the packets read" rather
 *  than claiming none was filed. */
const OPS_REQUEST_WINDOW = 60;

export async function fetchOpsRequests(): Promise<readonly JobLite[] | null> {
  try {
    const r = await fetch(`/api/jobs?kind=ops-request&limit=${OPS_REQUEST_WINDOW}`);
    if (!r.ok) return null;
    const body = (await r.json()) as { data?: JobLite[] };
    return Array.isArray(body.data) ? body.data : null;
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------------
// The cluster tower.
// ---------------------------------------------------------------------

export type ClusterMachine =
  | Readonly<{ kind: 'unknown' }>
  | Readonly<{ kind: 'ready'; commit: string | null; since: string }>
  | Readonly<{ kind: 'dark'; since: string; error: string }>;

export type HealthResult =
  | Readonly<{ ok: true; commit: string | null }>
  | Readonly<{ ok: false; error: string }>;

/** The next tower reading from the previous one and one health probe.
 *  `since` is when the current state began: kept while the state (and
 *  the build) is unchanged, restarted when it changes. */
export function clusterReading(prev: ClusterMachine, result: HealthResult, nowMs: number): ClusterMachine {
  const now = new Date(nowMs).toISOString();
  if (result.ok) {
    if (prev.kind === 'ready' && prev.commit === result.commit) return prev;
    return { kind: 'ready', commit: result.commit, since: now };
  }
  return { kind: 'dark', since: prev.kind === 'dark' ? prev.since : now, error: result.error };
}

export function clusterLabel(c: ClusterMachine): string {
  switch (c.kind) {
    case 'unknown':
      return 'no reading';
    case 'ready':
      return c.commit !== null ? `ready · ${c.commit.slice(0, 7)}` : 'ready · no build reported';
    case 'dark':
      return 'DARK';
  }
}

/** One probe of the jobs API's health, from this browser: the build
 *  the running binary reports, or why it did not answer. */
export async function fetchHealth(): Promise<HealthResult> {
  try {
    const r = await fetch('/api/jobs/health');
    if (!r.ok) return { ok: false, error: `HTTP ${r.status}` };
    const body = (await r.json()) as { capabilities?: { commit?: unknown } };
    return { ok: true, commit: str(body.capabilities?.commit) };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}
