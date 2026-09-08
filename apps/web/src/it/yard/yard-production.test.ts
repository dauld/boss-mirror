import { describe, expect, test } from 'bun:test';
import { ARRIVALS_SHOWN, CANCELLED_SHOWN, type CarRow, type TrainRow, type YardState } from './yard';
import type { YardStatus } from './yard-status';
import { landedBars, production, utcDay } from './yard-production';

// The production panel counts what landed today from the arrivals the
// page already computes. That window is bounded (the board shows the
// last few trains), so a count that may be cut off by the window says
// so instead of reading as the day's total.

const NOW = Date.parse('2026-09-08T02:35:20Z');

const yardOf = (over: Partial<YardState> = {}): YardState => ({
  inFlight: [],
  dock: [],
  dockStation: { source: 'derived' },
  arrivals: [],
  cancelled: [],
  delivery: [],
  awaitingProof: [],
  approach: [],
  cars: [],
  packets: { trains: [], gateRuns: [] },
  ...over,
});

const statusOf = (over: Partial<YardStatus> = {}): YardStatus => ({
  trains: [],
  dock: [],
  boarding: { dock_threshold: 1, cooldown_minutes: 45, at_times: [], dock_depth: 0, threshold_met: false, summary: '', held_because: null, cooldown_remaining_minutes: null, last_board_at: null, next_board: null },
  recent: [],
  stranded: [],
  held: [],
  gates: { capacity: 3, active: [] },
  garage: [],
  policy: { stall_hours: 2, max_red_trains: 2 },
  conductor: null,
  now: '2026-09-08T02:35:20Z',
  ...over,
});

const car = (id: string): CarRow => ({ id, kind: 'ship-a-change', branch: `fix/${id}`, title: id, tags: [], sim: false, skipReason: null, head: null });

const landed = (id: string, at: string, nCars: number, outcome: TrainRow['outcome'] = 'arrived'): TrainRow => ({
  id,
  title: `PR train ${at.slice(0, 10)} ${at.slice(11, 16)}`,
  prUrl: null,
  status: 'ARRIVED',
  lamp: 'green',
  mergeRef: 'abc1234',
  deployed: null,
  convergingSince: null,
  cars: Array.from({ length: nCars }, (_, i) => car(`${id}-c${i}`)),
  live: false,
  outcome,
  arrivedAt: { ms: Date.parse(at), at, basis: 'completed_at' },
  eta: { kind: 'phase', phase: 'arrived' },
  trouble: null,
  cancelRequested: null,
  cancelRefused: false,
});

describe('production — today, from the arrivals the page already holds', () => {
  test('the day is the UTC date of the clock', () => {
    expect(utcDay(NOW)).toBe('2026-09-08');
    expect(utcDay(Date.parse('2026-09-07T23:59:59Z'))).toBe('2026-09-07');
  });

  test("today's cars and trains count; yesterday's do not; a cancelled train counts as cancelled, its cars never landed", () => {
    const p = production(
      yardOf({
        arrivals: [landed('t3', '2026-09-08T02:30:25Z', 2), landed('t2', '2026-09-08T00:50:41Z', 1), landed('t1', '2026-09-07T23:50:00Z', 4)],
        cancelled: [landed('x1', '2026-09-08T01:46:42Z', 1, 'cancelled'), landed('x0', '2026-09-07T20:00:00Z', 1, 'cancelled')],
      }),
      statusOf(),
      NOW,
    );
    expect(p).toMatchObject({ day: '2026-09-08', carsLanded: 3, carsWindowed: false, trainsArrived: 2, trainsCancelled: 1, trainsWindowed: false });
  });

  test('a full window whose oldest train is still today may be cut off, and says so', () => {
    const arrivals = Array.from({ length: ARRIVALS_SHOWN }, (_, i) => landed(`t${i}`, `2026-09-08T0${i}:10:00Z`, 1));
    expect(production(yardOf({ arrivals }), statusOf(), NOW)).toMatchObject({ carsLanded: ARRIVALS_SHOWN, carsWindowed: true, trainsWindowed: true });
    const cancelled = Array.from({ length: CANCELLED_SHOWN }, (_, i) => landed(`x${i}`, `2026-09-08T0${i}:20:00Z`, 1, 'cancelled'));
    expect(production(yardOf({ cancelled }), statusOf(), NOW).trainsWindowed).toBe(true);
    // A window with room, or whose oldest is yesterday, is the whole day.
    const partial = [...arrivals.slice(0, 2), landed('old', '2026-09-07T22:00:00Z', 1)];
    expect(production(yardOf({ arrivals: partial }), statusOf(), NOW).carsWindowed).toBe(false);
  });

  test('cars per UTC hour, one bucket an hour, from the arrival instant', () => {
    const p = production(
      yardOf({ arrivals: [landed('t3', '2026-09-08T02:30:25Z', 2), landed('t2', '2026-09-08T02:05:00Z', 1), landed('t1', '2026-09-08T00:50:41Z', 1), landed('t0', '2026-09-07T23:50:00Z', 4)] }),
      statusOf(),
      NOW,
    );
    expect(p.perHour).toHaveLength(24);
    expect(p.perHour[0]).toBe(1);
    expect(p.perHour[2]).toBe(3);
    expect(p.perHour.reduce((a, b) => a + b, 0)).toBe(4);
  });

  test("the median car lead time is the server's journey_seconds over recent trains, or nothing", () => {
    expect(production(yardOf(), statusOf(), NOW)).toMatchObject({ medianJourneyS: null, journeySamples: 0 });
    const p = production(
      yardOf(),
      statusOf({
        recent: [
          { id: 'a', title: 'a', outcome: 'arrived', journey_seconds: 1500 },
          { id: 'b', title: 'b', outcome: 'cancelled', journey_seconds: null },
          { id: 'c', title: 'c', outcome: 'arrived', journey_seconds: 900 },
          { id: 'd', title: 'd', outcome: 'arrived', journey_seconds: 2100 },
        ],
      }),
      NOW,
    );
    expect(p).toMatchObject({ medianJourneyS: 1500, journeySamples: 3 });
    // No status at all: no number.
    expect(production(yardOf(), null, NOW).medianJourneyS).toBeNull();
  });
});

describe('landedBars — the chart, drawn to scale inside its own viewBox', () => {
  test('bars scale to the busiest hour; an empty day draws no bars and says so', () => {
    const per = Array.from({ length: 24 }, (_, h) => (h === 2 ? 3 : h === 0 ? 1 : 0));
    const bars = landedBars(per, 2);
    expect(bars.bars).toHaveLength(24);
    const tallest = bars.bars[2];
    const shorter = bars.bars[0];
    expect(tallest?.n).toBe(3);
    expect(tallest?.h).toBe(bars.plotH);
    expect(shorter?.h).toBeCloseTo(bars.plotH / 3, 5);
    expect(bars.bars[5]?.h).toBe(0);
    expect(bars.max).toBe(3);
    // Every bar stays inside the viewBox.
    for (const b of bars.bars) {
      expect(b.x).toBeGreaterThanOrEqual(0);
      expect(b.x + b.w).toBeLessThanOrEqual(bars.w);
      expect(b.y + b.h).toBeLessThanOrEqual(bars.h);
    }
    expect(bars.bars[2]?.current).toBe(true);
    expect(landedBars(new Array<number>(24).fill(0), 2).max).toBe(0);
  });
});
