// THE DEPARTMENT MAP'S SELECTION — design e765b3fc, car N1. What the
// `?at=` query selects, read against the regions the server served:
// nothing, one of its stations, or a name the map does not carry —
// which is SAID, never an empty panel that reads as a quiet station.
import { describe, expect, it } from 'bun:test';
import { selectionOf } from './selection';
import type { Region, Regions } from './regions';

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
    expect(selectionOf(undefined, REGIONS)).toEqual({ kind: 'none' });
    expect(selectionOf('', REGIONS)).toEqual({ kind: 'none' });
  });

  it('selects a station the server served, carrying the server\'s own row and a title for it', () => {
    const s = selectionOf('gates', REGIONS);
    expect(s.kind).toBe('station');
    if (s.kind !== 'station') return;
    expect(s.name).toBe('gates');
    expect(s.title).toBe('Gates');
    expect(s.region).toBe(REGIONS.regions[1]!);
  });

  it('a name the read did not carry is its own answer — never an empty panel', () => {
    // A layout name the server did not serve this time, a section
    // (`dock→track`, which car N2 teaches the panel) and a typo all
    // land here, named.
    expect(selectionOf('publish', REGIONS)).toEqual({ kind: 'unknown', at: 'publish' });
    expect(selectionOf('dock→track', REGIONS)).toEqual({ kind: 'unknown', at: 'dock→track' });
    expect(selectionOf('gatez', REGIONS)).toEqual({ kind: 'unknown', at: 'gatez' });
  });
});
