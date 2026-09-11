import { describe, expect, test } from 'bun:test';
import type { JobLite, StepLite } from './yard';
import { SIGNALS_SHOWN, yardSignals } from './yard-signals';

// The signals panel: what fired what, newest first, read off the
// packets' own completed steps. Who is derived from the packet kind
// (a pr-train step is the conductor's, a gate-run the gate-runner's,
// an ops-request the dispatcher's request and the ops-runner's
// answer). A step the record says was completed BY HAND is rendered
// as such — the one signal the page must never soften.

const step = (
  spec_slug: string,
  status: string,
  metadata: Record<string, unknown> = {},
  kind = 'task',
): StepLite & { kind: string } => ({
  spec_slug,
  title: spec_slug,
  status,
  metadata,
  kind,
});

const train = (id: string, over: Partial<JobLite> = {}): JobLite => ({
  id,
  kind: 'pr-train',
  title: 'PR train 2026-09-08 02:02',
  status: 'closed',
  opened_on: '2026-09-08',
  metadata: { closed_at: '2026-09-08T02:30:25.263+00:00', outcome: 'arrived' },
  steps: [
    step('scheduled', 'completed', {}, 'trigger'),
    step('collect', 'completed', { boarded: 'feat/a (2fc7e385), fix/b (04cdfbd0)', completed_at: '2026-09-08T02:03:05Z' }),
    step('pr', 'completed', { pr_url: 'http://10.20.0.15:3000/david/boss/pulls/262', completed_at: '2026-09-08T02:03:05Z' }),
    step('ci', 'completed', { result: 'green', completed_at: '2026-09-08T02:20:55Z' }),
    step('merged', 'completed', { merge_ref: '0d8c37dd9cf9', completed_at: '2026-09-08T02:20:56Z' }),
    step('converged', 'completed', {
      cluster_commit: '0d8c37dd9cf90c617c0fc932c5f4d24c18ceb4ee',
      completed_at: '2026-09-08T02:30:24Z',
    }),
    step('arrived', 'completed', {}, 'outcome'),
    step('cancelled', 'skipped', {}, 'outcome'),
  ],
  ...over,
});

const gateRun = (id: string, branch: string, verdict: string, closedAt: string, fails: string[] = []): JobLite => ({
  id,
  kind: 'gate-run',
  title: `Gate: ${branch}`,
  status: 'closed',
  opened_on: '2026-09-08',
  metadata: { branch, opened_at: '2026-09-08T01:50:46Z', closed_at: closedAt },
  steps: [
    step('launched', 'completed', {}, 'trigger'),
    step(
      'record-verdict',
      'completed',
      { verdict, receipt: JSON.stringify({ verdict, head: '1af0a1d2e0e8', fails }) },
      'gate-verdict',
    ),
    step('green', verdict === 'green' ? 'completed' : 'skipped', {}, 'outcome'),
  ],
});

const opsRequest = (id: string, over: Partial<JobLite> = {}): JobLite => ({
  id,
  kind: 'ops-request',
  title: 'converge on forge — a train merged to main',
  status: 'closed',
  opened_on: '2026-09-08',
  tags: ['dispatcher-spawned'],
  metadata: {
    host: 'forge',
    verb: 'converge',
    opened_at: '2026-09-08T02:20:56.2Z',
    closed_at: '2026-09-08T02:21:35.5Z',
    outcome: 'answered',
    spawned_by_rule: 'converge-on-merge',
  },
  steps: [
    step('filed', 'completed', {}, 'trigger'),
    step('execute', 'completed', { disposition: 'answered', exit_code: '0', output: '', runner_host: 'forge' }),
    step('answered', 'completed', {}, 'outcome'),
  ],
  ...over,
});

describe("yardSignals — what fired what, in the packets' own stamps", () => {
  test("a train's stamped steps are the conductor's signals, newest first; the arrival takes the packet's close", () => {
    const s = yardSignals([train('t1')], [], []);
    expect(s.map(x => [x.at, x.who, x.what])).toEqual([
      ['2026-09-08T02:30:25.263+00:00', 'conductor', '#262 arrived'],
      ['2026-09-08T02:30:24Z', 'conductor', '#262 converged · cluster on 0d8c37d'],
      ['2026-09-08T02:20:56Z', 'conductor', '#262 merged → main at 0d8c37d'],
      ['2026-09-08T02:20:55Z', 'conductor', '#262 CI green'],
      ['2026-09-08T02:03:05Z', 'conductor', '#262 PR opened'],
      ['2026-09-08T02:03:05Z', 'conductor', '#262 boarded 2 cars'],
    ]);
    expect(s.every(x => x.packetId === 't1' && !x.hand && x.sev === 'ok')).toBe(true);
    // The unstamped trigger is not a signal: no instant, no row.
    expect(s.some(x => x.what.includes('scheduled'))).toBe(false);
  });

  test('a step completed by hand says so, in the warn tone, with the hand reason', () => {
    const t = train('t2', {
      title: 'PR train 2026-09-07 21:41',
      steps: [
        step('pr', 'completed', { pr_url: 'http://10.20.0.15:3000/david/boss/pulls/257', completed_at: '2026-09-07T21:41:30Z' }),
        step('converged', 'completed', {
          cluster_commit: '77152b8 (rolled back; 1994077 quarantined at boot)',
          completed_by_hand: true,
          hand_reason: 'track deadlock — see verified',
          completed_at: '2026-09-07T22:51:30Z',
        }),
      ],
    });
    const s = yardSignals([t], [], []);
    expect(s[0]).toMatchObject({
      who: 'conductor',
      hand: true,
      sev: 'warn',
      what: '#257 converged by hand — track deadlock — see verified',
    });
  });

  test('a red CI and a cancellation read as trouble; a cancelled train names its reason', () => {
    const t = train('t3', {
      metadata: { closed_at: '2026-09-08T01:46:42Z', outcome: 'cancelled' },
      steps: [
        step('pr', 'completed', { pr_url: 'http://10.20.0.15:3000/david/boss/pulls/261', completed_at: '2026-09-08T01:17:59Z' }),
        step('ci', 'completed', {
          result: 'failing',
          checks: 'CI / test (pull_request):FAILURE',
          completed_at: '2026-09-08T01:30:00Z',
        }),
        step('cancelled', 'completed', { reason: 'Twin car: already landed in #259', completed_at: '2026-09-08T01:46:42Z' }, 'outcome'),
      ],
    });
    const s = yardSignals([t], [], []);
    expect(s.map(x => [x.sev, x.what])).toEqual([
      ['warn', '#261 cancelled — Twin car: already landed in #259'],
      ['err', '#261 CI failing · CI / test (pull_request):FAILURE'],
      ['ok', '#261 PR opened'],
    ]);
  });

  test("a gate-run's verdict is the gate-runner's signal at the packet's close, naming what failed", () => {
    const s = yardSignals(
      [],
      [gateRun('g1', 'feat/x', 'green', '2026-09-08T02:01:00Z'), gateRun('g2', 'fix/y', 'failed', '2026-09-08T00:06:03Z', ['clippy', 'test'])],
      [],
    );
    expect(s.map(x => [x.at, x.who, x.sev, x.what, x.packetId])).toEqual([
      ['2026-09-08T02:01:00Z', 'gate-runner', 'ok', 'gate feat/x green', 'g1'],
      ['2026-09-08T00:06:03Z', 'gate-runner', 'err', 'gate fix/y failed · clippy, test', 'g2'],
    ]);
  });

  // The runner now reports gate.sh's WHOLE receipt rather than a
  // four-field digest of it, so what failed is read off `checks` — the
  // array that also carries each check's duration. Old receipts, on
  // every landed car, still carry the derived `fails` and must keep
  // reading; the test above is that half.
  test('a wide receipt names what failed from its `checks`', () => {
    const wide = gateRun('g3', 'fix/z', 'failed', '2026-09-08T00:06:03Z');
    const verdictStep = wide.steps?.[1];
    if (verdictStep) {
      (verdictStep.metadata as Record<string, unknown>).receipt = JSON.stringify({
        verdict: 'failed', head: '1af0a1d2e0e8', mode: 'full', dirty: false,
        checks: [
          { name: 'fmt', result: 'pass', seconds: 3 },
          { name: 'clippy', result: 'fail', seconds: 44 },
          { name: 'test', result: 'fail', seconds: 812 },
        ],
      });
    }
    expect(yardSignals([], [wide], []).map(x => x.what)).toEqual([
      'gate fix/z failed · clippy, test',
    ]);
  });

  test("a converge ops-request is the dispatcher's request and the ops-runner's answer", () => {
    const s = yardSignals([], [], [opsRequest('o1')]);
    expect(s.map(x => [x.at, x.who, x.sev, x.what])).toEqual([
      ['2026-09-08T02:21:35.5Z', 'ops-runner', 'ok', 'converge answered on forge · exit 0'],
      ['2026-09-08T02:20:56.2Z', 'dispatcher', 'ok', 'converge requested on forge — rule converge-on-merge'],
    ]);
    // Filed by an operator, not a rule: the requester is the operator.
    const byHand = opsRequest('o2', {
      tags: [],
      metadata: {
        host: 'forge',
        verb: 'journal-tail',
        opened_at: '2026-09-07T22:22:15Z',
        closed_at: '2026-09-07T22:22:29Z',
        outcome: 'answered',
      },
    });
    expect(yardSignals([], [], [byHand]).map(x => [x.who, x.what])).toEqual([
      ['ops-runner', 'journal-tail answered on forge · exit 0'],
      ['operator', 'journal-tail requested on forge'],
    ]);
    // A refusal reads as trouble.
    const refused = opsRequest('o3', {
      metadata: {
        host: 'forge',
        verb: 'converge',
        opened_at: '2026-09-08T02:20:56Z',
        closed_at: '2026-09-08T02:21:35Z',
        outcome: 'refused',
      },
      steps: [step('execute', 'completed', { disposition: 'refused', output: 'verb not allowlisted' })],
    });
    expect(yardSignals([], [], [refused])[0]).toMatchObject({
      who: 'ops-runner',
      sev: 'err',
      what: 'converge refused on forge — verb not allowlisted',
    });
  });

  test('all sources interleave by instant, newest first, capped at the panel size', () => {
    const many = Array.from({ length: SIGNALS_SHOWN + 5 }, (_, i) =>
      gateRun(`g${i}`, `feat/${i}`, 'green', `2026-09-08T01:${String(i).padStart(2, '0')}:00Z`),
    );
    const s = yardSignals([train('t1')], many, [opsRequest('o1')]);
    expect(s).toHaveLength(SIGNALS_SHOWN);
    const ms = s.map(x => Date.parse(x.at));
    expect([...ms].sort((a, b) => b - a)).toEqual(ms);
    expect(s[0]?.what).toBe('#262 arrived');
    expect(new Set(s.map(x => x.id)).size).toBe(SIGNALS_SHOWN);
  });

  test('a packet with no instants anywhere contributes nothing rather than a fabricated time', () => {
    const bare = train('t9', { metadata: {}, steps: [step('merged', 'completed', { merge_ref: 'abc' })] });
    expect(yardSignals([bare], [], [])).toEqual([]);
  });
});
