import { describe, expect, test } from 'bun:test';
import {
  CONVERGE_SILENCE_MAX_S,
  CONVERGE_TIMER_MINUTES,
  JOURNAL_TAIL,
  convergeLabel,
  convergeRun,
  convergeSilence,
  stageRows,
  withConverge,
} from './yard-converge';
import type { RunnerMachine } from './yard-machines';
import type { JobLite, StepLite } from './yard';

// Every fixture below is a maintenance-cluster-converge packet as the
// forge's cluster-deploy-runner.service leaves it, read off the system
// of record on 2026-09-12 22:20 UTC: `boss-step.sh` (ExecStopPost)
// merges the run's summary file onto the `run` step, so the stage
// seconds are step metadata and every value is a string.

const runStep = (metadata: Record<string, unknown>, completed_at: string | null): StepLite => ({
  spec_slug: 'run',
  title: 'Run to completion',
  status: completed_at === null ? 'ready' : 'completed',
  metadata: { authority_role: 'platform-admin', ...metadata },
  completed_at,
});

const packet = (
  id: string,
  status: 'open' | 'closed',
  opened_at: string,
  closed_at: string | null,
  run: StepLite,
  outcome: 'completed' | 'failed' | null,
): JobLite => ({
  id,
  kind: 'maintenance-cluster-converge',
  title: 'Cluster converge on forge main — 2026-09-12',
  status,
  opened_on: '2026-09-12',
  metadata: {
    chore: 'maintenance-cluster-converge',
    opened_at,
    ...(closed_at !== null ? { closed_at } : {}),
    ...(outcome !== null ? { outcome } : {}),
  },
  steps: [
    { spec_slug: 'scheduled', title: 'Timer fired', status: 'completed', metadata: { trigger_kind: 'periodic' } },
    run,
    ...(outcome !== null
      ? [
          {
            spec_slug: outcome,
            title: outcome === 'completed' ? 'Maintenance completed' : 'Maintenance failed',
            status: 'completed',
            metadata: { outcome_kind: outcome === 'completed' ? 'completed' : 'aborted' },
            completed_at: closed_at,
          } satisfies StepLite,
        ]
      : []),
  ],
});

/** 7aedfcb2 — the tick that found forge main unchanged. */
const unchangedTick = packet(
  '7aedfcb2-0a23-43ea-a0ab-839e0a518696',
  'closed',
  '2026-09-12T22:20:10.047972020+00:00',
  '2026-09-12T22:20:10.487490848+00:00',
  runStep({ result: 'ok', unchanged: '845f44a' }, '2026-09-12T22:20:10.215219Z'),
  'completed',
);

/** c84e6d65 — the run that built 845f44a: 329 s build, 2 s push,
 *  21 s roll, 6 s verify. */
const fullRun = packet(
  'c84e6d65-c381-43ff-8024-088e01e25715',
  'closed',
  '2026-09-12T22:10:03.914688701+00:00',
  '2026-09-12T22:16:08.665964022+00:00',
  runStep(
    { result: 'ok', build_head: '845f44a', build_s: '329', push_s: '2', roll_s: '21', verify_s: '6' },
    '2026-09-12T22:16:08.432802Z',
  ),
  'completed',
);

/** A run in flight: boss-maintenance-wrap.sh (ExecStartPre) opened the
 *  packet and nothing has closed it. The stamps land only at
 *  ExecStopPost, so an open packet carries none. */
const inFlight = packet(
  '11111111-0000-0000-0000-000000000001',
  'open',
  '2026-09-12T22:30:04.000000000+00:00',
  null,
  runStep({}, null),
  null,
);

/** A run that died after its build: boss-step.sh records systemd's
 *  word for how (`result=exit-code`, `exit_status=1`) and the workflow's
 *  `failed` terminal takes it. What it had stamped before dying stays. */
const died = packet(
  '22222222-0000-0000-0000-000000000002',
  'closed',
  '2026-09-12T21:40:03.000000000+00:00',
  '2026-09-12T21:47:12.000000000+00:00',
  runStep(
    { result: 'exit-code', exit_status: '1', build_head: 'deadbee', build_s: '301', push_s: '3' },
    '2026-09-12T21:47:12.000000Z',
  ),
  'failed',
);

/** A run whose summary file was never written: boss-step.sh says so in
 *  so many words rather than leaving the step bare. */
const noSummary = packet(
  '33333333-0000-0000-0000-000000000003',
  'closed',
  '2026-09-12T21:30:03.000000000+00:00',
  '2026-09-12T21:30:40.000000000+00:00',
  runStep(
    { result: 'ok', summary_absent: '/home/david/.boss-cluster-converge.summary.json was not written by this run' },
    '2026-09-12T21:30:40.000000Z',
  ),
  'completed',
);

const T = (iso: string): number => Date.parse(iso);

describe('convergeRun', () => {
  test('no rows read is UNKNOWN — the record did not answer, which is not idle', () => {
    expect(convergeRun(null, T('2026-09-12T22:21:00Z'))).toEqual({ kind: 'unknown' });
  });

  test('an empty window is NONE', () => {
    expect(convergeRun([], T('2026-09-12T22:21:00Z'))).toEqual({ kind: 'none' });
  });

  test('an open packet is the run itself, running since its opened_at, with no stages yet', () => {
    const r = convergeRun([inFlight, unchangedTick, fullRun], T('2026-09-12T22:33:04Z'));
    expect(r.kind).toBe('running');
    if (r.kind !== 'running') return;
    expect(r.id).toBe(inFlight.id);
    expect(r.since).toBe('2026-09-12T22:30:04.000000000+00:00');
  });

  test('the newest closed packet is the last run, with its stage seconds as numbers', () => {
    const r = convergeRun([fullRun, unchangedTick], T('2026-09-12T22:21:00Z'));
    // Rows are served newest-first by the API but the reading must not
    // depend on that: unchangedTick opened after fullRun.
    expect(r.kind).toBe('ended');
    if (r.kind !== 'ended') return;
    expect(r.id).toBe(unchangedTick.id);
    expect(r.outcome).toBe('completed');
    expect(r.stages.unchanged).toBe('845f44a');
    expect(r.stages.buildS).toBeNull();
  });

  test('a full run carries build_head and all four stage durations', () => {
    const r = convergeRun([fullRun], T('2026-09-12T22:21:00Z'));
    expect(r.kind).toBe('ended');
    if (r.kind !== 'ended') return;
    expect(r.at).toBe('2026-09-12T22:16:08.665964022+00:00');
    expect(r.stages).toEqual({
      unchanged: null,
      buildHead: '845f44a',
      buildS: 329,
      pushS: 2,
      rollS: 21,
      verifyS: 6,
    });
    expect(r.summaryAbsent).toBeNull();
  });

  test('a run that died keeps the stamps it had and names how it died', () => {
    const r = convergeRun([died], T('2026-09-12T22:21:00Z'));
    expect(r.kind).toBe('ended');
    if (r.kind !== 'ended') return;
    expect(r.outcome).toBe('failed');
    expect(r.result).toBe('exit-code');
    expect(r.exitStatus).toBe('1');
    expect(r.stages.buildS).toBe(301);
    expect(r.stages.pushS).toBe(3);
    expect(r.stages.rollS).toBeNull();
  });

  test('a run that left no summary says so — not a bare ok', () => {
    const r = convergeRun([noSummary], T('2026-09-12T22:21:00Z'));
    expect(r.kind).toBe('ended');
    if (r.kind !== 'ended') return;
    expect(r.summaryAbsent).toMatch(/was not written by this run/);
  });

  test('a packet of another kind in the rows is ignored', () => {
    const stray: JobLite = { ...fullRun, id: 'stray', kind: 'ops-request' };
    const r = convergeRun([stray], T('2026-09-12T22:21:00Z'));
    expect(r).toEqual({ kind: 'none' });
  });
});

describe('stageRows', () => {
  test('a full run is four stages in the order the runner executes them', () => {
    const r = convergeRun([fullRun], T('2026-09-12T22:21:00Z'));
    if (r.kind !== 'ended') throw new Error('expected ended');
    expect(stageRows(r.stages)).toEqual([
      { stage: 'build', seconds: 329 },
      { stage: 'push', seconds: 2 },
      { stage: 'roll', seconds: 21 },
      { stage: 'verify', seconds: 6 },
    ]);
  });

  test('an unchanged tick has no stages — the head is the whole story', () => {
    const r = convergeRun([unchangedTick], T('2026-09-12T22:21:00Z'));
    if (r.kind !== 'ended') throw new Error('expected ended');
    expect(stageRows(r.stages)).toEqual([]);
  });

  test('a run that died shows the stages it reached and stops there', () => {
    const r = convergeRun([died], T('2026-09-12T22:21:00Z'));
    if (r.kind !== 'ended') throw new Error('expected ended');
    expect(stageRows(r.stages).map(s => s.stage)).toEqual(['build', 'push']);
  });
});

describe('convergeSilence', () => {
  // The timer fires every 10 minutes (cluster-deploy-runner.timer,
  // OnUnitActiveSec=10min), so a fresh packet arrives at least that
  // often while the forge is up. The threshold is the ONE number BOSS
  // already uses for a silent loop: 30 min, the convergence arm's and
  // infra/forge/journal-read.sh's — not a second number to remember.
  test('the threshold is the journal door’s 30 minutes, three timer ticks', () => {
    expect(CONVERGE_TIMER_MINUTES).toBe(10);
    expect(CONVERGE_SILENCE_MAX_S).toBe(1800);
  });

  test('a packet within the threshold is fresh and names its age', () => {
    const s = convergeSilence([unchangedTick], T('2026-09-12T22:25:10Z'));
    expect(s.kind).toBe('fresh');
    if (s.kind !== 'fresh') return;
    expect(s.ageS).toBe(300);
  });

  test('a newest packet older than the threshold is SILENT, with both clocks', () => {
    const s = convergeSilence([unchangedTick, fullRun], T('2026-09-13T05:20:10Z'));
    expect(s.kind).toBe('silent');
    if (s.kind !== 'silent') return;
    expect(s.newest).toBe('2026-09-12T22:20:10.047972020+00:00');
    expect(s.ageS).toBe(7 * 3600);
  });

  test('an OPEN packet is not silence, however old — it is a run that has not ended', () => {
    const s = convergeSilence([inFlight], T('2026-09-12T23:30:04Z'));
    expect(s.kind).toBe('fresh');
  });

  test('nothing read is UNREAD, never fresh', () => {
    expect(convergeSilence(null, T('2026-09-12T22:21:00Z'))).toEqual({ kind: 'unread' });
    expect(convergeSilence([], T('2026-09-12T22:21:00Z'))).toEqual({ kind: 'unread' });
  });
});

describe('convergeLabel', () => {
  test('running says since when and for how long', () => {
    const r = convergeRun([inFlight], T('2026-09-12T22:33:04Z'));
    expect(convergeLabel(r, T('2026-09-12T22:33:04Z'))).toBe('converging · started 22:30 UTC · 3m00s — stages land when the run ends');
  });

  test('an unchanged tick names the head it found unchanged', () => {
    const r = convergeRun([unchangedTick], T('2026-09-12T22:21:00Z'));
    expect(convergeLabel(r, T('2026-09-12T22:21:00Z'))).toBe('22:20 UTC · forge main unchanged at 845f44a');
  });

  test('a full run names the head and the total of its stages', () => {
    const r = convergeRun([fullRun], T('2026-09-12T22:21:00Z'));
    expect(convergeLabel(r, T('2026-09-12T22:21:00Z'))).toBe('22:16 UTC · converged 845f44a in 5m58s (build 5m29s)');
  });

  test('a failed run is FAILED with how it died and where it got to', () => {
    const r = convergeRun([died], T('2026-09-12T22:21:00Z'));
    expect(convergeLabel(r, T('2026-09-12T22:21:00Z'))).toBe('21:47 UTC · FAILED · exit-code (exit 1) after push');
  });

  test('a run with no summary says the record is missing, not ok', () => {
    const r = convergeRun([noSummary], T('2026-09-12T22:21:00Z'));
    expect(convergeLabel(r, T('2026-09-12T22:21:00Z'))).toBe('21:30 UTC · ok, but the run left no summary');
  });

  test('unknown and none are each named, neither reads as calm', () => {
    expect(convergeLabel({ kind: 'unknown' }, 0)).toBe('no reading — the converge packets did not answer');
    expect(convergeLabel({ kind: 'none' }, 0)).toBe('no converge packet in the window');
  });
});

describe('withConverge — the shed reads the run itself over the request', () => {
  // The request that started the 21:40 run, answered seconds after it was
  // filed — so it predates the run's own verdict.
  const idleShed: RunnerMachine = { kind: 'idle', last: { id: 'req-1', at: '2026-09-12T21:39:30Z', host: 'forge' } };

  test('an open converge packet makes the shed RUNNING from the packet’s own clock', () => {
    const r = convergeRun([inFlight], T('2026-09-12T22:33:04Z'));
    expect(withConverge(idleShed, r)).toEqual({
      kind: 'running',
      id: inFlight.id,
      since: '2026-09-12T22:30:04.000000000+00:00',
      host: 'forge',
    });
  });

  test('a failed converge makes the shed FAILED when the request said nothing', () => {
    const r = convergeRun([died], T('2026-09-12T22:21:00Z'));
    const m = withConverge(idleShed, r);
    expect(m.kind).toBe('failed');
    if (m.kind !== 'failed') return;
    expect(m.id).toBe(died.id);
    expect(m.reason).toBe('exit-code (exit 1) after push');
  });

  test('a completed converge leaves the shed’s own reading alone', () => {
    const r = convergeRun([fullRun], T('2026-09-12T22:21:00Z'));
    expect(withConverge(idleShed, r)).toBe(idleShed);
  });

  test('an older failure does not outrank a request that has since answered', () => {
    const later: RunnerMachine = { kind: 'idle', last: { id: 'req-2', at: '2026-09-12T22:16:30Z', host: 'forge' } };
    const r = convergeRun([died], T('2026-09-12T22:21:00Z')); // closed 21:47, before req-2
    expect(withConverge(later, r)).toBe(later);
  });

  test('no converge reading leaves the shed alone', () => {
    expect(withConverge(idleShed, { kind: 'unknown' })).toBe(idleShed);
    expect(withConverge(idleShed, { kind: 'none' })).toBe(idleShed);
  });
});

describe('JOURNAL_TAIL — the live tail is named as not yet wired, never drawn empty', () => {
  test('the card states the tail is unwired and names the shell door that reads it', () => {
    expect(JOURNAL_TAIL.kind).toBe('unwired');
    expect(JOURNAL_TAIL.unit).toBe('cluster-deploy-runner.service');
    expect(JOURNAL_TAIL.command).toContain('infra/forge/journal-read.sh');
    expect(JOURNAL_TAIL.command).toContain('_SYSTEMD_UNIT=cluster-deploy-runner.service');
    expect(JOURNAL_TAIL.why).toMatch(/gateway/);
  });
});
