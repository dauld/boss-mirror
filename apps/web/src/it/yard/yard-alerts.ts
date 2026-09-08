// The alerts strip — what is wrong on the floor right now, derived
// from the scene the page already holds. No new endpoint: every alert
// is a fact one of the read models states (a block the conductor
// recorded, a gate the server flagged stale, a car the server garaged,
// a stranded green, a silent conductor) or one the page read from
// outside (a dark tower, a failed or unanswered converge). Each alert
// is a button to its subject: `subject` is the selection key the map,
// the board and the entity panel speak. Errors first, then newest.

import { prNumber, type Scene } from './yard-floor';
import { blockLabel, elapsedText, type YardStatus } from './yard-status';

export type Alert = Readonly<{
  /** Unique on the strip (several alerts may share a subject). */
  id: string;
  /** The selection key clicking it makes. */
  subject: string;
  sev: 'err' | 'warn';
  text: string;
  /** An RFC3339 instant or a bare date, whichever the record carries. */
  since: string | null;
}>;

/** The ops-runner on the forge polls its host's open packets every
 *  minute (infra/ops/boss-ops-runner.timer, OnUnitActiveSec=1min). A
 *  converge request still unanswered after this many minutes has
 *  missed several polls: the runner is not running, and the shed must
 *  say so rather than wait. */
export const OPS_RUNNER_STUCK_MINUTES = 5;

const sinceMs = (s: string | null): number => {
  const n = s ? Date.parse(s) : Number.NaN;
  return Number.isNaN(n) ? Number.NEGATIVE_INFINITY : n;
};

export function yardAlerts(s: Scene, status: YardStatus | null, nowMs: number): readonly Alert[] {
  const serverTrain = new Map((status?.trains ?? []).map(t => [t.id, t]));
  const trains: Alert[] = s.locos.flatMap(l => {
    if (l.blocked === null) return [];
    const b = serverTrain.get(l.id)?.block ?? null;
    const since = b && (b.kind === 'deploy-blocked' || b.kind === 'stalled') ? b.since : null;
    const name = l.n !== null ? `#${l.n}` : l.title;
    return [{ id: `train:${l.id}`, subject: `train:${l.id}`, sev: 'err', text: `${name} · ${l.blocked}`, since }];
  });

  const c = s.machines.conductor;
  const conductor: Alert[] = c.silent
    ? [{ id: 'conductor', subject: 'conductor', sev: 'err', text: `conductor ${c.label}`, since: c.lastSeen }]
    : [];

  const cl = s.machines.cluster;
  const cluster: Alert[] =
    cl.kind === 'dark'
      ? [{ id: 'cluster', subject: 'cluster', sev: 'err', text: `cluster DARK — /api/jobs/health does not answer (${cl.error})`, since: cl.since }]
      : [];

  const bays: Alert[] = s.bays.flatMap(b =>
    b.stale && b.branch !== null
      ? [{
          id: `bay:${b.index}`,
          subject: `bay:${b.index}`,
          sev: 'warn',
          text: `gate bay ${b.index + 1} · ${b.branch} STALE — past the runner's usual; the verdict may never reach the packet; re-gate`,
          since: b.since,
        }]
      : [],
  );

  // The garage, one alert per car, in the server's words for the check.
  const garageByBranch = new Map((status?.garage ?? []).map(g => [g.branch, g]));
  const garage: Alert[] = s.wagons
    .filter(w => w.station === 'garage')
    .map(w => ({
      id: `garage:${w.id}`,
      subject: 'garage',
      sev: 'warn',
      text: `gate red: ${w.branch} (${garageByBranch.get(w.branch)?.failed_check ?? 'run died outside a check'}) — car garaged, rework`,
      since: w.since,
    }));

  // Stranded greens: the wagons on the approach that say so, plus any
  // branch the server strands that the approach window no longer lists.
  const strandedWagons = s.wagons.filter(w => w.station === 'approach' && w.status.startsWith('stranded'));
  const seen = new Set(strandedWagons.map(w => w.branch));
  const stranded: Alert[] = [
    ...strandedWagons.map(w => ({ id: `stranded:${w.id}`, branch: w.branch, since: w.since })),
    ...(status?.stranded ?? []).filter(x => !seen.has(x.branch)).map(x => ({ id: `stranded:${x.branch}`, branch: x.branch, since: null })),
  ].map(x => ({
    id: x.id,
    subject: 'approach',
    sev: 'warn' as const,
    text: `stranded green: ${x.branch} — gated, never parked; rebase + re-gate`,
    since: x.since,
  }));

  const r = s.machines.runner;
  const runner: Alert[] =
    r.kind === 'failed'
      ? [{ id: 'runner', subject: 'runner', sev: 'warn', text: `deploy runner · converge FAILED — ${r.reason}`, since: r.at }]
      : r.kind === 'requested' && nowMs - sinceMs(r.at) > OPS_RUNNER_STUCK_MINUTES * 60_000
        ? [{
            id: 'runner',
            subject: 'runner',
            sev: 'warn',
            text: `deploy runner · converge requested ${elapsedText(r.at, nowMs) ?? '—'} ago and not answered — the ops-runner on forge polls every minute`,
            since: r.at,
          }]
        : [];

  return [...cluster, ...trains, ...conductor, ...bays, ...garage, ...stranded, ...runner].sort(
    (a, b) => (a.sev === 'err' ? 0 : 1) - (b.sev === 'err' ? 0 : 1) || sinceMs(b.since) - sinceMs(a.since),
  );
}

/** The train name the alerts and the board share. */
export const trainName = (n: number | null, title: string): string => (n !== null ? `#${n}` : title);
export { prNumber };
