// THE DEPARTMENT MAP'S SELECTION — design e765b3fc, car N1. What the
// `?at=` query selects, read against the regions the server served:
// nothing, one of its stations, or a name the map does not carry —
// which is SAID, never an empty panel that reads as a quiet station.
import { describe, expect, it } from 'bun:test';
import { markOf, selectionOf, unreadStationOf } from './selection';
import { sectionHref } from './regions';
import type { Border, Borders } from './borders';
import type { Region, Regions } from './regions';
import { parseRoute } from '../../router';

const region = (name: string, over: Partial<Region> = {}): Region => ({
  name,
  count: 3,
  bound: 3,
  bound_kind: 'capacity',
  unit: 'bays in use',
  state: 'clear',
  why: '3 of 3 bays in use',
  band: null,
  trend: { metric: 'gate duration', unit: 'minutes', current: 11, previous: 9, samples: 5, previous_samples: 4 },
  kpi: [],
  machines: [],
  places: [],
  ...over,
});

const REGIONS: Regions = {
  window_hours: 24,
  now: '2026-09-25T17:21:00Z',
  regions: [region('dock', { count: 6, bound: null, bound_kind: null, unit: 'cars parked' }), region('gates')],
  thirds: [],
  machines: null,
  plant: [],
};

describe('selectionOf — what `?at=` selects on the map', () => {
  it('selects nothing when the query names nothing', () => {
    expect(selectionOf(undefined, REGIONS, BORDERS)).toEqual({ kind: 'none' });
    expect(selectionOf('', REGIONS, BORDERS)).toEqual({ kind: 'none' });
  });

  it('selects a station the server served, carrying the server\'s own row and a title for it', () => {
    const s = selectionOf('gates', REGIONS, BORDERS);
    expect(s.kind).toBe('station');
    if (s.kind !== 'station') return;
    expect(s.name).toBe('gates');
    expect(s.title).toBe('Gates');
    expect(s.region).toBe(REGIONS.regions[1]!);
  });

  it('a name the read did not carry is its own answer — never an empty panel', () => {
    // A layout name the server did not serve this time, a section the
    // borders read did not carry, and a typo all land here, named.
    expect(selectionOf('publish', REGIONS, BORDERS)).toEqual({ kind: 'unknown', at: 'publish' });
    expect(selectionOf('track->dock', REGIONS, BORDERS)).toEqual({ kind: 'unknown', at: 'track->dock' });
    expect(selectionOf('gatez', REGIONS, BORDERS)).toEqual({ kind: 'unknown', at: 'gatez' });
  });
});

// A SECTION is a selection too (car N2): "I can click a line segment and
// see info on how it is performing" (David, added_2026_09_25_david_2).
// `?at=<from>-><to>` names it; the arrow the design wrote, `→`, reads the
// same, so a link copied out of the design opens the section it names.
const border = (from: string, to: string): Border => ({
  from,
  to,
  crossing: 'a car boarded a train',
  rate: { metric: 'crossings', unit: 'per day', current: 121, previous: 110, samples: 121, previous_samples: 110 },
  last_crossed: '2026-09-25T17:02:00Z',
  waiting: 5,
  holds: [],
  holds_by_class: { machine: 5, person: 0, unknown: 0, stuck: 0 },
  flowing: true,
  held_since: null,
  flowing_why: 'last crossed 19m ago',
  machine: { name: 'train-board-on-dock-depth', kind: 'dispatcher-rule', last_fired: null, silent_for_minutes: null, expected_every_minutes: null, silent: null, why: '' },
  state: 'clear',
  why: 'flowing',
});

const BORDERS: Borders = { window_hours: 24, now: REGIONS.now, borders: [border('gates', 'dock'), border('dock', 'track')] };

describe('selectionOf — a section of track', () => {
  it('selects the border the server served, with both ends and a title in words', () => {
    const s = selectionOf('dock->track', REGIONS, BORDERS);
    expect(s.kind).toBe('section');
    if (s.kind !== 'section') return;
    expect(s.from).toBe('dock');
    expect(s.to).toBe('track');
    expect(s.key).toBe('dock->track');
    expect(s.title).toBe('Dock → Track');
    expect(s.border).toBe(BORDERS.borders[1]!);
  });

  it('reads the design\'s arrow as the same section, and keys it one way', () => {
    const s = selectionOf('dock→track', REGIONS, BORDERS);
    expect(s.kind === 'section' ? s.key : s.kind).toBe('dock->track');
  });

  it('while the borders are unread, a section is selected with no reading — never an unknown name', () => {
    // The map says the rails could not be read; the panel must not add
    // that the section does not exist.
    const s = selectionOf('dock->track', REGIONS, null);
    expect(s.kind).toBe('section');
    if (s.kind !== 'section') return;
    expect(s.border).toBeNull();
  });

  it('a half-written section is a name the map does not carry', () => {
    expect(selectionOf('dock->', REGIONS, BORDERS)).toEqual({ kind: 'unknown', at: 'dock->' });
    expect(selectionOf('->track', REGIONS, BORDERS)).toEqual({ kind: 'unknown', at: '->track' });
  });
});

describe('sectionHref — the link a section selects itself by', () => {
  it('is the Department Map with the section in the query, round-tripping through the router', () => {
    expect(sectionHref('dock', 'track')).toBe('/it?at=dock-%3Etrack');
    expect(parseRoute('/it', '?at=dock-%3Etrack')).toEqual({ kind: 'systemYard', at: 'dock->track' });
  });

  it('is the key the map marks the selection by, in the map\'s own arrow', () => {
    expect(markOf({ kind: 'none' })).toBeNull();
    expect(markOf(selectionOf('gates', REGIONS, BORDERS))).toBe('gates');
    expect(markOf(selectionOf('dock->track', REGIONS, BORDERS))).toBe('dock→track');
    expect(markOf(selectionOf('gatez', REGIONS, BORDERS))).toBeNull();
  });
});

// A STATION WHOSE READING FAILED STILL OPENS (design e765b3fc, car N3).
// The floor pages at /it/yard/<region> drew their boards whatever the
// regions read did, because each board reads its own endpoints; the
// retired page's content lives in the station's panel now, and a failed
// regions read must not take the boards down with the readings it could
// not take. So a name that IS a station opens its panel on the name
// alone — the readings said to be unread, the boards reading their own.
describe('unreadStationOf — a station named while the regions read is out', () => {
  it('names a station the map lays out, titled as its panel titles it', () => {
    expect(unreadStationOf('marshalling')).toEqual({ name: 'marshalling', title: 'Marshalling' });
    expect(unreadStationOf('shop-floor')).toEqual({ name: 'shop-floor', title: 'Shop floor' });
  });

  it('names nothing for no selection, a section, or a name that is no station', () => {
    expect(unreadStationOf(undefined)).toBeNull();
    expect(unreadStationOf('')).toBeNull();
    expect(unreadStationOf('dock->track')).toBeNull();
    expect(unreadStationOf('gatez')).toBeNull();
  });
});
