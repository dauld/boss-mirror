import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { Border, Borders } from './borders';
import { REGION_NAMES, type Region, type Regions } from './regions';
import { BORDERS } from './world';
import {
  LAYOUT_THIRDS,
  PHONE_MAX_WIDTH,
  PHONE_QUERY,
  railLine,
  stripGroups,
  thirdsOf,
  verdictOf,
} from './phone-strip';

// THE PHONE STRIP MAP (design 62de32ae decision 12, car G): one row
// per region in flow order, grouped under the three thirds, each row
// the state, the KPI and the in-rail's waiting count and rate. These pin
// the grouping (the server's partition wins; the layout's stands in
// until it is sent), that no region is dropped or drawn twice, and the
// words a rail prints — an unread rail is "no reading", never 0.

const trend = { metric: 'm', unit: 'per day', current: null, previous: null, samples: 0, previous_samples: 0 };

const region = (name: string, over: Partial<Region> = {}): Region => ({
  name,
  count: 1,
  bound: null,
  bound_kind: null,
  unit: '',
  state: 'clear',
  why: 'fine',
  band: null,
  trend,
  kpi: [],
  machines: [],
  ...over,
});

const regions = (over: Partial<Regions> = {}): Regions => ({
  window_hours: 24,
  now: '2026-09-24T12:00:00Z',
  regions: REGION_NAMES.map((n) => region(n)),
  ...over,
});

const border = (from: string, to: string, over: Partial<Border> = {}): Border => ({
  from,
  to,
  crossing: 'a packet crossed',
  rate: { metric: 'crossings', unit: 'per day', current: 2, previous: 3, samples: 2, previous_samples: 3 },
  last_crossed: null,
  waiting: 0,
  holds: [],
  machine: { name: 'm', kind: 'actors', last_fired: null, silent_for_minutes: null, expected_every_minutes: null, silent: null, why: '' },
  state: 'clear',
  why: '',
  ...over,
});

const borders = (rows: ReadonlyArray<Border>): Borders => ({ window_hours: 24, borders: rows, now: '' });

describe('the thirds a strip groups under', () => {
  it('the layout partition places every region exactly once, in flow order', () => {
    const placed = LAYOUT_THIRDS.flatMap((t) => t.regions);
    expect([...placed].sort()).toEqual([...REGION_NAMES].sort());
    expect(new Set(placed).size).toBe(placed.length);
    expect(LAYOUT_THIRDS.map((t) => t.third)).toEqual(['queue-management', 'actors-building', 'delivery']);
    // The line's order, as a car walks it: shop floor -> gates -> dock ->
    // track -> arrivals -> shed (decision 3); the garage hangs under the
    // gates, publish off arrivals.
    expect(placed).toEqual([
      'receiving', 'marshalling', 'shop-floor', 'gates', 'garage', 'dock', 'track', 'arrivals', 'shed', 'publish',
    ]);
  });

  it('reads the server’s thirds when the payload carries them, and says which it used', () => {
    const served = [
      { third: 'queue-management', regions: ['receiving'] },
      { third: 'the rest', regions: ['marshalling', 'dock'] },
    ];
    const got = thirdsOf({ ...regions(), thirds: served } as Regions);
    expect(got.source).toBe('server');
    expect(got.thirds).toEqual(served);
  });

  it('stands in the layout partition for a server that sends none, or a malformed block', () => {
    expect(thirdsOf(regions()).source).toBe('layout');
    expect(thirdsOf({ ...regions(), thirds: [] } as Regions).source).toBe('layout');
    expect(thirdsOf({ ...regions(), thirds: [{ third: 7 }] } as unknown as Regions).source).toBe('layout');
  });
});

describe('the strip’s rows', () => {
  it('draws every region once, grouped, and a region no third names under its own heading', () => {
    const served = [
      { third: 'queue-management', regions: ['receiving', 'marshalling'] },
      { third: 'delivery', regions: ['dock'] },
    ];
    const groups = stripGroups({ ...regions(), thirds: served } as Regions, null);
    const drawn = groups.flatMap((g) => g.rows.map((r) => r.name));
    expect([...drawn].sort()).toEqual([...REGION_NAMES].sort());
    expect(new Set(drawn).size).toBe(drawn.length);
    const last = groups[groups.length - 1]!;
    expect(last.third).toBe('unplaced');
    // In the line's own order, not the payload's.
    expect(last.rows.map((r) => r.name)).toEqual(['shop-floor', 'gates', 'garage', 'track', 'arrivals', 'shed', 'publish']);
  });

  it('a region a third names that the payload does not carry is a row with no reading, never dropped', () => {
    const groups = stripGroups(regions({ regions: [region('dock')] }), null);
    const rows = groups.flatMap((g) => g.rows);
    expect(rows).toHaveLength(REGION_NAMES.length);
    expect(rows.find((r) => r.name === 'dock')!.region?.name).toBe('dock');
    expect(rows.find((r) => r.name === 'gates')!.region).toBeUndefined();
  });

  it('each row carries the rails INTO its region, in the borders’ order; the garage has two', () => {
    const b = borders(BORDERS.map((x) => border(x.from, x.to)));
    const rows = stripGroups(regions(), b).flatMap((g) => g.rows);
    const into = (name: string) => rows.find((r) => r.name === name)!.rails;
    expect(into('receiving')).toEqual([]);
    expect(into('dock').map((r) => r.from)).toEqual(['gates']);
    expect(into('garage').map((r) => r.from)).toEqual(['gates', 'track']);
    // Unread rails are said per row, not left blank.
    const unread = stripGroups(regions(), null).flatMap((g) => g.rows);
    expect(unread.every((r) => r.rails === null)).toBe(true);
  });
});

describe('the rail words', () => {
  it('prints what waits and the rate, per day', () => {
    expect(railLine(border('dock', 'track', { waiting: 3, rate: { ...border('a', 'b').rate, current: 24 } }))).toBe(
      'from dock · 3 waiting · 24 /day',
    );
    expect(railLine(border('gates', 'dock'))).toBe('from gates · nothing waiting · 2 /day');
  });

  it('an unmeasured rail reads no reading, never zero', () => {
    const blind = border('receiving', 'marshalling', {
      waiting: null,
      rate: { metric: 'crossings', unit: 'per day', current: null, previous: null, samples: 0, previous_samples: 0 },
    });
    expect(railLine(blind)).toBe('from receiving · waiting: no reading · rate: no reading');
  });
});

describe('a row’s verdict', () => {
  const band = { id: 'b', reads: 'oldest 5d > the 3-day triage band', hold_minutes: 0, since: null, held_minutes: null, held: null };
  it('is silent when clear, the band when attention, the band and the why when troubled', () => {
    expect(verdictOf(region('dock'))).toEqual([]);
    expect(verdictOf(region('receiving', { state: 'attention', band }))).toEqual(['oldest 5d > the 3-day triage band']);
    expect(verdictOf(region('gates', { state: 'troubled', why: 'a corpse in a bay', band }))).toEqual([
      'oldest 5d > the 3-day triage band',
      'a corpse in a bay',
    ]);
  });
  it('is the why on a payload with no band, and says so for a region the read did not carry', () => {
    expect(verdictOf(region('marshalling', { state: 'troubled', why: 'the registry could not be read' }))).toEqual([
      'the registry could not be read',
    ]);
    expect(verdictOf(undefined)).toEqual(['the regions read answered nothing for this region']);
  });
});

describe('the phone breakpoint lives once', () => {
  // A fact that lives twice gets an equality test (CLAUDE.md 9a): the
  // media query that collapses the shell is CSS, the one that swaps the
  // world for the strip is script, and a phone between the two numbers
  // would get a strip inside a desktop shell or a world squeezed into a
  // phone one.
  it('the shell collapses at the same width the strip takes over', () => {
    expect(PHONE_QUERY).toBe(`(max-width: ${PHONE_MAX_WIDTH}px)`);
    const styles = readFileSync(join(import.meta.dir, '..', '..', 'styles.css'), 'utf8');
    const shell = [...styles.matchAll(/@media\s*(\([^)]*\))\s*\{[^@]*?\.app-shell\b/g)].map((m) => m[1]);
    expect(shell).toEqual([PHONE_QUERY]);
    const page = readFileSync(join(import.meta.dir, 'MapPage.svelte'), 'utf8');
    const pageQueries = [...page.matchAll(/@media\s*(\([^)]*\))/g)].map((m) => m[1]);
    expect(pageQueries).toEqual([PHONE_QUERY]);
  });
});
