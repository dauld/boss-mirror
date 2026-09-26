// THE SELECTION'S PANEL — design e765b3fc, car N2. What a station or a
// section says below the map: its rate, what waits and on whom, what is
// stuck, its trend, the band and the why that decided its state, its
// machines and its crossings. Every word is the server's reading turned
// into a sentence; nothing here counts or judges anything of its own,
// and a reading that was not taken is "no reading", never zero.
import { describe, expect, it } from 'bun:test';
import type { Border, Borders } from './borders';
import type { Region } from './regions';
import type { Route } from './routes';
import { MOVES_NOT_KEPT, NO_RATE_YET, STATION_BOARDS, boardsOf, classesText, sectionCells, stationCells, type PanelCell, type PanelField, type StationBoard } from './panel';

const NOW = '2026-09-25T17:21:00Z';

const border = (from: string, to: string, over: Partial<Border> = {}): Border => ({
  from,
  to,
  crossing: 'a run opened a gate',
  rate: { metric: 'crossings', unit: 'per day', current: 202, previous: 180, samples: 202, previous_samples: 180 },
  last_crossed: '2026-09-25T17:02:00Z',
  waiting: 3,
  holds: [{ what: 'feat/a-branch', why: 'in line for one of 3 bays' }],
  holds_by_class: { machine: 3, person: 0, unknown: 0, stuck: 0 },
  flowing: true,
  held_since: null,
  flowing_why: 'last crossed 19m ago, inside 4× its mean gap',
  machine: { name: 'boss gate', kind: 'actors', last_fired: null, silent_for_minutes: null, expected_every_minutes: null, silent: null, why: 'crossed by whoever runs the verb' },
  state: 'clear',
  why: 'flowing',
  ...over,
});

const gates: Region = {
  name: 'gates',
  count: 3,
  bound: 3,
  bound_kind: 'capacity',
  unit: 'bays in use',
  state: 'attention',
  why: '3 of 3 bays in use, 3 runs waiting for a slot',
  band: { id: 'gates-at-bound', reads: 'held 31m > the 30m bound', hold_minutes: 30, since: '2026-09-25T16:50:00Z', held_minutes: 31, held: '31m' },
  trend: { metric: 'gate duration', unit: 'minutes', current: 11, previous: 9, samples: 5, previous_samples: 4 },
  kpi: [{ name: 'bays', value: 3, unit: 'bays', text: '3 of 3 bays in use' }],
  machines: [{ id: 'gate-bay-1', name: 'bay 1', state: 'running', why: 'a gate-run holds it' }],
  places: [],
};

const BORDERS: Borders = {
  window_hours: 24,
  now: NOW,
  borders: [
    border('shop-floor', 'gates'),
    border('gates', 'dock', {
      crossing: 'a green parked a car',
      rate: { metric: 'crossings', unit: 'per day', current: 132, previous: 140, samples: 132, previous_samples: 140 },
      waiting: 2,
      holds: [],
      holds_by_class: { machine: 0, person: 1, unknown: 0, stuck: 1 },
    }),
    border('dock', 'track'),
  ],
};

const cell = (cells: ReadonlyArray<PanelCell>, field: PanelField): ReadonlyArray<string> =>
  cells.find((c) => c.field === field)?.lines ?? [`<no ${field} cell>`];

describe('stationCells — a station, read through its own sections', () => {
  const cells = stationCells(gates, BORDERS, NOW);

  it('carries every field the design names, in its order', () => {
    expect(cells.map((c) => c.field)).toEqual(['rate', 'waiting', 'stuck', 'trend', 'verdict', 'machines', 'crossings']);
  });

  it('rate: each section in and out, crossings a day and the headway — never another station\'s', () => {
    expect(cell(cells, 'rate')).toEqual(['in from shop floor: 202/d · 7m gap', 'out to dock: 132/d · 11m gap']);
  });

  it('waiting: on whom, by the server\'s own classes, with the holds it named and the count it did not list', () => {
    // The server bounds the LIST, never the count: what it counted and
    // did not name is said, so the panel never under-reports a queue.
    expect(cell(cells, 'waiting')).toEqual([
      'in from shop floor: 3 waiting — 3 on a machine',
      'feat/a-branch — in line for one of 3 bays',
      '+2 more waiting, not listed',
      'out to dock: 2 waiting — 1 on a person or the world · 1 stuck',
      '+2 more waiting, not listed',
    ]);
  });

  it('stuck: the stuck block\'s own members, section by section', () => {
    expect(cell(cells, 'stuck')).toEqual(['out to dock: 1 stuck']);
  });

  it('trend: the region\'s own metric against the window before, and its KPI in the server\'s words', () => {
    expect(cell(cells, 'trend')).toEqual(['gate duration: 11 vs 9 minutes · n=5 / 4', '3 of 3 bays in use']);
  });

  it('verdict: the state with how long it has held, the band that decided it, and the why', () => {
    expect(cell(cells, 'verdict')).toEqual([
      'attention for 31m',
      'held 31m > the 30m bound',
      '3 of 3 bays in use, 3 runs waiting for a slot',
    ]);
  });

  it('machines: what works here and what its state was read from', () => {
    expect(cell(cells, 'machines')).toEqual(['bay 1 · running — a gate-run holds it']);
  });

  it('recent crossings: the last crossing of each section, and says the per-packet list is not kept yet', () => {
    expect(cell(cells, 'crossings')).toEqual([
      'in from shop floor: 2026-09-25 17:02 UTC · 19m ago',
      'out to dock: 2026-09-25 17:02 UTC · 19m ago',
      MOVES_NOT_KEPT,
    ]);
  });

  it('a station the rails do not reach says so, rather than printing nothing', () => {
    const lone = stationCells({ ...gates, name: 'publish', machines: [] }, BORDERS, NOW);
    expect(cell(lone, 'rate')).toEqual(['no section enters or leaves publish on the map']);
    expect(cell(lone, 'stuck')).toEqual(['nothing stuck on its sections']);
    expect(cell(lone, 'machines')).toEqual(['no machine of ours works here']);
  });

  it('unread rails are "no reading" in every field that reads them — never an empty station', () => {
    const unread = stationCells(gates, null, NOW);
    for (const f of ['rate', 'waiting', 'stuck', 'crossings'] as const) {
      expect(cell(unread, f), f).toEqual(['no reading — the sections could not be read']);
    }
    // What the regions read carries still stands.
    expect(cell(unread, 'verdict')[0]).toBe('attention for 31m');
  });

  it('an unread queue on one section is "no reading" there, never 0', () => {
    const b: Borders = { ...BORDERS, borders: [border('shop-floor', 'gates', { waiting: null, holds_by_class: null, holds: [] })] };
    const c = stationCells(gates, b, NOW);
    expect(cell(c, 'waiting')).toEqual(['in from shop floor: waiting: no reading']);
    expect(cell(c, 'stuck')).toEqual(['in from shop floor: no reading']);
  });
});

describe('sectionCells — one section of track', () => {
  const b = BORDERS.borders[1]!;
  const cells = sectionCells(b, NOW);

  it('carries every field the design names, and what one crossing is', () => {
    expect(cells.map((c) => c.field)).toEqual(['crossing', 'rate', 'waiting', 'stuck', 'trend', 'verdict', 'machines', 'crossings']);
    expect(cell(cells, 'crossing')).toEqual(['a green parked a car']);
  });

  it('rate is the headway; the trend is this window against the last', () => {
    expect(cell(cells, 'rate')).toEqual(['132/d · 11m gap']);
    expect(cell(cells, 'trend')).toEqual(['132 vs 140 /day · n=132 / 140']);
  });

  it('waiting and stuck are the section\'s own', () => {
    expect(cell(cells, 'waiting')).toEqual(['2 waiting — 1 on a person or the world · 1 stuck', '+2 more waiting, not listed']);
    expect(cell(cells, 'stuck')).toEqual(['1 stuck']);
  });

  it('a held section says since when, and the rule it was judged by', () => {
    const held = sectionCells(
      { ...b, flowing: false, held_since: '2026-09-25T08:48:00Z', state: 'troubled', why: 'nothing parked in 8h', flowing_why: 'quiet 8h > 4 mean gaps' },
      NOW,
    );
    expect(cell(held, 'rate')).toEqual(['132/d · held 08:48Z']);
    expect(cell(held, 'verdict')).toEqual(['troubled', 'nothing parked in 8h', 'quiet 8h > 4 mean gaps']);
  });

  it('names the machine that moves it, and the last crossing', () => {
    expect(cell(cells, 'machines')).toEqual(['boss gate · worked by actors', 'crossed by whoever runs the verb']);
    expect(cell(cells, 'crossings')).toEqual(['2026-09-25 17:02 UTC · 19m ago', MOVES_NOT_KEPT]);
  });

  it('an unread section is "no reading" in every field', () => {
    for (const c of sectionCells(null, NOW)) {
      expect(c.lines, c.field).toEqual(['no reading — the sections could not be read']);
    }
  });

  // WHAT DECLARES IT (car R3): a drawn section is a route the server
  // serves, and its panel opens on the sources that put it on the map.
  const route: Route = {
    from: 'gates',
    to: 'track',
    declared: true,
    sources: [
      { source: 'workflow', workflow: 'pr-train', version: 3, step: 'merged', via: 'completed' },
      { source: 'observed', moves: 9, last_at: NOW },
    ],
  };

  it('a served route leads with what declares it: the protocol step, and the moves the record counted', () => {
    const served = sectionCells(b, NOW, { route, windowHours: 24, bordersRead: true });
    expect(served[0]!.field).toBe('route');
    expect(cell(served, 'route')).toEqual(['pr-train v3, step merged (completed)', '9 moves observed in 24h']);
  });

  it('a served route with no rate yet says so in every reading — never "could not be read", never 0', () => {
    const served = sectionCells(null, NOW, { route, windowHours: 24, bordersRead: true });
    expect(served.map((c) => c.field)).toEqual(['route', 'crossing', 'rate', 'waiting', 'stuck', 'trend', 'verdict', 'machines', 'crossings']);
    for (const c of served.slice(1)) expect(c.lines, c.field).toEqual([NO_RATE_YET]);
  });

  it('a served route whose borders read failed says the read failed — not that it has no rate yet', () => {
    const unread = sectionCells(null, NOW, { route, windowHours: 24, bordersRead: false });
    expect(unread[0]!.field).toBe('route');
    for (const c of unread.slice(1)) expect(c.lines, c.field).toEqual(['no reading — the sections could not be read']);
  });

  it('an undeclared route says it is a finding before its counts', () => {
    const undeclared = sectionCells(null, NOW, {
      route: { from: 'shed', to: 'arrivals', declared: false, sources: [{ source: 'observed', moves: 39, last_at: NOW }] },
      windowHours: 24,
      bordersRead: true,
    });
    expect(cell(undeclared, 'route')).toEqual([
      'observed, undeclared — no protocol or hand-off declares this route',
      '39 moves observed in 24h',
    ]);
  });
});

describe('classesText — on whom a queue waits', () => {
  it('names each class that holds any, and nothing for an empty queue', () => {
    expect(classesText({ machine: 5, person: 2, unknown: 1, stuck: 0 })).toBe('5 on a machine · 2 on a person or the world · 1 cannot tell');
    expect(classesText({ machine: 0, person: 0, unknown: 0, stuck: 0 })).toBe('');
  });
});

// THE BOARDS THE RETIRED PAGES BECAME (design e765b3fc, car N3). Four
// pages held content no station drew — yard status, the conductor's
// feed, the feedback board and the backlog board — and each was MOVED
// into the panel of the station it describes before its page retired,
// so nothing a reader used vanished. This is the one table that says
// where each went; the page draws exactly what it lists.
describe('boardsOf — the retired pages each station panel carries', () => {
  it('names, per station, the boards the design moved there', () => {
    expect(boardsOf('track')).toEqual(['yard-status', 'conductor']);
    expect(boardsOf('dock')).toEqual(['yard-status']);
    expect(boardsOf('garage')).toEqual(['yard-status']);
    expect(boardsOf('receiving')).toEqual(['feedback', 'backlog']);
    expect(boardsOf('marshalling')).toEqual(['backlog']);
  });

  it('carries every retired board somewhere — no content was lost with its page', () => {
    const ALL: ReadonlyArray<StationBoard> = ['yard-status', 'conductor', 'feedback', 'backlog'];
    const carried = new Set(Object.values(STATION_BOARDS).flat());
    expect(ALL.filter((b) => !carried.has(b))).toEqual([]);
  });

  it('a station the table does not name carries none, and a name that is no station carries none', () => {
    expect(boardsOf('gates')).toEqual([]);
    expect(boardsOf('plant')).toEqual([]);
    expect(boardsOf('')).toEqual([]);
  });
});
