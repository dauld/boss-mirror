import { describe, expect, test } from 'bun:test';
import type { CarRow, TrainRow, YardState } from './yard';
import { scene, type Feeds } from './yard-floor';
import type { YardStatus } from './yard-status';
import { OPS_RUNNER_STUCK_MINUTES, yardAlerts } from './yard-alerts';

// The alerts strip is derived from the floor the page already holds —
// no new endpoint. Every alert is a button to its subject, so each one
// carries the selection key the map speaks. Errors first, then newest.

const NOW = Date.parse('2026-09-08T02:35:20Z');
const NOW_ISO = '2026-09-08T02:35:20.000Z';

const yardOf = (over: Partial<YardState> = {}): YardState => ({
  inFlight: [],
  dock: [],
  dockStation: { source: 'derived' },
  arrivals: [],
  cancelled: [],
  delivery: [],
  awaitingProof: [],
  publishing: [],
  cars: [],
  packets: { trains: [], gateRuns: [] },
  ...over,
});

const statusOf = (over: Partial<YardStatus> = {}): YardStatus => ({
  trains: [],
  dock: [],
  boarding: {
    dock_threshold: 1,
    cooldown_minutes: 45,
    at_times: [],
    dock_depth: 0,
    threshold_met: false,
    summary: '',
    held_because: null,
    cooldown_remaining_minutes: null,
    last_board_at: null,
    next_board: null,
  },
  recent: [],
  stranded: [],
  held: [],
  held_cars: [],
  gates: { capacity: 3, active: [], queued: [], typical_seconds: null },
  garage: [],
  limbo: [],
  policy: { stall_hours: 2, max_red_trains: 2 },
  conductor: null,
  now: NOW_ISO,
  ...over,
});

const car = (id: string, branch: string): CarRow => ({
  id,
  kind: 'ship-a-change',
  branch,
  title: `Car ${id}`,
  tags: [],
  sim: false,
  skipReason: null,
  head: 'abc1234',
});

const trainRow = (id: string, over: Partial<TrainRow> = {}): TrainRow => ({
  id,
  title: 'PR train 2026-09-08 02:02',
  prUrl: 'http://10.20.0.15:3000/david/boss/pulls/262',
  status: 'DEPARTED',
  lamp: 'green',
  mergeRef: null,
  deployed: null,
  convergingSince: null,
  cars: [],
  live: true,
  outcome: 'unknown',
  arrivedAt: { ms: 0, at: '', basis: 'opened_on' },
  eta: { kind: 'phase', phase: 'deploying' },
  trouble: null,
  cancelRequested: null,
  cancelRefused: false,
  ...over,
});

/** The server's verdict-lane rows the approach is drawn from. */
const strandedGreen = (branch: string, packet_id: string, since = '2026-09-08') => ({
  branch, packet_id, sha: null, since,
});
const garaged = (branch: string, failed_check: string | null, since: string, packet_id = `g-${branch}`) => ({
  branch, failed_check, since, packet_id, sha: null,
});

const quiet: Feeds = {
  runner: { kind: 'idle', last: null },
  cluster: { kind: 'ready', commit: '0d8c37d', since: NOW_ISO },
};
const alertsOf = (yard: YardState, status: YardStatus | null, feeds: Feeds = quiet) =>
  yardAlerts(scene(yard, status, NOW, feeds), status, NOW);

describe('yardAlerts — what is wrong right now, each a button to its subject', () => {
  test('a quiet floor has no alerts — every machine working or idle by design', () => {
    expect(alertsOf(yardOf(), statusOf())).toEqual([]);
    // Before any reading at all there is nothing to say either: unknown
    // is not an alarm.
    expect(alertsOf(yardOf(), null, { runner: { kind: 'unknown' }, cluster: { kind: 'unknown' } })).toEqual([]);
  });

  test("a blocked train is an error, since the server's block, selecting the train", () => {
    const a = alertsOf(
      yardOf({ inFlight: [trainRow('t1', { status: 'CONVERGING' })] }),
      statusOf({
        trains: [
          {
            id: 't1',
            title: 'PR train 2026-09-08 02:02',
            phase: 'converging',
            at_step: 'converged',
            block: { kind: 'stalled', since: '2026-09-08T00:30:00Z' },
            ci_result: 'green',
            pr_url: null,
            car_count: 1,
            boarded_at: null,
            eta: { kind: 'unknown', reason: 'not under test' },
          },
        ],
      }),
    );
    expect(a).toHaveLength(1);
    expect(a[0]).toMatchObject({ subject: 'train:t1', sev: 'err', since: '2026-09-08T00:30:00Z' });
    expect(a[0]?.text).toBe('#262 · STALLED — no step completed inside the policy window');
  });

  test("the board's trouble stands in when the server sent no block, with no since invented", () => {
    const a = alertsOf(
      yardOf({ inFlight: [trainRow('t1', { status: 'BOARDED', lamp: 'failing', trouble: { kind: 'ci-red' } })] }),
      statusOf(),
    );
    expect(a[0]).toMatchObject({ subject: 'train:t1', sev: 'err', since: null });
    expect(a[0]?.text).toContain('CI RED');
  });

  test('a silent conductor is an error, since it was last seen', () => {
    const a = alertsOf(
      yardOf(),
      statusOf({
        conductor: {
          last_seen: '2026-09-08T01:00:00Z',
          silent_for_minutes: 95,
          expected_every_minutes: 10,
          silent: true,
          last_verb: 'reconcile',
          last_rc: 0,
        },
      }),
    );
    expect(a[0]).toMatchObject({ subject: 'conductor', sev: 'err', since: '2026-09-08T01:00:00Z' });
    expect(a[0]?.text).toBe('conductor SILENT · 95m since it last fired');
  });

  test('a dark cluster is an error — the system of record does not answer from outside', () => {
    const a = alertsOf(yardOf(), statusOf(), {
      ...quiet,
      cluster: { kind: 'dark', since: '2026-09-08T02:30:00Z', error: 'HTTP 503' },
    });
    expect(a[0]).toMatchObject({ subject: 'cluster', sev: 'err', since: '2026-09-08T02:30:00Z' });
    expect(a[0]?.text).toBe('cluster DARK — /api/jobs/health does not answer (HTTP 503)');
  });

  test('a stale gate warns, since the gate started, selecting its bay', () => {
    const a = alertsOf(
      yardOf(),
      statusOf({
        gates: { capacity: 3, active: [{ branch: 'feat/slow', packet_id: 'g1', since: '2026-09-08T02:00:00Z', stale: true }], queued: [], typical_seconds: null },
      }),
    );
    expect(a[0]).toMatchObject({ subject: 'bay:0', sev: 'warn', since: '2026-09-08T02:00:00Z' });
    expect(a[0]?.text).toBe(
      "gate bay 1 · feat/slow STALE — past the runner's usual; the verdict may never reach the packet; re-gate",
    );
  });

  test('each garaged car warns with the check that failed, selecting the garage', () => {
    const a = alertsOf(
      yardOf({ cars: [car('c1', 'fix/red')] }),
      statusOf({
        garage: [
          garaged('fix/red', 'clippy, test', '2026-09-08T01:00:00Z'),
          garaged('fix/old', null, '2026-09-01'),
        ],
      }),
    );
    expect(a.map(x => [x.subject, x.sev, x.text, x.since])).toEqual([
      ['garage', 'warn', 'gate red: fix/red (clippy, test) — car garaged, rework', '2026-09-08T01:00:00Z'],
      ['garage', 'warn', 'gate red: fix/old (run died outside a check) — car garaged, rework', '2026-09-01'],
    ]);
    expect(new Set(a.map(x => x.id)).size).toBe(2);
  });

  test('every stranded green the server names warns, selecting the approach', () => {
    // The approach siding IS `status.stranded` now — at any age — so a
    // branch gated weeks ago warns beside one gated this morning, and
    // the alert and the wagon cannot disagree about which exist.
    const a = alertsOf(
      yardOf(),
      statusOf({ stranded: [strandedGreen('feat/stranded', 's1'), strandedGreen('feat/aged-out', 's2')] }),
    );
    // In the server's order (it sorts the lane by branch); same `since`,
    // so nothing re-orders them here.
    expect(a.map(x => [x.subject, x.sev, x.text])).toEqual([
      ['approach', 'warn', 'stranded green: feat/stranded — gated, never parked; rebase + re-gate'],
      ['approach', 'warn', 'stranded green: feat/aged-out — gated, never parked; rebase + re-gate'],
    ]);
  });

  test('a failed converge warns, selecting the runner; a request the runner has not answered in time warns too', () => {
    const failed = alertsOf(yardOf(), statusOf(), {
      ...quiet,
      runner: { kind: 'failed', id: 'r1', at: '2026-09-08T02:30:40Z', reason: 'refused — outside the allowlist' },
    });
    expect(failed[0]).toMatchObject({ subject: 'runner', sev: 'warn', since: '2026-09-08T02:30:40Z' });
    expect(failed[0]?.text).toBe('deploy runner · converge FAILED — refused — outside the allowlist');
    const stuckAt = new Date(NOW - (OPS_RUNNER_STUCK_MINUTES + 1) * 60_000).toISOString();
    const stuck = alertsOf(yardOf(), statusOf(), { ...quiet, runner: { kind: 'requested', id: 'o1', at: stuckAt } });
    expect(stuck[0]).toMatchObject({ subject: 'runner', sev: 'warn', since: stuckAt });
    expect(stuck[0]?.text).toBe(
      'deploy runner · converge requested 6m ago and not answered — the ops-runner on forge polls every minute',
    );
    // A fresh request is the protocol working.
    const fresh = alertsOf(yardOf(), statusOf(), {
      ...quiet,
      runner: { kind: 'requested', id: 'o1', at: '2026-09-08T02:34:50Z' },
    });
    expect(fresh).toEqual([]);
  });

  test('errors first, then newest; an alert with no since sorts last among its severity', () => {
    const a = alertsOf(
      yardOf({
        inFlight: [trainRow('t1', { status: 'BOARDED', lamp: 'failing', trouble: { kind: 'ci-red' } })],
      }),
      statusOf({
        stranded: [strandedGreen('feat/stranded', 's1')],
        gates: { capacity: 3, active: [{ branch: 'feat/slow', packet_id: 'g1', since: '2026-09-08T02:00:00Z', stale: true }], queued: [], typical_seconds: null },
        garage: [garaged('fix/red', 'test', '2026-09-08T02:10:00Z')],
      }),
      { ...quiet, cluster: { kind: 'dark', since: '2026-09-08T02:30:00Z', error: 'fetch failed' } },
    );
    expect(a.map(x => x.subject)).toEqual(['cluster', 'train:t1', 'garage', 'bay:0', 'approach']);
  });
});
