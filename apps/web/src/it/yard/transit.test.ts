import { describe, expect, it } from 'bun:test';
import { ROUTES, routesPayload, type FixtureRoute } from '../../../tests/fixtures/yard';
import type { Border } from './borders';
import { regionHref, type Region, type Regions } from './regions';
import { parseRoutes } from './routes';
import { TERRITORIES } from './world';
import { pointAt } from './world-motion';
import {
  ARC_ABOVE,
  ARC_BELOW,
  LINE_TOKEN,
  MAX_BLOCKS,
  MAX_PERIOD_S,
  MIN_TRAVEL_S,
  STATIONS,
  TRANSIT_VIEW,
  alarmsOf,
  gapText,
  headwayText,
  linesOf,
  routePath,
  sectionGround,
  sectionKey,
  sectionsOf,
  stationCount,
  stationLabel,
  trainsOf,
  waitingBlocks,
} from './transit';

const border = (from: string, to: string, over: Partial<Border> = {}): Border => ({
  from,
  to,
  crossing: 'a packet crossed',
  rate: { metric: 'crossings', unit: 'per day', current: 429, previous: 400, samples: 429, previous_samples: 400 },
  last_crossed: '2026-09-24T22:14:00Z',
  waiting: 3,
  holds: [],
  holds_by_class: null,
  flowing: true,
  held_since: null,
  flowing_why: 'last crossed 1m ago',
  machine: { name: 'a rule', kind: 'dispatcher-rule', last_fired: null, silent_for_minutes: null, expected_every_minutes: null, silent: null, why: '' },
  state: 'clear',
  why: 'flowing',
  ...over,
});

const region = (name: string, over: Partial<Region> = {}): Region => ({
  name,
  count: 2,
  bound: null,
  bound_kind: null,
  unit: 'cars parked',
  state: 'clear',
  why: 'fine',
  band: null,
  trend: { metric: 'm', unit: 'u', current: null, previous: null, samples: 0, previous_samples: 0 },
  kpi: [],
  machines: [],
  places: [],
  ...over,
});

const read = (regions: ReadonlyArray<Region>): Regions => ({
  window_hours: 24,
  regions,
  now: '2026-09-24T22:15:00Z',
  thirds: [],
  machines: null,
  plant: [],
});

/** The fixture's routes, as the page parses them off the wire. */
const served = (routes: ReadonlyArray<FixtureRoute> = ROUTES) => parseRoutes(routesPayload(routes));
const laid = (routes?: ReadonlyArray<FixtureRoute>) => sectionsOf(served(routes)).sections;
const find = (key: string) => laid().find((s) => s.key === key)!;
const station = (name: string) => STATIONS.find((s) => s.name === name)!;

describe('the stations are the world layout, drawn as a line', () => {
  it('has one station per territory — a region the world draws is a station here', () => {
    expect(STATIONS.map((s) => s.name).sort()).toEqual(TERRITORIES.map((t) => t.name).sort());
  });

  it('keeps every station inside the view', () => {
    for (const s of STATIONS) {
      expect(s.x).toBeGreaterThan(20);
      expect(s.x).toBeLessThan(TRANSIT_VIEW.width - 20);
      expect(s.y).toBeGreaterThan(40);
      expect(s.y).toBeLessThan(TRANSIT_VIEW.height - 40);
    }
  });

  it('names every line colour as a --map-line-* token, never a literal', () => {
    for (const t of Object.values(LINE_TOKEN)) expect(t).toMatch(/^--map-line-[a-z]+$/);
  });
});

// THE EDGES ARE SERVED (design e765b3fc, car R3): the map draws exactly
// the routes /api/yard/routes answers — one drawn element per route,
// no more and no fewer — laid out by the octilinear router.
describe('the map draws exactly the routes the server serves', () => {
  it('one drawn element per served route, keyed by its two ends', () => {
    expect(laid().map((s) => s.key)).toEqual(ROUTES.map((r) => sectionKey(r.from, r.to)));
  });

  it('a route the server starts serving arrives on the map, and one it stops serving leaves it — no web change', () => {
    const more = laid([...ROUTES, { from: 'track', to: 'garage', declared: true, sources: [{ source: 'observed', moves: 1, last_at: '' }] }]);
    expect(more.map((s) => s.key)).toContain('track→garage');
    const fewer = laid(ROUTES.filter((r) => !(r.from === 'gates' && r.to === 'track')));
    expect(fewer.map((s) => s.key)).not.toContain('gates→track');
    expect(fewer).toHaveLength(ROUTES.length - 1);
  });

  it('draws nothing it was not served: no routes, no sections', () => {
    expect(laid([])).toEqual([]);
  });

  it('names a section, an exit and an entry by their ends, the key a move (car M2) finds its path by', () => {
    expect(find('gates→track').kind).toBe('section');
    expect(find('dock→').kind).toBe('exit');
    expect(find('→receiving').kind).toBe('entry');
    expect(sectionKey('dock', null)).toBe('dock→');
    expect(sectionKey(null, 'receiving')).toBe('→receiving');
  });

  it('says, rather than drops, a route to a region this layout has no station for', () => {
    const out = sectionsOf(served([...ROUTES, { from: 'gates', to: 'the-moon', declared: true, sources: [{ source: 'observed', moves: 1, last_at: '' }] }]));
    expect(out.unplaced).toEqual(['gates→the-moon']);
    expect(out.sections).toHaveLength(ROUTES.length);
  });

  it('carries whether a route is declared — observed-only routes are drawn apart (dashed red)', () => {
    expect(find('shed→arrivals').declared).toBe(false);
    expect(find('gates→track').declared).toBe(true);
  });

  it('puts a route on the line of the station it serves off the main line', () => {
    expect(find('receiving→marshalling').line).toBe('delivery');
    expect(find('gates→track').line).toBe('delivery');
    expect(find('gates→garage').line).toBe('siding');
    expect(find('garage→dock').line).toBe('siding');
    expect(linesOf(laid())).toEqual(['delivery', 'siding']);
  });
});

describe('the octilinear router', () => {
  it('runs every section from its from-station to its to-station', () => {
    for (const s of laid().filter((x) => x.kind === 'section')) {
      const a = station(s.from!);
      const b = station(s.to!);
      const start = pointAt(s.walked, 0);
      const end = pointAt(s.walked, s.walked.length);
      expect([s.key, Math.round(start.x), Math.round(start.y)]).toEqual([s.key, a.x, a.y]);
      expect([s.key, Math.round(end.x), Math.round(end.y)]).toEqual([s.key, b.x, b.y]);
    }
  });

  it('draws at fixed angles only — every leg horizontal, vertical or 45°', () => {
    for (const s of laid()) {
      const pts = s.walked.points;
      for (let i = 1; i < pts.length; i++) {
        const dx = Math.abs(pts[i]!.x - pts[i - 1]!.x);
        const dy = Math.abs(pts[i]!.y - pts[i - 1]!.y);
        expect([s.key, dx === 0 || dy === 0 || Math.abs(dx - dy) < 0.01]).toEqual([s.key, true]);
      }
    }
  });

  it('runs straight to the next station on the line', () => {
    expect(find('receiving→marshalling').walked.points).toHaveLength(2);
  });

  it('lifts a forward run over the station it passes — the train departs from the gates OVER the dock to the track', () => {
    const pts = find('gates→track').walked.points;
    const dock = station('dock');
    expect(Math.min(...pts.map((p) => p.y))).toBe(dock.y - ARC_ABOVE);
    // It never touches the dock's ring.
    const nearest = Math.min(...pts.map((p) => Math.hypot(p.x - dock.x, p.y - dock.y)));
    expect(nearest).toBeGreaterThan(20);
  });

  it('drops a backward run UNDER the line — the train made up at the dock runs back to the gates', () => {
    const pts = find('dock→gates').walked.points;
    expect(Math.max(...pts.map((p) => p.y))).toBe(station('dock').y + ARC_BELOW);
    const back = find('shop-floor→marshalling').walked.points;
    expect(Math.max(...back.map((p) => p.y))).toBeGreaterThan(station('shop-floor').y);
  });

  it('keeps the two directions of a two-way run apart — the garage and the dock, both ways', () => {
    const out = find('dock→garage').walked;
    const back = find('garage→dock').walked;
    const mid = (w: typeof out) => pointAt(w, w.length / 2);
    expect(Math.hypot(mid(out).x - mid(back).x, mid(out).y - mid(back).y)).toBeGreaterThan(8);
  });

  it('moves a straight two-way run a lane apart', () => {
    const a = { name: 'a', x: 100, y: 100, below: false, line: 'delivery' as const };
    const b = { name: 'b', x: 100, y: 300, below: true, line: 'siding' as const };
    const there = routePath(a, b, [a, b], true);
    const back = routePath(b, a, [a, b], true);
    expect(there[0]!.x).not.toBe(back[1]!.x);
    expect(routePath(a, b, [a, b], false)[0]).toEqual({ x: 100, y: 100 });
  });

  it('draws an exit away from the line, and an entry into its station', () => {
    const exit = find('dock→').walked.points;
    expect(exit[0]!.y).toBeGreaterThan(station('dock').y);
    expect(exit[exit.length - 1]!.y).toBeGreaterThan(exit[0]!.y);
    const entry = find('→receiving').walked.points;
    expect(entry[0]!.y).toBeLessThan(entry[entry.length - 1]!.y);
    expect(entry[entry.length - 1]!.y).toBeLessThan(station('receiving').y);
  });
});

describe('a station', () => {
  it('reads its name without hyphens', () => {
    expect(stationLabel('shop-floor')).toBe('shop floor');
    expect(stationLabel('dock')).toBe('dock');
  });

  it('selects itself on the Department Map — every station, through the one regionHref the world map uses too', () => {
    for (const s of STATIONS) expect(regionHref(s.name), s.name).toBe(`/it?at=${s.name}`);
    expect(regionHref('publish')).toBe('/it?at=publish');
  });

  it('prints its count in its unit, against a capacity where it has one, and never 0 for an unread count', () => {
    expect(stationCount(region('dock', { count: 2, unit: 'cars parked' }))).toBe('2 cars parked');
    expect(stationCount(region('gates', { count: 2, bound: 3, bound_kind: 'capacity', unit: 'bays in use' }))).toBe('2 / 3 bays in use');
    expect(stationCount(region('dock', { count: 6, bound: 1, bound_kind: 'threshold', unit: 'cars parked' }))).toBe('6 cars parked');
    expect(stationCount(region('shed', { count: null }))).toBe('no reading');
    expect(stationCount(undefined)).toBe('no reading');
  });
});

describe('a section', () => {
  it('is held exactly where the server says it is not flowing, and unknown where it cannot tell', () => {
    expect(sectionGround(border('a', 'b'))).toBe('flowing');
    expect(sectionGround(border('a', 'b', { flowing: false }))).toBe('held');
    expect(sectionGround(border('a', 'b', { flowing: null }))).toBe('unknown');
    expect(sectionGround(undefined)).toBe('unknown');
  });

  it('prints its headway: crossings a day and the mean gap', () => {
    expect(gapText(429)).toBe('3m gap');
    expect(gapText(44)).toBe('33m gap');
    expect(gapText(13)).toBe('1.8h gap');
    expect(gapText(1)).toBe('24.0h gap');
    expect(gapText(0)).toBe('none crossed');
    expect(headwayText(border('a', 'b'))).toBe('429/d · 3m gap');
  });

  it('says since when it is held, from the server\'s own held_since, on a held section', () => {
    const held = border('a', 'b', {
      rate: { metric: 'c', unit: 'd', current: 6, previous: 6, samples: 6, previous_samples: 6 },
      flowing: false,
      held_since: '2026-09-24T08:48:00Z',
    });
    expect(headwayText(held)).toBe('6/d · held 08:48Z');
    expect(headwayText(border('a', 'b', { flowing: false, held_since: null }))).toBe('429/d · held');
  });

  it('prints no reading — never 0 — for an unread rate or an unread border', () => {
    const unread = border('a', 'b', {
      rate: { metric: 'c', unit: 'd', current: null, previous: null, samples: 0, previous_samples: 0 },
    });
    expect(headwayText(unread)).toBe('no reading');
    expect(headwayText(undefined)).toBe('no reading');
  });
});

describe('waiting blocks stand on the approach to the station they wait to enter', () => {
  const s = find('marshalling→shop-floor');

  it('one block per waiting packet, up to the cap, nearest the destination first', () => {
    const w = waitingBlocks(s, border('marshalling', 'shop-floor', { waiting: 3 }));
    expect(w.blocks).toHaveLength(3);
    expect(w.more).toBeNull();
    expect(w.blocks[0]!.x).toBeGreaterThan(w.blocks[1]!.x);
    // on the approach: nearer the shop floor than the marshalling yard
    expect(w.blocks[2]!.x).toBeGreaterThan(s.walked.length / 2 + 180);
  });

  it('past the cap, the rest are a count, not more blocks', () => {
    const w = waitingBlocks(s, border('marshalling', 'shop-floor', { waiting: 236 }));
    expect(w.blocks).toHaveLength(MAX_BLOCKS);
    expect(w.more?.n).toBe(236 - MAX_BLOCKS);
  });

  it('draws nothing for nothing waiting, and nothing for an unread count', () => {
    expect(waitingBlocks(s, border('a', 'b', { waiting: 0 }))).toEqual({ blocks: [], more: null });
    expect(waitingBlocks(s, border('a', 'b', { waiting: null }))).toEqual({ blocks: [], more: null });
    expect(waitingBlocks(s, undefined)).toEqual({ blocks: [], more: null });
  });
});

describe('the moving block is the real crossing rate, replayed ×60', () => {
  const rate = (current: number | null) => ({ metric: 'c', unit: 'd', current, previous: null, samples: 0, previous_samples: 0 });

  it('departs once per 1440/rate seconds of screen time', () => {
    // 205/day: one every 421 s real, 7.02 s on screen at ×60.
    const t = trainsOf(border('a', 'b', { rate: rate(205) }), false)!;
    expect(t.begins).toEqual([0]);
    expect(t.dur).toBeCloseTo(1440 / 205, 5);
  });

  it('a section faster than one block can cross runs several, staggered, so departures still keep the rate', () => {
    // 2400/day: one every 0.6 s on screen, under the shortest crossing.
    const t = trainsOf(border('a', 'b', { rate: rate(2400) }), false)!;
    const period = 1440 / 2400;
    expect(t.dur).toBeGreaterThanOrEqual(MIN_TRAVEL_S);
    expect(t.dur / t.begins.length).toBeCloseTo(period, 5);
    expect(t.begins.length).toBeGreaterThan(1);
    t.begins.forEach((b, i) => expect(b + i * period).toBeCloseTo(0, 9));
  });

  it('draws no train slower than one in MAX_PERIOD_S — the headway says it instead', () => {
    expect(trainsOf(border('a', 'b', { rate: rate(1440 / (MAX_PERIOD_S + 1)) }), false)).toBeNull();
    expect(trainsOf(border('a', 'b', { rate: rate(1440 / MAX_PERIOD_S) }), false)).not.toBeNull();
  });

  it('moves nothing on a held, unknown, unread or empty section', () => {
    expect(trainsOf(border('a', 'b', { flowing: false }), false)).toBeNull();
    expect(trainsOf(border('a', 'b', { flowing: null }), false)).toBeNull();
    expect(trainsOf(border('a', 'b', { rate: rate(null) }), false)).toBeNull();
    expect(trainsOf(border('a', 'b', { rate: rate(0) }), false)).toBeNull();
    expect(trainsOf(undefined, false)).toBeNull();
  });

  it('moves nothing under reduced motion', () => {
    expect(trainsOf(border('a', 'b'), true)).toBeNull();
  });
});

describe('the alarms board', () => {
  it('lists every non-clear station with the server\'s why — troubled first, then attention, in line order', () => {
    const alarms = alarmsOf(
      read(
        TERRITORIES.map((t) =>
          t.name === 'shed'
            ? region('shed', { state: 'troubled', why: '9 of 10 open past 24h' })
            : t.name === 'receiving'
              ? region('receiving', { state: 'attention', why: '80 packets standing' })
              : t.name === 'publish'
                ? region('publish', { state: 'troubled', why: 'a pull request open 26 hours' })
                : t.name === 'shop-floor'
                  ? region('shop-floor', { state: 'troubled', why: '1 run finished and not reported' })
                  : region(t.name),
        ),
      ),
    );
    expect(alarms.map((a) => [a.name, a.state])).toEqual([
      ['shop-floor', 'troubled'],
      ['shed', 'troubled'],
      ['publish', 'troubled'],
      ['receiving', 'attention'],
    ]);
    expect(alarms[1]!.why).toBe('9 of 10 open past 24h');
    expect(alarms[0]!.label).toBe('shop floor');
  });

  it('a station the read did not carry is an alarm, never a quiet station', () => {
    const alarms = alarmsOf(read(TERRITORIES.filter((t) => t.name !== 'dock').map((t) => region(t.name))));
    expect(alarms.map((a) => [a.name, a.state])).toEqual([['dock', 'troubled']]);
    expect(alarms[0]!.why).toContain('no reading');
  });

  it('a full station is no alarm — at capacity and moving is the good state (design e765b3fc §4a)', () => {
    const alarms = alarmsOf(
      read(TERRITORIES.map((t) => (t.name === 'gates' ? region('gates', { state: 'full', why: '3 of 3 bays in use' }) : region(t.name)))),
    );
    expect(alarms).toEqual([]);
  });

  it('is empty when every station is clear', () => {
    expect(alarmsOf(read(TERRITORIES.map((t) => region(t.name))))).toEqual([]);
  });
});
