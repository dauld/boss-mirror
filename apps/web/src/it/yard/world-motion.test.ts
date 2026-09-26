import { describe, expect, it } from 'bun:test';
import type { Border } from './borders';
import { BORDERS } from '../../../tests/fixtures/yard';
import { BADGE_HALF, TERRITORIES, railOf, territoryOf } from './world';
import {
  COMPRESSIONS,
  DEFAULT_COMPRESSION,
  FRAME_BUDGET_MS,
  MAX_EMIT_PER_SEC,
  MAX_IN_FLIGHT,
  READ_EVERY_MS,
  REPLAY_TEXT,
  RING_MS,
  SPEED,
  TWEEN_MS,
  advance,
  capacityOf,
  clockText,
  compressionText,
  emitPerSec,
  heldMs,
  heldText,
  motionRateText,
  newlyTroubled,
  pathPoints,
  pileBox,
  pileOf,
  pileSquares,
  pointAt,
  prefill,
  railStill,
  ringAt,
  spacingOf,
  squareAlpha,
  squareAt,
  staleFor,
  walk,
  type Flow,
} from './world-motion';

// THE IT MAP MOVES (design 31bade8f, car M2 on backlog d220022f). The
// motion layer draws from the server's reading only; these pin the
// arithmetic it draws with — the rate replay, the piles, the stillness
// read off the server, the clocks, the grey-out and the one ring —
// because the design's rule is that the spec pins the arithmetic and
// never snapshots pixels.

const border = (over: Partial<Border> = {}): Border => ({
  from: 'dock',
  to: 'track',
  crossing: 'a car boarded a train',
  rate: { metric: 'crossings', unit: 'per day', current: 24, previous: 18, samples: 24, previous_samples: 18 },
  last_crossed: '2026-09-24T11:00:00Z',
  waiting: 5,
  holds: [],
  holds_by_class: { machine: 2, person: 1, unknown: 1, stuck: 1 },
  flowing: true,
  held_since: null,
  flowing_why: 'last crossed 1h ago, inside 4× its mean gap',
  machine: { name: 'm', kind: 'cadence', last_fired: null, silent_for_minutes: null, expected_every_minutes: null, silent: null, why: '' },
  state: 'clear',
  why: '',
  ...over,
});

describe('the rate replay: density carries the rate, the compression is stated', () => {
  it('defaults to ×600 and says what one real hour becomes, and what a token is', () => {
    expect(DEFAULT_COMPRESSION).toBe(600);
    expect([...COMPRESSIONS]).toEqual([0, 60, 600, 3600]);
    expect(compressionText(600)).toBe('×600 — 1 real hour = 6 s on screen');
    expect(compressionText(60)).toBe('×60 — 1 real hour = 1 min on screen');
    expect(compressionText(3600)).toBe('×3600 — 1 real hour = 1 s on screen');
    expect(compressionText(0)).toBe('paused — nothing travels');
    expect(REPLAY_TEXT).toContain('not individual events');
  });

  it('emits the measured rate, time-compressed, evenly — and one token stands for k past 5 a second', () => {
    // 24 a day at ×600 is 24 × 600 / 86400 = 1/6 a second.
    expect(emitPerSec(24, 600)).toEqual({ per: 24 * 600 / 86_400, k: 1 });
    // 501 a day at ×3600 is ~20.9 a second: k = 5, each token five crossings.
    const busy = emitPerSec(501, 3600);
    expect(busy.k).toBe(5);
    expect(busy.per).toBeLessThanOrEqual(MAX_EMIT_PER_SEC);
    expect(busy.per * busy.k).toBeCloseTo(501 * 3600 / 86_400, 9);
    // A rail twice as busy is drawn twice as dense: spacing is speed ÷ rate.
    expect(spacingOf(emitPerSec(12, 600)) / spacingOf(emitPerSec(24, 600))).toBeCloseTo(2, 9);
  });

  it('emits nothing it did not measure, nothing empty and nothing while paused', () => {
    expect(emitPerSec(null, 600).per).toBe(0);
    expect(emitPerSec(0, 600).per).toBe(0);
    expect(emitPerSec(24, 0).per).toBe(0);
  });

  it('caps the tokens in flight on a rail by standing one for more crossings', () => {
    const length = 4000;
    const e = emitPerSec(400, 600, length);
    const inFlight = (length / SPEED) * e.per;
    expect(inFlight).toBeLessThanOrEqual(MAX_IN_FLIGHT);
    expect(e.k).toBeGreaterThan(1);
  });
});

describe('tokens on a rail', () => {
  const e = emitPerSec(144, 600); // 1 a second → spacing 40
  it('prefills a flowing rail to its steady state, evenly spaced — it never starts empty', () => {
    const f = prefill(200, e);
    expect(f.tokens.map((t) => t.d)).toEqual([20, 60, 100, 140, 180]);
  });

  it('keeps the spacing as it runs, at one speed, and a token past the end has crossed', () => {
    const later = Array.from({ length: 50 }, () => 0.1).reduce<Flow>((f, dt) => advance(f, dt, 200, e, true), prefill(200, e));
    const ds = [...later.tokens.map((t) => t.d)].sort((a, b) => a - b);
    expect(ds.length).toBe(5);
    ds.slice(1).forEach((d, i) => expect(d - ds[i]!).toBeCloseTo(40, 6));
    expect(ds.every((d) => d < 200)).toBe(true);
  });

  it('a rail that stops emitting lets its tokens finish their trip, then nothing moves', () => {
    const stopped = advance(prefill(200, e), 1, 200, e, false);
    expect(stopped.tokens.map((t) => t.d)).toEqual([60, 100, 140, 180]);
    const empty = Array.from({ length: 10 }, () => 1).reduce<Flow>((f, dt) => advance(f, dt, 200, e, false), stopped);
    expect(empty.tokens).toEqual([]);
  });

  it('carries k on every token of an aggregated rail', () => {
    const agg = emitPerSec(501, 3600);
    expect(prefill(300, agg).tokens.every((t) => t.k === agg.k)).toBe(true);
  });
});

describe('the server judges stillness; the map reads it', () => {
  it('moves only a measured, flowing rail that is not troubled', () => {
    expect(railStill(border())).toBeNull();
  });
  it('unknown — no border, no rate, or flowing cannot be told — is still and grey', () => {
    expect(railStill(undefined)).toBe('unknown');
    expect(railStill(border({ rate: { ...border().rate, current: null } }))).toBe('unknown');
    expect(railStill(border({ flowing: null }))).toBe('unknown');
  });
  it('troubled and not-flowing rails stand still', () => {
    expect(railStill(border({ state: 'troubled' }))).toBe('troubled');
    expect(railStill(border({ flowing: false, held_since: '2026-09-24T09:00:00Z' }))).toBe('held');
    expect(railStill(border({ rate: { ...border().rate, current: 0 } }))).toBe('held');
  });
});

describe('piles: the server\'s count, shaded by whom it waits on', () => {
  it('draws sediment first, then machine, person, unknown — and the count is waiting, not squares', () => {
    const p = pileOf(border());
    expect(pileSquares(p, 100)).toEqual({ squares: ['stuck', 'machine', 'machine', 'person', 'unknown'], more: 0 });
  });
  it('writes the rest as +N past the grid, never dropping the sediment', () => {
    const p = pileOf(border({ waiting: 235, holds_by_class: { machine: 0, person: 200, unknown: 0, stuck: 35 } }));
    const { squares, more } = pileSquares(p, 24);
    expect(squares.length).toBe(24);
    expect(squares.every((s) => s === 'stuck')).toBe(true);
    expect(more).toBe(211);
  });
  it('an uncounted pile is unknown — never an empty one — and a count with no classes is grey', () => {
    expect(pileOf(border({ waiting: null, holds_by_class: null }))).toEqual({ kind: 'unknown' });
    expect(pileSquares({ kind: 'unknown' }, 24)).toEqual({ squares: [], more: 0 });
    expect(pileSquares(pileOf(border({ waiting: 3, holds_by_class: null })), 24).squares).toEqual(['unknown', 'unknown', 'unknown']);
    // Classes short of the count leave the rest grey; never more squares than the count.
    expect(pileSquares(pileOf(border({ waiting: 2, holds_by_class: { machine: 5, person: 0, unknown: 0, stuck: 0 } })), 24).squares)
      .toEqual(['machine', 'machine']);
  });
  it('tweens only between two reads\' counts: arrivals drop in, departures fade', () => {
    expect(squareAlpha(0, 3, 5, 0)).toBe(1);
    expect(squareAlpha(4, 3, 5, 0)).toBe(0);
    expect(squareAlpha(4, 3, 5, TWEEN_MS / 2)).toBeCloseTo(0.5, 9);
    expect(squareAlpha(4, 5, 3, TWEEN_MS)).toBe(0);
    expect(squareAlpha(4, 5, 3, 0)).toBe(1);
    expect(squareAlpha(9, 5, 3, 0)).toBe(0);
  });

  const inside = (p: { x: number; y: number }, b: { x: number; y: number; w: number; h: number }) =>
    p.x >= b.x && p.x <= b.x + b.w && p.y >= b.y && p.y <= b.y + b.h;
  const rails = BORDERS.map((b) => ({ b, from: territoryOf(b.from)!, rail: railOf(territoryOf(b.from)!, territoryOf(b.to)!) }));

  it('stands every pile at its rail\'s head, clear of every other territory and of every badge', () => {
    rails.forEach(({ b, from, rail }) => {
      const box = pileBox(rail, from);
      const rect = { x: box.x, y: box.y, w: box.cols * 6, h: box.rows * 6 };
      const corners = [
        { x: rect.x, y: rect.y }, { x: rect.x + rect.w, y: rect.y },
        { x: rect.x, y: rect.y + rect.h }, { x: rect.x + rect.w, y: rect.y + rect.h },
      ];
      TERRITORIES.filter((t) => t.name !== from.name).forEach((t) =>
        corners.forEach((c) => expect(inside(c, t), `${b.from}→${b.to} pile over ${t.name}`).toBe(false)));
      rails.forEach(({ rail: other }) => {
        const badge = { x: other.mid.x - BADGE_HALF, y: other.mid.y - 7, w: 2 * BADGE_HALF, h: 14 };
        corners.forEach((c) => expect(inside(c, badge), `${b.from}→${b.to} pile over a badge`).toBe(false));
      });
      expect(capacityOf(box)).toBeGreaterThanOrEqual(16);
    });
  });

  it('stands a line rail\'s pile inside the territory it leaves, in the corner the rail climbs out of', () => {
    rails.filter(({ rail }) => rail.kind === 'line').forEach(({ from, rail }) => {
      const box = pileBox(rail, from);
      expect(box.x).toBeGreaterThan(rail.from.x);
      expect(box.x + box.cols * 6).toBeLessThanOrEqual(from.x + from.w);
      expect(box.y).toBeGreaterThan(from.y);
      expect(squareAt(box, box.cols)).toEqual({ x: box.x, y: box.y + 6 });
    });
  });
});

describe('clocks tick from the server\'s own timestamps', () => {
  const held = border({ flowing: false, held_since: '2026-09-24T08:00:00Z' });
  it('counts held-for from the server\'s now plus the time since the read, not the page\'s clock', () => {
    const readAt = 1_000_000;
    expect(heldMs(held, '2026-09-24T11:12:00Z', readAt, readAt)).toBe((3 * 60 + 12) * 60_000);
    expect(heldMs(held, '2026-09-24T11:12:00Z', readAt, readAt + 60_000)).toBe((3 * 60 + 13) * 60_000);
    expect(heldMs(border(), '2026-09-24T11:12:00Z', readAt, readAt)).toBeNull();
  });
  it('words it', () => {
    expect(clockText(40_000)).toBe('40s');
    expect(clockText(12 * 60_000)).toBe('12m');
    expect(clockText((3 * 60 + 12) * 60_000)).toBe('3h 12m');
    expect(clockText((52 * 60) * 60_000)).toBe('2d 4h');
    expect(heldText(held, 'held', (3 * 60 + 12) * 60_000)).toBe('held 3h 12m');
    expect(heldText(border({ flowing: false }), 'held', null)).toBe('held — nothing crossed in the read');
    expect(heldText(border(), null, null)).toBe('');
    expect(heldText(border({ state: 'troubled' }), 'troubled', null)).toBe('');
  });
  it('prints the rate with its aggregation, and nothing-arriving against the window before', () => {
    expect(motionRateText(border(), { per: 1, k: 1 })).toBe('24/d');
    expect(motionRateText(border(), { per: 5, k: 3 })).toBe('24/d ●=3');
    expect(motionRateText(border({ rate: { ...border().rate, current: 0, previous: 4 } }), { per: 0, k: 1 })).toBe('0 vs 4/d');
    expect(motionRateText(border({ rate: { ...border().rate, current: null } }), { per: 0, k: 1 })).toBe('?');
    expect(motionRateText(undefined, { per: 0, k: 1 })).toBe('?');
  });
  it('greys the map past three missed reads, and not before', () => {
    expect(staleFor(null, 0)).toBeNull();
    expect(staleFor(0, 3 * READ_EVERY_MS + 5_000)).toBeNull();
    expect(staleFor(0, 40_000)).toBe(40);
  });
});

describe('the one ring on entering troubled (Q2: once, never a loop)', () => {
  it('rings only a key that crossed INTO troubled between two reads', () => {
    const prev = new Map([['dock', 'clear'], ['gates', 'troubled'], ['shed', 'attention']] as const);
    const next = new Map([['dock', 'troubled'], ['gates', 'troubled'], ['shed', 'troubled'], ['new', 'troubled']] as const);
    expect(newlyTroubled(prev, next)).toEqual(['dock', 'shed']);
    expect(newlyTroubled(null, next)).toEqual([]);
  });
  it('grows and fades once, then is over for good', () => {
    const start = ringAt(0)!;
    const mid = ringAt(RING_MS / 2)!;
    expect(mid.grow).toBeGreaterThan(start.grow);
    expect(mid.alpha).toBeLessThan(start.alpha);
    expect(ringAt(RING_MS)).toBeNull();
    expect(ringAt(RING_MS * 3)).toBeNull();
  });
});

describe('walking the SVG path the map strokes', () => {
  it('reads the rails world.ts writes, turns included, at their true length', () => {
    const line = walk(pathPoints('M0 100 V10 Q0 0 10 0 H90 Q100 0 100 10 V100'));
    expect(line.points[0]).toEqual({ x: 0, y: 100 });
    expect(line.points[line.points.length - 1]).toEqual({ x: 100, y: 100 });
    // 90 + 80 + 90 straight, plus two quadratic turns on a 10-unit
    // corner (~16.2 each: longer than the arc, shorter than the corner).
    expect(line.length).toBeGreaterThan(260 + 2 * 15.7);
    expect(line.length).toBeLessThan(260 + 2 * 20);
    expect(pointAt(line, 45)).toEqual({ x: 0, y: 55 });
    const drop = walk(pathPoints('M50 296 V362'));
    expect(drop.length).toBe(66);
    expect(pointAt(drop, 33)).toEqual({ x: 50, y: 329 });
  });
  it('walks every rail on the world', () => {
    BORDERS.forEach((b) => {
      const rail = railOf(territoryOf(b.from)!, territoryOf(b.to)!);
      const w = walk(pathPoints(rail.d));
      expect(pointAt(w, 0)).toEqual(rail.from);
      expect(pointAt(w, w.length)).toEqual(rail.to);
    });
  });
});

describe('the frame budget', () => {
  it('steps 500 tokens well inside the 2 ms a frame is allowed', () => {
    const e = emitPerSec(501, 3600, 200);
    const flows = Array.from({ length: 500 / 25 }, () => ({ tokens: Array.from({ length: 25 }, (_, i) => ({ d: i * 8, k: e.k })), acc: 0 }));
    const runs = 200;
    const t0 = performance.now();
    Array.from({ length: runs }).reduce<ReadonlyArray<Flow>>((fs) => fs.map((f) => advance(f, 1 / 60, 200, e, true)), flows);
    expect((performance.now() - t0) / runs).toBeLessThan(FRAME_BUDGET_MS);
  });
});
