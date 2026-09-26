// The queue-age lens's one step-keyed reader (backlog 66a5d5be). The
// parse and duration cases moved here with the reader from the
// incidents card's tests (1a242883); `waitedText` and `lensNow` are
// what both pages print with, so a lower bound reads the same on each.

import { describe, expect, test } from 'bun:test';
import { durationText, lensNow, parseStepWaits, waitedText } from './queueAge';

describe('durationText', () => {
  const m = 60_000;
  test('scales from minutes to days, two units at most', () => {
    expect(durationText(30_000)).toBe('<1m');
    expect(durationText(12 * m)).toBe('12m');
    expect(durationText(5 * 60 * m + 12 * m)).toBe('5h 12m');
    expect(durationText(3 * 1440 * m + 4 * 60 * m + 9 * m)).toBe('3d 4h');
  });
  test('a negative span (clock skew) reads as zero, not a minus sign', () => {
    expect(durationText(-5 * m)).toBe('<1m');
  });
});

describe('parseStepWaits — /api/jobs/queue-age keyed by step', () => {
  test('keeps since and the exact flag, and the server clock', () => {
    const w = parseStepWaits({
      data: [
        { step_id: 's-1', since: '2026-09-26T01:00:00Z', exact: true },
        { step_id: 's-2', since: '2026-09-26T02:00:00Z', exact: false },
        { step_id: null, since: '2026-09-26T02:00:00Z', exact: true },
      ],
      total: 3,
      now: '2026-09-26T05:00:00Z',
    });
    expect(w.now).toBe(Date.parse('2026-09-26T05:00:00Z'));
    expect(w.byStep.get('s-1')).toEqual({ sinceMs: Date.parse('2026-09-26T01:00:00Z'), exact: true });
    expect(w.byStep.get('s-2')?.exact).toBe(false);
    expect(w.byStep.size).toBe(2);
  });
  test('a body that is not the lens\'s shape is a throw, not an empty map', () => {
    expect(() => parseStepWaits([])).toThrow();
    expect(() => parseStepWaits({ total: 0 })).toThrow();
  });
});

describe('waitedText — the figure, or its floor', () => {
  const since = Date.parse('2026-09-21T01:10:00Z');
  const now = Date.parse('2026-09-23T15:40:00Z');
  test('an exact stamp prints the span', () => {
    expect(waitedText({ sinceMs: since, exact: true }, now)).toBe('2d 14h');
  });
  test('a fallback stamp prints as a lower bound, never as the figure', () => {
    expect(waitedText({ sinceMs: since, exact: false }, now)).toBe('≥2d 14h');
  });
});

describe('lensNow — whose clock an age is measured on', () => {
  test('the server clock the lens sent, else the caller\'s', () => {
    expect(lensNow({ now: 5, byStep: new Map() }, 9)).toBe(5);
    expect(lensNow({ now: null, byStep: new Map() }, 9)).toBe(9);
  });
});
