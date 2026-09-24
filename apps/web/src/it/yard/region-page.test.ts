import { describe, expect, it } from 'bun:test';
import type { Border, Borders } from './borders';
import type { Alert } from './yard-alerts';
import type { BoardRow, Station } from './yard-floor';
import type { Region } from './regions';
import type { Platform } from './world-interior';
import {
  alertsFor,
  boardFor,
  drawnNote,
  quietText,
  railLines,
  railsText,
  regionRails,
  regionTitle,
} from './region-page';

// A REGION OWNS ITS PAGE (design 62de32ae, decision 7). The review of
// 2026-09-24 (finding 8) measured every floor region mounting the WHOLE
// yard's deck: the garage's departure board listed dock cars and
// trains at CI, the heading read "The IT world" inside every region,
// the world's summary line ("1516 crossings · 666 waiting") sat between
// the region's map and its floor, and "No alerts — every machine is
// working or idle by design" stood directly under a TROUBLED shed. The
// view swapped; the page around it did not. These pin the words and the
// selections a region page is made of, so the page derives none.

const rate = (current: number | null) => ({
  metric: 'crossings',
  unit: 'per day',
  current,
  previous: current,
  samples: current ?? 0,
  previous_samples: current ?? 0,
});

const border = (from: string, to: string, over: Partial<Border> = {}): Border => ({
  from,
  to,
  crossing: 'a packet crossed',
  rate: rate(4),
  last_crossed: null,
  waiting: 0,
  holds: [],
  machine: {
    name: 'auto-park-on-gate-green',
    kind: 'dispatcher-rule',
    last_fired: '2026-09-24T14:00:00Z',
    silent_for_minutes: 7,
    expected_every_minutes: null,
    silent: false,
    why: '',
  },
  state: 'clear',
  why: 'nothing waiting',
  ...over,
});

const borders = (rows: ReadonlyArray<Border>): Borders => ({ window_hours: 24, borders: rows, now: '' });

describe('regionTitle — the region as a heading names it', () => {
  it('reads a region name as words', () => {
    expect(regionTitle('dock')).toBe('Dock');
    expect(regionTitle('shop-floor')).toBe('Shop floor');
  });
});

describe("regionRails — the region's own borders, in place of the world's summary", () => {
  const world = borders([
    border('gates', 'dock', { rate: rate(175), waiting: 6 }),
    border('dock', 'track', {
      waiting: 2,
      machine: {
        name: 'train-board-on-dock-depth',
        kind: 'cadence',
        last_fired: null,
        silent_for_minutes: 180,
        expected_every_minutes: 30,
        silent: true,
        why: '',
      },
      state: 'troubled',
      why: 'train-board-on-dock-depth silent for 180m',
    }),
    border('track', 'arrivals'),
  ]);

  it('takes the rails into and out of the region and no other, in first', () => {
    const rails = regionRails(world, 'dock');
    expect(rails.map((r) => `${r.side}:${r.other}`)).toEqual(['in:gates', 'out:track']);
  });

  it('says what crosses, what waits and the machine, and a troubled rail says why', () => {
    const [into, out] = railsText(regionRails(world, 'dock'));
    expect(into).toBe('in from gates · 175/day · 6 waiting · auto-park-on-gate-green · fired 7m ago');
    expect(out).toBe(
      'out to track · 4/day · 2 waiting · train-board-on-dock-depth · SILENT 180m · troubled — train-board-on-dock-depth silent for 180m',
    );
  });

  it('reads an unmeasured rate and an uncounted queue as unknown, never as zero', () => {
    const blind = borders([border('receiving', 'marshalling', { rate: rate(null), waiting: null })]);
    const [line] = railsText(regionRails(blind, 'marshalling'));
    expect(line).toContain('rate: no reading');
    expect(line).toContain('waiting: no reading');
    expect(line).not.toMatch(/\b0\b/);
  });

  it('says so when no rail touches the region, rather than printing nothing', () => {
    expect(railsText(regionRails(world, 'publish'))).toEqual([]);
    expect(railLines(world, 'publish')).toEqual(['no rail enters or leaves the publish region on the map']);
    expect(railLines(world, 'dock')).toEqual(railsText(regionRails(world, 'dock')));
  });
});

// The board and the strip are SCOPED, and what they leave out is
// COUNTED — a region's panel that quietly drops the rest of the floor
// would read as a floor with nothing else on it.
const wagon = (id: string, station: Station) => ({ id, station });
const row = (id: string, landed = false): BoardRow => ({ id, where: id, landed });
const floor = {
  wagons: [
    wagon('parked', 'dock'),
    wagon('aboard', 'train'),
    wagon('in-bay', 'gate'),
    wagon('landed', 'arrivals'),
    wagon('red', 'garage'),
  ],
  boardRows: [row('in-bay'), row('parked'), row('aboard'), row('red'), row('landed', true)],
};

describe("boardFor — the departure board filtered to the region's stations", () => {
  it('keeps the rows standing in the region, in the board order, and counts the rest', () => {
    const dock = boardFor(floor, 'dock');
    expect(dock.rows.map((r) => r.id)).toEqual(['parked']);
    expect(dock.elsewhere).toBe(4);
  });

  it('keeps the landed rows on the arrivals board, and a gate bay on the gates board', () => {
    expect(boardFor(floor, 'arrivals').rows.map((r) => r.id)).toEqual(['landed']);
    expect(boardFor(floor, 'gates').rows.map((r) => r.id)).toEqual(['in-bay']);
  });

  it('keeps a row whose wagon it cannot place, rather than dropping it', () => {
    const orphan = { ...floor, boardRows: [...floor.boardRows, row('ghost')] };
    expect(boardFor(orphan, 'dock').rows.map((r) => r.id)).toEqual(['parked', 'ghost']);
  });
});

const alert = (subject: string, sev: 'err' | 'warn' = 'warn'): Alert => ({
  id: subject,
  subject,
  sev,
  text: subject,
  since: null,
});

describe("alertsFor — the alerts strip scoped to the region's floor", () => {
  const strip = [
    alert('train:t1', 'err'),
    alert('conductor', 'err'),
    alert('cluster', 'err'),
    alert('runner'),
    alert('bay:0'),
    alert('approach'),
    alert('garage'),
    alert('car:parked'),
  ];

  it('places each alert in the region its subject stands in', () => {
    const at = (region: string) => alertsFor(strip, region, floor).here.map((a) => a.subject);
    expect(at('track')).toEqual(['train:t1', 'conductor']);
    expect(at('arrivals')).toEqual(['cluster', 'runner']);
    expect(at('gates')).toEqual(['bay:0', 'approach']);
    expect(at('garage')).toEqual(['garage']);
    expect(at('dock')).toEqual(['car:parked']);
    expect(at('shed')).toEqual([]);
  });

  it('counts what the strip leaves to the other regions', () => {
    expect(alertsFor(strip, 'garage', floor).elsewhere).toBe(7);
  });

  it('keeps on every strip an alert whose subject it cannot place', () => {
    const odd = [alert('something-new')];
    expect(alertsFor(odd, 'dock', floor).here).toHaveLength(1);
    expect(alertsFor(odd, 'shed', floor).here).toHaveLength(1);
  });
});

describe('quietText — a quiet strip cannot contradict the region above it', () => {
  it('a clear region: nothing here is failing', () => {
    expect(quietText('dock', 'clear', 0)).toBe('No alerts in the dock — every machine here is working or idle by design.');
  });

  it('a troubled region: no machine alert, and the state above is the region reading, not a machine', () => {
    const t = quietText('shed', 'troubled', 0);
    expect(t).not.toContain('every machine here is working');
    expect(t).toBe(
      "No machine alerts in the shed — its troubled state above is the region's own reading, not a machine's.",
    );
  });

  it('counts the alerts elsewhere on the floor', () => {
    expect(quietText('dock', 'clear', 2)).toContain('2 alerts elsewhere on the floor.');
    expect(quietText('dock', 'clear', 1)).toContain('1 alert elsewhere on the floor.');
  });

  it('claims nothing about a region whose state could not be read, or is still being read', () => {
    expect(quietText('dock', 'unread', 0)).toBe("No alerts in the dock — the region's own state could not be read.");
    expect(quietText('dock', 'reading', 0)).toBe('No alerts in the dock.');
  });
});

// DECISION 5: each interior draws the header's count, or names what it
// leaves out. Marshalling's platforms draw each STATION's depth, and a
// packet stands at every station whose predicate it matches — while
// the head counts each packet once, and only while it is marshalling's
// (decision 4). 517 standings over a head of 280 with nothing to say so
// is the contradiction the review measured.
const region = (over: Partial<Region> = {}): Region => ({
  name: 'marshalling',
  count: 280,
  bound: null,
  bound_kind: null,
  unit: 'packets at stations',
  state: 'clear',
  why: '',
  band: null,
  trend: rate(1),
  kpi: [],
  machines: [],
  ...over,
});

const platform = (key: string, standing: number | null): Platform => ({
  key,
  name: key,
  standing,
  bound: null,
  rate: null,
  flag: { from: 'tail', n: 0 },
  note: '',
});

describe('drawnNote — the interior against the head it sits under', () => {
  it('names the difference when the platforms stand more than the head counts', () => {
    const note = drawnNote(region(), [platform('q.platform-admin.task', 297), platform('a.platform-admin.opus-5-1m', 213), platform('sign-off', 7)]);
    expect(note).toBe(
      'the platforms stand 517 — the head counts 280 packets at stations: a packet stands at every station it matches, ' +
        'including one still in receiving or held by another region, and the head counts each once, only while it is marshalling’s',
    );
  });

  it('says nothing when the drawing is the head', () => {
    expect(drawnNote(region({ name: 'receiving', count: 3 }), [platform('feedback', 2), platform('session', 1)])).toBeNull();
  });

  it('a region the head could not count, or a platform nobody could, draws no comparison', () => {
    expect(drawnNote(region({ count: null }), [platform('a', 2)])).toBeNull();
    expect(drawnNote(region(), [platform('a', null), platform('b', 3)])).toBeNull();
  });

  it('a region without a reason of its own still names the two numbers', () => {
    expect(drawnNote(region({ name: 'shop-floor', count: 2, unit: 'runs in flight' }), [platform('s', 3)])).toBe(
      'the platforms stand 3 — the head counts 2 runs in flight',
    );
  });
});
