import { describe, expect, test } from 'bun:test';
import {
  conductorTimeline,
  pacificDate,
  pacificTime,
  prNumberFromUrl,
  MAX_ENTRIES,
  type TimelineEntry,
} from './conductorActivity';
import type { JobLite, StepLite } from '../yard/yard';

// A step builder — completed with an RFC3339 instant unless told
// otherwise, since the feed places entries by that instant.
function step(
  slug: string,
  completedAt: string | null,
  extra: Partial<StepLite> = {},
): StepLite {
  const md: Record<string, unknown> = { ...(extra.metadata ?? {}) };
  if (completedAt !== null) md.completed_at = completedAt;
  return {
    spec_slug: slug,
    title: slug,
    status: extra.status ?? 'completed',
    metadata: md,
    completed_at: completedAt,
  };
}

function train(id: string, steps: StepLite[], metadata: Record<string, unknown> = {}): JobLite {
  return {
    id,
    kind: 'pr-train',
    title: `train ${id}`,
    status: 'open',
    opened_on: '2026-09-05',
    metadata,
    steps,
  };
}

const byAction = (entries: readonly TimelineEntry[], action: string) =>
  entries.find(e => e.action === action);

describe('conductorTimeline', () => {
  test('sorts entries newest-first across trains', () => {
    const t1 = train('t1', [
      step('pr', '2026-09-05T22:00:00Z'),
      step('merged', '2026-09-05T22:30:00Z', { metadata: { merge_ref: 'abc12345def' } }),
    ]);
    const t2 = train('t2', [step('pr', '2026-09-05T23:00:00Z')]);

    // Feed the trains out of order — the function must re-sort.
    const feed = conductorTimeline([t1, t2]);
    const stamps = feed.map(e => e.at);
    expect(stamps).toEqual([
      '2026-09-05T23:00:00Z', // t2 boarded
      '2026-09-05T22:30:00Z', // t1 merged
      '2026-09-05T22:00:00Z', // t1 boarded
    ]);
    // And genuinely descending by ms.
    for (let i = 1; i < feed.length; i++) {
      expect(feed[i - 1]!.ms).toBeGreaterThanOrEqual(feed[i]!.ms);
    }
  });

  test('a cancelled train renders a cancelled entry', () => {
    const t = train('tc', [
      step('collect', '2026-09-05T20:00:00Z'),
      step('cancelled', '2026-09-05T20:05:00Z', { metadata: { reason: 'nothing ready to board' } }),
    ]);
    const feed = conductorTimeline([t]);
    const cancelled = byAction(feed, 'cancelled');
    expect(cancelled).toBeDefined();
    expect(cancelled!.label).toBe('cancelled');
    expect(cancelled!.detail).toBe('nothing ready to board');
  });

  test('a green ci step renders "CI green"; failing renders "CI failed"', () => {
    const green = conductorTimeline([
      train('tg', [step('ci', '2026-09-05T21:00:00Z', { metadata: { result: 'green' } })]),
    ]);
    const g = byAction(green, 'ci-green');
    expect(g).toBeDefined();
    expect(g!.label).toBe('CI green');

    const failing = conductorTimeline([
      train('tf', [
        step('ci', '2026-09-05T21:00:00Z', { metadata: { result: 'failing', checks: 'test:FAILURE' } }),
      ]),
    ]);
    const f = byAction(failing, 'ci-failed');
    expect(f).toBeDefined();
    expect(f!.label).toBe('CI failed');
    expect(f!.detail).toBe('test:FAILURE');
  });

  test('a train with no completed steps yields nothing', () => {
    const t = train('tp', [
      step('pr', '2026-09-05T22:00:00Z', { status: 'active' }),
      step('ci', '2026-09-05T22:10:00Z', { status: 'ready' }),
    ]);
    expect(conductorTimeline([t])).toEqual([]);
  });

  test('a completed step without a timestamp is left off the feed', () => {
    const t = train('tn', [step('merged', null, { metadata: { merge_ref: 'deadbeef' } })]);
    expect(conductorTimeline([t])).toEqual([]);
  });

  test('exactly one "boarded" entry per train, preferring the pr step time', () => {
    const t = train('tb', [
      step('collect', '2026-09-05T22:00:00Z'),
      step('pr', '2026-09-05T22:05:00Z', { metadata: { pr_url: 'https://forge/x/pulls/234' } }),
    ], { boarded_jobs: ['a', 'b'] });
    const feed = conductorTimeline([t]);
    const boarded = feed.filter(e => e.action === 'boarded');
    expect(boarded.length).toBe(1);
    // pr wins, so the boarded time is the pr step's, not collect's.
    expect(boarded[0]!.at).toBe('2026-09-05T22:05:00Z');
    // Without a name map, the boarded detail is a plain count.
    expect(boarded[0]!.detail).toBe('2 cars');
    // The PR number rides on the entry.
    expect(boarded[0]!.prNumber).toBe(234);
  });

  test('car names, when supplied, are listed on the boarded entry', () => {
    const t = train('tb2', [step('pr', '2026-09-05T22:05:00Z')], {
      boarded_jobs: ['a', 'b', 'c'],
    });
    const names = new Map([
      ['a', 'fix-the-alert'],
      ['b', 'regate-dc'],
      ['c', 'third-branch'],
    ]);
    const feed = conductorTimeline([t], names);
    const boarded = feed.find(e => e.action === 'boarded');
    expect(boarded!.detail).toBe('3 cars: fix-the-alert, regate-dc +1 more');
  });

  test('merged and converged carry short refs', () => {
    const t = train('tm', [
      step('merged', '2026-09-05T22:30:00Z', { metadata: { merge_ref: '7864789abcdef' } }),
      step('converged', '2026-09-05T22:45:00Z', { metadata: { cluster_commit: '7864789abcdef' } }),
    ]);
    const feed = conductorTimeline([t]);
    expect(byAction(feed, 'merged')!.detail).toBe('→ 7864789a');
    expect(byAction(feed, 'converged')!.label).toBe('cluster converged');
    expect(byAction(feed, 'converged')!.detail).toBe('→ 7864789a');
  });

  test('the feed is capped at MAX_ENTRIES', () => {
    // Each train emits one boarded entry; make more than the cap.
    const trains = Array.from({ length: MAX_ENTRIES + 10 }, (_, i) =>
      train(`t${i}`, [
        step('pr', `2026-09-05T${String(i % 24).padStart(2, '0')}:00:00Z`),
      ]),
    );
    expect(conductorTimeline(trains).length).toBe(MAX_ENTRIES);
  });

  test('title-fallback addressing works when spec_slug is absent', () => {
    const s: StepLite = {
      title: 'Merged into main',
      status: 'completed',
      metadata: { completed_at: '2026-09-05T22:30:00Z', merge_ref: 'abcdef12' },
    };
    const t = train('tt', [s]);
    const feed = conductorTimeline([t]);
    expect(byAction(feed, 'merged')).toBeDefined();
  });
});

describe('prNumberFromUrl', () => {
  test('parses the trailing number of a forge PR url', () => {
    expect(prNumberFromUrl('https://forge.example/algedonic/boss/pulls/234')).toBe(234);
    expect(prNumberFromUrl('https://github.com/o/r/pull/17')).toBe(17);
    expect(prNumberFromUrl('https://forge/x/pulls/99/')).toBe(99);
  });

  test('returns null when there is no number', () => {
    expect(prNumberFromUrl(null)).toBeNull();
    expect(prNumberFromUrl('')).toBeNull();
    expect(prNumberFromUrl('https://forge/x/pulls/')).toBeNull();
  });
});

describe('Pacific-time conversion', () => {
  test('a summer UTC instant renders in PDT (UTC-7)', () => {
    // 2026-09-05 is in Pacific Daylight Time (UTC-7).
    expect(pacificTime('2026-09-05T23:01:00Z')).toBe('4:01 PM');
    expect(pacificDate('2026-09-05T23:01:00Z')).toBe('Sep 5');
  });

  test('a winter UTC instant renders in PST (UTC-8), rolling the date back', () => {
    // 2026-01-15 00:30 UTC is 2026-01-14 16:30 in Pacific Standard Time.
    expect(pacificTime('2026-01-15T00:30:00Z')).toBe('4:30 PM');
    expect(pacificDate('2026-01-15T00:30:00Z')).toBe('Jan 14');
  });

  test('an unparseable stamp degrades to empty strings', () => {
    expect(pacificTime('not-a-date')).toBe('');
    expect(pacificDate('not-a-date')).toBe('');
  });
});
