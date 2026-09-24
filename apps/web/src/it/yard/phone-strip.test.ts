import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { parseBorders, type Border, type Borders } from './borders';
import { REGION_NAMES, parseRegions, type Region, type Regions } from './regions';
import { BORDERS } from './world';
import { PHONE_MAX_WIDTH, PHONE_QUERY, railLine, stripGroups, verdictOf } from './phone-strip';

// THE PHONE STRIP MAP (design 62de32ae decision 12, car G): one row
// per region in flow order, grouped under the three thirds, each row
// the state, the KPI and the in-rail's waiting count and rate. These pin
// the grouping (the server's `thirds` block, car F — the strip holds no
// copy of it), that no region is dropped or drawn twice, and the words a
// rail prints — an unread rail is "no reading", never 0.
//
// EVERY FIXTURE IS A WIRE PAYLOAD, PARSED. This file built `Regions` as
// a TypeScript literal twice and turned two trains red for it (17:17
// and 18:31 on 2026-09-24): car F added `thirds` and `machines` to the
// type, car E added `plant`, and each literal stopped type-checking on
// the assembled tree. A payload goes through `parseRegions` /
// `parseBorders` — the one door the page reads through, and the way
// hud.test.ts builds its own — so the next field the type gains arrives
// here as the parser's own default for an older server, and only a field
// this file asserts on is ever spelled in it.

type Wire = Readonly<Record<string, unknown>>;

const trend = { metric: 'm', unit: 'per day', current: null, previous: null, samples: 0, previous_samples: 0 };

const wireRegion = (name: string, over: Wire = {}): Wire => ({ name, count: 1, state: 'clear', why: 'fine', trend, ...over });

/** A third as the server sends it; only its name and regions matter to
 *  the strip, the rest is the HUD's. */
const wireThird = (third: string, regions: ReadonlyArray<string>): Wire => ({
  third,
  regions,
  balance: { unit: 'u', in_means: '', out_means: '', in: null, out: null, net: null, in_count: null, out_count: null },
  stuck: { stuck: 0, waiting: 0, unknown: [], oldest_hours: null, regions: [] },
});

/** The partition the server sends (boss_jobs::regions::THIRDS). */
const SERVED: ReadonlyArray<Wire> = [
  wireThird('queue-management', ['receiving', 'marshalling']),
  wireThird('actors-building', ['shop-floor', 'gates', 'garage']),
  wireThird('delivery', ['dock', 'track', 'arrivals', 'shed', 'publish']),
];

/** The regions read, through the page's own parser. */
const regions = (over: Wire = {}): Regions =>
  parseRegions({
    window_hours: 24,
    now: '2026-09-24T12:00:00Z',
    regions: REGION_NAMES.map((n) => wireRegion(n)),
    thirds: SERVED,
    ...over,
  });

/** One region, parsed the same way. */
const region = (name: string, over: Wire = {}): Region => regions({ regions: [wireRegion(name, over)] }).regions[0]!;

const wireBorder = (from: string, to: string, over: Wire = {}): Wire => ({
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

/** The borders read, through the page's own parser. */
const borders = (rows: ReadonlyArray<Wire>): Borders => parseBorders({ window_hours: 24, borders: rows, now: '' });

/** One border, parsed the same way. */
const border = (from: string, to: string, over: Wire = {}): Border => borders([wireBorder(from, to, over)]).borders[0]!;

const names = (groups: ReturnType<typeof stripGroups>) =>
  groups.map((g) => [g.third, g.rows.map((r) => r.name)] as const);

describe('the strip’s rows', () => {
  it('groups by the server’s thirds, in their order and each third’s own order', () => {
    expect(names(stripGroups(regions(), null))).toEqual([
      ['queue-management', ['receiving', 'marshalling']],
      ['actors-building', ['shop-floor', 'gates', 'garage']],
      ['delivery', ['dock', 'track', 'arrivals', 'shed', 'publish']],
    ]);
    // The server's block decides, whatever it says — none of it is ours.
    expect(names(stripGroups(regions({ thirds: [wireThird('the rest', ['dock', 'receiving'])] }), null))[0]).toEqual([
      'the rest',
      ['dock', 'receiving'],
    ]);
  });

  it('draws every region once, and a region no third names under its own heading in the layout’s order', () => {
    const partial = [wireThird('queue-management', ['receiving', 'marshalling']), wireThird('delivery', ['dock'])];
    const groups = stripGroups(regions({ thirds: partial }), null);
    const drawn = groups.flatMap((g) => g.rows.map((r) => r.name));
    expect([...drawn].sort()).toEqual([...REGION_NAMES].sort());
    expect(new Set(drawn).size).toBe(drawn.length);
    const last = groups[groups.length - 1]!;
    expect(last.third).toBe('unplaced');
    expect(last.label).toBe('In no third');
    // The line as a car walks it, then the sidings — not the payload's order.
    expect(last.rows.map((r) => r.name)).toEqual(['shop-floor', 'gates', 'track', 'arrivals', 'shed', 'garage', 'publish']);
  });

  it('a server older than the thirds block still draws every region, under one heading that says so', () => {
    const groups = stripGroups(regions({ thirds: undefined }), null);
    expect(groups.map((g) => g.third)).toEqual(['unplaced']);
    expect(groups[0]!.rows).toHaveLength(REGION_NAMES.length);
  });

  it('a region a third names that the payload does not carry is a row with no reading, never dropped', () => {
    const groups = stripGroups(regions({ regions: [wireRegion('dock')] }), null);
    const rows = groups.flatMap((g) => g.rows);
    expect(rows).toHaveLength(REGION_NAMES.length);
    expect(rows.find((r) => r.name === 'dock')!.region?.name).toBe('dock');
    expect(rows.find((r) => r.name === 'gates')!.region).toBeUndefined();
  });

  it('each row carries the rails INTO its region, in the borders’ order; the garage has two', () => {
    const b = borders(BORDERS.map((x) => wireBorder(x.from, x.to)));
    const rows = stripGroups(regions(), b).flatMap((g) => g.rows);
    const into = (name: string): ReadonlyArray<Border> => {
      const rails = rows.find((r) => r.name === name)!.rails;
      if (rails === null) throw new Error(`${name}: the rails were read, yet the row says unread`);
      return rails;
    };
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
    // The page, and the HUD frame above the map (car F), which stacks its
    // rows at the width the strip takes over.
    for (const file of ['MapPage.svelte', 'HudFrame.svelte']) {
      const src = readFileSync(join(import.meta.dir, file), 'utf8');
      const queries = [...src.matchAll(/@media\s*(\([^)]*\))/g)].map((m) => m[1]);
      expect({ file, queries }).toEqual({ file, queries: [PHONE_QUERY] });
    }
  });
});
