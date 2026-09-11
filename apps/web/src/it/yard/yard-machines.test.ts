import { describe, expect, test } from 'bun:test';
import type { JobLite } from './yard';
import {
  CONVERGE_USUAL_MINUTES,
  clusterLabel,
  clusterReading,
  runnerLabel,
  runnerMachine,
  runnerPacketId,
  type ClusterMachine,
} from './yard-machines';

// The two machines Car 1 left dark: the deploy-runner shed, read off
// the converge ops-request packets the dispatcher files on a merge and
// the forge's ops-runner answers; and the cluster tower, read off
// /api/jobs/health from outside. Every state below is one a live
// packet has been in (rows read 2026-09-08).

const NOW = Date.parse('2026-09-08T02:35:20Z');
const NOW_ISO = '2026-09-08T02:35:20.000Z';

// An ops-request as the dispatcher files it and the runner answers it.
// No STEP carries an instant — only the day-granular completed_on —
// so the packet's own metadata.opened_at / closed_at are the clocks.
const ops = (
  id: string,
  status: 'open' | 'closed',
  md: Record<string, unknown>,
  execute: Readonly<{ status: string; metadata?: Record<string, unknown> }>,
): JobLite => ({
  id,
  kind: 'ops-request',
  title: 'converge on forge — a train merged to main',
  status,
  opened_on: '2026-09-08',
  tags: ['dispatcher-spawned'],
  metadata: { host: 'forge', verb: 'converge', spawned_by_rule: 'converge-on-merge', ...md },
  steps: [
    { spec_slug: 'filed', title: 'Host read requested: converge on forge', status: 'completed', metadata: {} },
    { spec_slug: 'execute', title: 'Run converge on forge', status: execute.status, metadata: execute.metadata ?? {} },
    { spec_slug: 'answered', title: 'Answered', status: status === 'closed' ? 'completed' : 'pending', metadata: {} },
  ],
});

const answered = (id: string, openedAt: string, closedAt: string, exitCode = '0', output = ''): JobLite =>
  ops(
    id,
    'closed',
    { opened_at: openedAt, closed_at: closedAt, outcome: 'answered' },
    { status: 'completed', metadata: { disposition: 'answered', exit_code: exitCode, output, runner_host: 'forge' } },
  );

describe('runnerMachine — the deploy-runner shed, from the converge packets', () => {
  test('no rows is no reading, not idle', () => {
    expect(runnerMachine(null, NOW)).toEqual({ kind: 'unknown' });
    expect(runnerLabel({ kind: 'unknown' }, NOW)).toBe('no reading');
  });

  test('a window with no converge packet is idle with nothing on record; other verbs do not count', () => {
    const journal = ops(
      'j1',
      'closed',
      { verb: 'journal-tail', opened_at: '2026-09-08T02:30:00Z', closed_at: '2026-09-08T02:30:10Z' },
      { status: 'completed', metadata: { disposition: 'answered', exit_code: '0' } },
    );
    expect(runnerMachine([journal], NOW)).toEqual({ kind: 'idle', last: null });
    expect(runnerLabel({ kind: 'idle', last: null }, NOW)).toBe('idle · no converge on record');
  });

  test('an open packet the runner has not answered is requested, since the packet opened', () => {
    const open = ops('o1', 'open', { opened_at: '2026-09-08T02:33:20Z' }, { status: 'ready' });
    const m = runnerMachine([open], NOW);
    expect(m).toEqual({ kind: 'requested', id: 'o1', at: '2026-09-08T02:33:20Z' });
    expect(runnerLabel(m, NOW)).toBe('requested · 2m ago');
  });

  test('a claimed execute step is running with no host yet', () => {
    const active = ops('o2', 'open', { opened_at: '2026-09-08T02:34:50Z' }, { status: 'active' });
    expect(runnerMachine([active], NOW)).toEqual({ kind: 'running', id: 'o2', since: '2026-09-08T02:34:50Z', host: null });
  });

  test("an answered converge inside the runner's usual duration is running — the unit was started, the build is in flight", () => {
    const m = runnerMachine([answered('a1', '2026-09-08T02:31:56Z', '2026-09-08T02:32:35Z')], NOW);
    expect(m).toEqual({ kind: 'running', id: 'a1', since: '2026-09-08T02:32:35Z', host: 'forge' });
    expect(runnerLabel(m, NOW)).toBe('started 02:32 UTC · 3m');
  });

  test('past the usual duration the shed is idle, naming the last converge', () => {
    const closedAt = new Date(NOW - (CONVERGE_USUAL_MINUTES + 1) * 60_000).toISOString();
    const m = runnerMachine([answered('a2', '2026-09-08T02:20:56Z', closedAt)], NOW);
    expect(m).toEqual({ kind: 'idle', last: { id: 'a2', at: closedAt, host: 'forge' } });
    expect(runnerLabel(m, NOW)).toBe('idle · last 02:22 UTC');
  });

  test('a refusal is failed with the reason; an empty output names the allowlist', () => {
    const refused = ops(
      'r1',
      'closed',
      { opened_at: '2026-09-08T02:30:00Z', closed_at: '2026-09-08T02:30:40Z', outcome: 'refused' },
      { status: 'completed', metadata: { disposition: 'refused', output: '', runner_host: 'forge' } },
    );
    const m = runnerMachine([refused], NOW);
    expect(m).toEqual({ kind: 'failed', id: 'r1', at: '2026-09-08T02:30:40Z', reason: 'refused — outside the allowlist' });
    expect(runnerLabel(m, NOW)).toBe('FAILED · refused — outside the allowlist');
  });

  test('a non-zero exit is failed with the exit code and the first line of output', () => {
    const m = runnerMachine(
      [answered('x1', '2026-09-08T02:30:00Z', '2026-09-08T02:30:40Z', '1', 'Failed to start cluster-deploy-runner.service: Unit not found.\nmore')],
      NOW,
    );
    expect(m).toEqual({
      kind: 'failed',
      id: 'x1',
      at: '2026-09-08T02:30:40Z',
      reason: 'exit 1 — Failed to start cluster-deploy-runner.service: Unit not found.',
    });
  });

  test('the newest converge packet by opened_at wins, whatever order the rows arrive in', () => {
    const older = answered('a1', '2026-09-08T02:20:56Z', '2026-09-08T02:21:35Z');
    const newer = ops('o9', 'open', { opened_at: '2026-09-08T02:34:00Z' }, { status: 'ready' });
    expect(runnerMachine([older, newer], NOW).kind).toBe('requested');
    expect(runnerMachine([newer, older], NOW).kind).toBe('requested');
  });

  test('a closed packet with no verdict on record is idle, not invented', () => {
    const odd = ops('z1', 'closed', { opened_at: '2026-09-08T02:20:00Z', closed_at: '2026-09-08T02:21:00Z' }, { status: 'skipped' });
    expect(runnerMachine([odd], NOW)).toEqual({ kind: 'idle', last: { id: 'z1', at: '2026-09-08T02:21:00Z', host: null } });
  });

  test('the packet behind the reading, for the entity panel', () => {
    expect(runnerPacketId({ kind: 'unknown' })).toBeNull();
    expect(runnerPacketId({ kind: 'idle', last: null })).toBeNull();
    expect(runnerPacketId({ kind: 'idle', last: { id: 'a2', at: NOW_ISO, host: 'forge' } })).toBe('a2');
    expect(runnerPacketId({ kind: 'requested', id: 'o1', at: null })).toBe('o1');
    expect(runnerPacketId({ kind: 'running', id: 'a1', since: null, host: null })).toBe('a1');
    expect(runnerPacketId({ kind: 'failed', id: 'r1', at: null, reason: 'x' })).toBe('r1');
  });
});

describe('clusterReading — the tower, from /api/jobs/health read from outside', () => {
  const ready: ClusterMachine = {
    kind: 'ready',
    commit: '0d8c37dd9cf90c617c0fc932c5f4d24c18ceb4ee',
    since: '2026-09-08T02:30:00.000Z',
  };

  test('the first good answer lights the tower, since now', () => {
    expect(clusterReading({ kind: 'unknown' }, { ok: true, commit: '0d8c37dd9cf9' }, NOW)).toEqual({
      kind: 'ready',
      commit: '0d8c37dd9cf9',
      since: NOW_ISO,
    });
  });

  test('the same build keeps its since; a new build starts a new since', () => {
    expect(clusterReading(ready, { ok: true, commit: ready.commit }, NOW)).toEqual(ready);
    expect(clusterReading(ready, { ok: true, commit: 'bad0428adf2c' }, NOW)).toEqual({
      kind: 'ready',
      commit: 'bad0428adf2c',
      since: NOW_ISO,
    });
  });

  test('a failed read is DARK since the first failure, and stays dark on every failure after', () => {
    const dark = clusterReading(ready, { ok: false, error: 'HTTP 503' }, NOW);
    expect(dark).toEqual({ kind: 'dark', since: NOW_ISO, error: 'HTTP 503' });
    const later = clusterReading(dark, { ok: false, error: 'fetch failed' }, NOW + 60_000);
    expect(later).toEqual({ kind: 'dark', since: NOW_ISO, error: 'fetch failed' });
    // Back up: ready since the answer came back, not since it went dark.
    expect(clusterReading(later, { ok: true, commit: 'abc1234' }, NOW + 120_000)).toEqual({
      kind: 'ready',
      commit: 'abc1234',
      since: new Date(NOW + 120_000).toISOString(),
    });
  });

  test('an answer naming no build is ready, and says so rather than inventing one', () => {
    const m = clusterReading({ kind: 'unknown' }, { ok: true, commit: null }, NOW);
    expect(m).toEqual({ kind: 'ready', commit: null, since: NOW_ISO });
    expect(clusterLabel(m)).toBe('ready · no build reported');
  });

  test('the tower label: the short sha, or DARK, or no reading', () => {
    expect(clusterLabel({ kind: 'unknown' })).toBe('no reading');
    expect(clusterLabel(ready)).toBe('ready · 0d8c37d');
    expect(clusterLabel({ kind: 'dark', since: NOW_ISO, error: 'x' })).toBe('DARK');
  });
});
