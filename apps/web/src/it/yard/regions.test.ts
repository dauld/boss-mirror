import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  REGION_NAMES,
  bandText,
  compactCountText,
  countText,
  floorHref,
  floorSelection,
  kpiText,
  lampOf,
  parseRegions,
  stateText,
  trendText,
  type Region,
  type Trend,
} from './regions';

// The IT system map (design 0524fc95, car 2): the region cards read
// from ONE endpoint, /api/yard/regions (car 1), each a door to its
// floor. These pin the client half — the parse of the server's payload,
// the floor each card opens, and the words a card prints for a count
// and a trend — so the map cannot make up a number the server did not
// send (a trend with no samples is "no reading", never 0).

const trend = (over: Partial<Trend> = {}): Trend => ({
  metric: 'dock wait',
  unit: 'hours',
  current: 4.25,
  previous: 3,
  samples: 6,
  previous_samples: 5,
  ...over,
});

const region = (over: Partial<Region> = {}): Region => ({
  name: 'dock',
  count: 3,
  bound: null,
  bound_kind: null,
  unit: '',
  state: 'clear',
  why: '3 cars parked',
  band: null,
  trend: trend(),
  kpi: [],
  machines: [],
  places: [],
  ...over,
});

/** The wire shape car 1 serialises, region by region, in map order. */
const PAYLOAD = {
  window_hours: 24,
  now: '2026-09-19T05:00:00Z',
  regions: [
    { name: 'dock', count: 3, bound: 5, state: 'clear', why: '3 cars parked', trend: { metric: 'dock wait', unit: 'hours', current: 4.25, previous: 3, samples: 6, previous_samples: 5 } },
    { name: 'gates', count: 1, bound: 3, state: 'troubled', why: '1 bay holds a corpse — gate-run past its own deadline', trend: { metric: 'gate duration', unit: 'minutes', current: 11, previous: 9.5, samples: 20, previous_samples: 18 } },
    { name: 'track', count: 0, bound: 1, state: 'clear', why: 'no train in transit', trend: { metric: 'time at CI', unit: 'minutes', current: null, previous: 14, samples: 0, previous_samples: 4 } },
    { name: 'shed', count: 2, state: 'attention', why: '2 landed cars await their probe', trend: { metric: 'time to proven', unit: 'hours', current: 1, previous: 1.5, samples: 3, previous_samples: 7 } },
    { name: 'arrivals', count: 17, state: 'clear', why: '17 trains arrived in the window', trend: { metric: 'arrivals', unit: 'per day', current: 17, previous: 12, samples: 17, previous_samples: 12 } },
    { name: 'garage', count: 0, state: 'clear', why: 'nothing gated red', trend: { metric: 'reds', unit: 'per day', current: 0, previous: 2, samples: 0, previous_samples: 2 } },
    { name: 'receiving', count: 4, state: 'attention', why: '4 inbound, oldest 5 days', trend: { metric: 'inbound', unit: 'per day', current: 4, previous: 6, samples: 4, previous_samples: 6 } },
    { name: 'marshalling', count: null, state: 'troubled', why: 'the station registry could not be read', trend: { metric: 'served', unit: 'per day', current: null, previous: null, samples: 0, previous_samples: 0 } },
    { name: 'shop-floor', count: 2, bound: 6, state: 'clear', why: '2 runs in flight, 1 crew on the floor', trend: { metric: 'build duration', unit: 'minutes', current: 64, previous: 58, samples: 5, previous_samples: 4 }, machines: [{ id: 'session:s1', name: 'claude@algedonic.dev', state: 'running', why: 'last prompt 3 min ago; 2 runs in flight' }] },
    { name: 'publish', count: 1, state: 'troubled', why: 'https://mirror/pull/240 — the scan read failure — 109 alert(s) over 14 rule(s), no disposition recorded', trend: { metric: 'publishes', unit: 'per day', current: 1, previous: 1, samples: 1, previous_samples: 1 } },
  ],
};

describe('parseRegions — the payload, parsed once', () => {
  it('reads the regions in the order the server sent them, with count, bound, state, why and trend', () => {
    const m = parseRegions(PAYLOAD);
    expect(m.window_hours).toBe(24);
    expect(m.regions.map((r) => r.name)).toEqual([...REGION_NAMES]);
    const gates = m.regions[1]!;
    expect(gates).toEqual({
      name: 'gates',
      count: 1,
      bound: 3,
      // An older payload carries no kind, unit, band or KPI: each reads
      // as absent — never as a made-up value.
      bound_kind: null,
      unit: '',
      state: 'troubled',
      why: '1 bay holds a corpse — gate-run past its own deadline',
      band: null,
      trend: { metric: 'gate duration', unit: 'minutes', current: 11, previous: 9.5, samples: 20, previous_samples: 18 },
      kpi: [],
      // A payload with no machinery list draws no glyphs — never
      // invented idle ones (car 5, world-machines.test.ts).
      machines: [],
      places: [],
    });
    // No bound on the wire (skip_serializing_if) reads as null, not 0.
    expect(m.regions[3]!.bound).toBeNull();
    // An unread region keeps its null count — the card must not print 0.
    expect(m.regions[7]!.count).toBeNull();
    expect(m.regions[7]!.state).toBe('troubled');
  });

  it('refuses a payload that is not the map — a list where the object is due is a malformed read, not an empty one', () => {
    expect(() => parseRegions([])).toThrow();
    expect(() => parseRegions(null)).toThrow();
    expect(() => parseRegions({ window_hours: 24 })).toThrow(/regions/);
  });

  it('refuses a state it does not know rather than drawing it clear', () => {
    const bad = { ...PAYLOAD, regions: [{ ...PAYLOAD.regions[0], state: 'fine' }] };
    expect(() => parseRegions(bad)).toThrow(/state/);
  });

  // Car E of design 62de32ae: the server says how many of a region's
  // own members stand at each of its places (decision 5), and names
  // the plant that serves every region (decision 11).
  it('parses the places a region counts and the plant that serves every region', () => {
    const withBoth = {
      ...PAYLOAD,
      regions: PAYLOAD.regions.map((r) =>
        r.name === 'shed'
          ? { ...r, places: [{ name: 'inspection-shed', count: 1 }, { name: 'siding-event', count: 1 }] }
          : r,
      ),
      plant: [{ id: 'runner:host:forge', name: 'forge runner', state: 'idle', why: 'last df answered exit 0' }],
    };
    const m = parseRegions(withBoth);
    expect(m.regions[3]!.places).toEqual([
      { name: 'inspection-shed', count: 1 },
      { name: 'siding-event', count: 1 },
    ]);
    expect(m.regions[0]!.places).toEqual([]);
    expect(m.plant.map((p) => p.id)).toEqual(['runner:host:forge']);
    // An older server sends neither: no places, and no plant — never
    // invented idle machines.
    expect(parseRegions(PAYLOAD).plant).toEqual([]);
    // A plant machine in a state this client does not know is refused,
    // as a region's is.
    expect(() => parseRegions({ ...withBoth, plant: [{ id: 'x', name: 'x', state: 'fine', why: '' }] })).toThrow(/state/);
  });
});

describe('the names are the server\'s, in map order', () => {
  it('equal boss_jobs::regions::REGIONS (crates/core/boss-jobs/src/regions/mod.rs)', () => {
    // A fact that lives twice gets an equality test (CLAUDE.md §9a): the
    // server's constant is the decision (0524fc95 Q2); this list is the
    // client's copy so the map can draw a card per name before the read
    // answers, and so a ninth name from a newer server is noticed.
    const src = readFileSync(
      join(import.meta.dir, '..', '..', '..', '..', '..', 'crates', 'core', 'boss-jobs', 'src', 'regions', 'mod.rs'),
      'utf8',
    );
    const block = src.match(/pub const REGIONS: \[&str; (\d+)\] = \[([^\]]*)\];/);
    expect(block, 'boss_jobs::regions::REGIONS is where the names live').not.toBeNull();
    const names = [...block![2]!.matchAll(/"([a-z-]+)"/g)].map((m) => m[1]!);
    expect(Number(block![1])).toBe(names.length);
    expect([...REGION_NAMES] as string[]).toEqual(names);
  });
});

describe('floorHref — every card is a door to a floor that already exists', () => {
  it('every region opens the world zoomed into it — the six on their yard panel, the two queue boards on their board', () => {
    expect(floorHref('dock')).toBe('/it/yard/dock');
    expect(floorHref('gates')).toBe('/it/yard/gates');
    expect(floorHref('track')).toBe('/it/yard/track');
    expect(floorHref('shed')).toBe('/it/yard/shed');
    expect(floorHref('arrivals')).toBe('/it/yard/arrivals');
    expect(floorHref('garage')).toBe('/it/yard/garage');
    // Car 4 of design d2154293: these two were the only cards that
    // left the world. They no longer do — their board mounts under
    // the zoomed territory, like every other floor.
    expect(floorHref('receiving')).toBe('/it/yard/receiving');
    expect(floorHref('marshalling')).toBe('/it/yard/marshalling');
    // The shop floor's board is the crew board, which was the floor
    // before the region existed (backlog 94c6ffd0).
    expect(floorHref('shop-floor')).toBe('/it/yard/shop-floor');
  });

  it('a name this client does not know still opens the yard, never a dead link', () => {
    expect(floorHref('siding')).toBe('/it/yard');
  });
});

describe('floorSelection — the yard panel a floor route opens on', () => {
  it('maps each yard region to the selection key the yard already speaks', () => {
    expect(floorSelection('dock')).toBe('dock');
    expect(floorSelection('gates')).toBe('approach');
    expect(floorSelection('track')).toBe('track');
    expect(floorSelection('shed')).toBe('inspection-shed');
    expect(floorSelection('arrivals')).toBe('arrivals');
    expect(floorSelection('garage')).toBe('garage');
  });

  it('falls back to the track for anything else — a key from a future map must not throw the page', () => {
    expect(floorSelection('receiving')).toBe('track');
    expect(floorSelection('')).toBe('track');
  });
});

describe('countText — what is here, against its bound when it has one', () => {
  it('prints the count, the count over its bound, or "no reading" for a null count', () => {
    expect(countText(region({ count: 3, bound: null }))).toBe('3');
    expect(countText(region({ count: 1, bound: 3 }))).toBe('1 / 3');
    expect(countText(region({ count: 0, bound: 1 }))).toBe('0 / 1');
    expect(countText(region({ count: null, bound: 3 }))).toBe('no reading');
  });
});

describe('trendText — this window against the previous, in the unit', () => {
  it('prints both halves with the unit and the sample counts', () => {
    expect(trendText(trend())).toBe('4.3 vs 3 hours · n=6 / 5');
    expect(trendText(trend({ unit: 'per day', current: 17, previous: 12, samples: 17, previous_samples: 12 }))).toBe(
      '17 vs 12 per day · n=17 / 12',
    );
  });

  it('a half nobody measured is a dash, never a zero', () => {
    expect(trendText(trend({ current: null, samples: 0 }))).toBe('— vs 3 hours · n=0 / 5');
    expect(trendText(trend({ current: null, previous: null, samples: 0, previous_samples: 0 }))).toBe('no reading');
  });
});

describe('lampOf — the yard\'s own lamp for a state', () => {
  it('clear is ok, attention is warn, troubled is err', () => {
    expect(lampOf('clear')).toBe('ok');
    expect(lampOf('attention')).toBe('warn');
    expect(lampOf('troubled')).toBe('err');
  });
});

// Design 62de32ae, "The IT map, round 3: meaning before drawing" — car A:
// one vocabulary, every non-clear state with the declared band that
// decided it and how long it has held, every count and KPI with its unit.
describe('one state vocabulary, each state with its band (62de32ae decisions 1, 2, 5, 9)', () => {
  it('the three words are the server\'s RegionState, in its order (CLAUDE.md §9a)', () => {
    const src = readFileSync(
      join(import.meta.dir, '..', '..', '..', '..', '..', 'crates', 'core', 'boss-jobs', 'src', 'regions', 'mod.rs'),
      'utf8',
    );
    const block = src.match(/pub enum RegionState \{([^}]*)\}/);
    expect(block, 'boss_jobs::regions::RegionState is where the words live').not.toBeNull();
    const words = [...block![1]!.matchAll(/^\s*([A-Z][a-z]+),/gm)].map((m) => m[1]!.toLowerCase());
    expect(words).toEqual(['clear', 'attention', 'troubled']);
  });

  it('refuses the retired word rather than drawing a new meaning under it', () => {
    const old = { ...PAYLOAD, regions: [{ ...PAYLOAD.regions[0], state: 'busy' }] };
    expect(() => parseRegions(old)).toThrow(/state/);
    const kind = { ...PAYLOAD, regions: [{ ...PAYLOAD.regions[0], bound_kind: 'quota' }] };
    expect(() => parseRegions(kind)).toThrow(/bound kind/);
  });

  it('parses the band, the unit, the bound kind and the KPI the server sends', () => {
    const wire = {
      ...PAYLOAD,
      regions: [
        {
          ...PAYLOAD.regions[6],
          bound_kind: null,
          unit: 'packets standing',
          band: {
            id: 'receiving-aging',
            reads: 'oldest 5d > the 3-day triage band',
            hold_minutes: 0,
            since: '2026-09-18T00:00:00+00:00',
            held_minutes: 2160,
            held: '36h',
          },
          kpi: [{ name: 'oldest untriaged', value: 5, unit: 'days', text: 'oldest untriaged 5 days' }],
        },
      ],
    };
    const r = parseRegions(wire).regions[0]!;
    expect(r.unit).toBe('packets standing');
    expect(r.band?.reads).toBe('oldest 5d > the 3-day triage band');
    expect(stateText(r)).toBe('attention for 36h');
    expect(bandText(r)).toBe('oldest 5d > the 3-day triage band');
    expect(kpiText(r)).toBe('oldest untriaged 5 days');
    expect(countText(r)).toBe('4 packets standing');
  });

  it('a clear state, or one with no onset on record, is the bare word — never a made-up duration', () => {
    expect(stateText(region())).toBe('clear');
    expect(bandText(region())).toBeNull();
    const noOnset = region({
      state: 'attention',
      band: { id: 'track-gate-waiting', reads: 'a train gate not filed for 10m', hold_minutes: 10, since: null, held_minutes: null, held: null },
    });
    expect(stateText(noOnset)).toBe('attention');
    expect(stateText(undefined)).toBe('troubled');
  });

  it('a threshold is never drawn as room: the dock reads "6 cars parked · threshold 1", the gates "3 / 3 bays in use"', () => {
    expect(countText(region({ count: 6, bound: 1, bound_kind: 'threshold', unit: 'cars parked' }))).toBe(
      '6 cars parked · threshold 1',
    );
    expect(countText(region({ count: 3, bound: 3, bound_kind: 'capacity', unit: 'bays in use' }))).toBe(
      '3 / 3 bays in use',
    );
    expect(compactCountText(region({ count: 6, bound: 1, bound_kind: 'threshold' }))).toBe('6 · threshold 1');
    expect(compactCountText(region({ count: 3, bound: 3, bound_kind: 'capacity' }))).toBe('3 / 3');
    expect(compactCountText(region({ count: null }))).toBe('no reading');
  });
});
