import { describe, expect, it } from 'bun:test';
import { ROUTES, routesPayload, type FixtureRoute } from '../../../tests/fixtures/yard';
import type { Border } from './borders';
import { regionHref, type Region, type Regions } from './regions';
import { parseRoutes } from './routes';
import { TERRITORIES } from './world';
import { sectionsOf } from './route-layout';
import {
  LINE_TOKEN,
  MAX_BLOCKS,
  MAX_PERIOD_S,
  MIN_TRAVEL_S,
  STATIONS,
  TRANSIT_VIEW,
  alarmsOf,
  gapText,
  headwayText,
  sectionGround,
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
