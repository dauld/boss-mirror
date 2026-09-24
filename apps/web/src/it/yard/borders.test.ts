import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  RAIL_MAX_WIDTH,
  crossedText,
  densityOf,
  machineStatus,
  machineText,
  parseBorders,
  railWidth,
  rateText,
  summaryLine,
  unlistedText,
  waitingText,
  type Border,
  type Machine,
} from './borders';
import { BORDERS } from './world';

// THE WORLD MAP'S RAILS (design d2154293, car 2): every border read
// from ONE endpoint, /api/yard/borders, carrying the crossing rate,
// what waits to cross and the machine that moves it. These pin the
// client half — the parse, and the words a rail prints — so the map
// cannot make up a number the server did not send. The rule under all
// of them: a border whose flow the server could not compute renders as
// UNKNOWN, never as zero. A zero on a map read at a glance is the
// false-empty class in its most visible form.

const machine = (over: Partial<Machine> = {}): Machine => ({
  name: 'train-board-on-dock-depth',
  kind: 'cadence',
  last_fired: '2026-09-19T09:00:00+00:00',
  silent_for_minutes: 180,
  expected_every_minutes: 30,
  silent: true,
  why: 'its own firing in cadence_firings',
  ...over,
});

const border = (over: Partial<Border> = {}): Border => ({
  from: 'dock',
  to: 'track',
  crossing: 'a car boarded a train',
  rate: {
    metric: 'crossings',
    unit: 'per day',
    current: 3,
    previous: 5,
    samples: 3,
    previous_samples: 5,
  },
  last_crossed: '2026-09-19T11:00:00+00:00',
  waiting: 2,
  holds: [{ what: 'fix/a', why: 'parked, waiting for the boarding depth' }],
  machine: machine(),
  state: 'troubled',
  why: '2 packets waiting and train-board-on-dock-depth silent for 180m',
  ...over,
});

describe('parseBorders — the payload, once', () => {
  it('reads a border whole, and keeps every unknown as null', () => {
    const parsed = parseBorders({
      window_hours: 24,
      now: '2026-09-19T12:00:00Z',
      borders: [
        {
          from: 'receiving',
          to: 'marshalling',
          crossing: 'an inbound packet triaged',
          rate: {
            metric: 'crossings',
            unit: 'per day',
            current: null,
            previous: null,
            samples: 0,
            previous_samples: 0,
          },
          last_crossed: null,
          waiting: null,
          holds: [],
          machine: {
            name: 'the receiving desk',
            kind: 'actors',
            last_fired: null,
            silent_for_minutes: null,
            expected_every_minutes: null,
            silent: null,
            why: 'no machine moves this hop',
          },
          state: 'troubled',
          why: 'the workflow registry could not be read',
        },
      ],
    });
    expect(parsed.window_hours).toBe(24);
    const b = parsed.borders[0]!;
    expect(b.waiting).toBeNull();
    expect(b.rate.current).toBeNull();
    expect(b.machine.silent).toBeNull();
    expect(b.last_crossed).toBeNull();
    expect(b.holds).toEqual([]);
  });

  it('refuses a payload without borders — a wrong server is not an empty map', () => {
    expect(() => parseBorders({ window_hours: 24 })).toThrow();
    expect(() => parseBorders(null)).toThrow();
  });

  it('refuses a state it does not know rather than drawing it as clear', () => {
    expect(() =>
      parseBorders({ window_hours: 24, borders: [{ ...border(), state: 'fine' }] }),
    ).toThrow();
  });
});

describe('the words a rail prints', () => {
  it('prints the rate against the previous window with the samples behind it', () => {
    expect(rateText(border().rate)).toBe('3 vs 5 /day · n=3 / 5');
  });

  it('says NO READING for a rate the server could not compute — never 0', () => {
    const unknown = { ...border().rate, current: null, previous: null };
    expect(rateText(unknown)).toBe('no reading');
    expect(rateText(unknown)).not.toContain('0');
  });

  it('counts what waits, and admits when it could not be counted', () => {
    expect(waitingText(border())).toBe('2 waiting');
    expect(waitingText(border({ waiting: 1 }))).toBe('1 waiting');
    expect(waitingText(border({ waiting: 0 }))).toBe('nothing waiting');
    expect(waitingText(border({ waiting: null }))).toBe('waiting: no reading');
  });

  it('names the machine with how long it has been quiet, and says when nothing records it', () => {
    expect(machineText(machine())).toBe('train-board-on-dock-depth · SILENT 180m');
    expect(machineText(machine({ silent: false, silent_for_minutes: 4 }))).toBe(
      'train-board-on-dock-depth · fired 4m ago',
    );
    expect(
      machineText(machine({ silent: null, silent_for_minutes: null, last_fired: null })),
    ).toBe('train-board-on-dock-depth · no firing recorded');
  });

  // A border of kind `actors` has NO rule to fire — a person or an
  // agent does the crossing — so "no firing recorded" there is not a
  // finding, it is the only sentence that could ever be true. It read
  // as a dead automation on `receiving -> marshalling`, the most
  // backed-up border in the yard, which is the pairing most likely to
  // send a reader hunting for a rule that does not exist (beec1130).
  //
  // CLAUDE.md §Diagnosis says a troubled packet must look troubled;
  // the inverse has to hold too, or the surface spends attention on a
  // non-problem and teaches the reader to discount the phrase on the
  // borders where it IS a finding.
  it('says an actor-worked border is worked by actors, not that nothing fired', () => {
    const actors = machine({
      name: 'the receiving desk',
      kind: 'actors',
      last_fired: null,
      silent_for_minutes: null,
      expected_every_minutes: null,
      silent: null,
      why: 'no machine moves this hop — an actor does; the last crossing is the stamp',
    });
    expect(machineText(actors)).toBe('the receiving desk · worked by actors');
    expect(machineText(actors)).not.toContain('no firing recorded');

    // THE CONTROL. A machine that really does fire and has no record
    // must still say so — the phrase is a finding there, and a change
    // that silenced it everywhere would satisfy the line above.
    expect(
      machineText(machine({ silent: null, silent_for_minutes: null, last_fired: null })),
    ).toContain('no firing recorded');
  });

  // The rail's status half on its own — the line written under the
  // machine's name ON the rail (design 62de32ae decision 6), which
  // machineText joins to the name for the one-line surfaces.
  it('says the machine\'s status apart from its name, in the same words machineText uses', () => {
    expect(machineStatus(machine())).toBe('SILENT 180m');
    expect(machineStatus(machine({ silent: false, silent_for_minutes: 4 }))).toBe('fired 4m ago');
    expect(machineStatus(machine({ silent: null, silent_for_minutes: null, last_fired: null }))).toBe(
      'no firing recorded',
    );
    expect(machineStatus(machine({ kind: 'actors', silent: null, silent_for_minutes: null }))).toBe(
      'worked by actors',
    );
    for (const m of [machine(), machine({ kind: 'actors' }), machine({ silent: false, silent_for_minutes: 4 })]) {
      expect(machineText(m)).toBe(`${m.name} · ${machineStatus(m)}`);
    }
  });

  // RAIL WIDTH FOLLOWS RATE (decision 6). Presentation only — the rate
  // is printed beside it — but it must never draw an unmeasured rail as
  // a quiet one, nor let a busy rail swallow the map.
  it('draws a rail wider the more crosses it, a hairline when nothing did, and never past its cap', () => {
    expect(railWidth(0)).toBeLessThan(railWidth(1));
    expect(railWidth(1)).toBeLessThan(railWidth(10));
    expect(railWidth(10)).toBeLessThan(railWidth(100));
    expect(railWidth(100)).toBeLessThan(railWidth(400));
    expect(railWidth(100_000)).toBe(RAIL_MAX_WIDTH);
    expect(railWidth(0)).toBeLessThan(2);
    // Unknown is its own drawing (the dotted density band), at the
    // width of a thin measured rail — not the hairline an empty one is.
    expect(railWidth(null)).toBeGreaterThan(railWidth(0));
  });

  it('keeps the live rates apart: a few a day, tens and hundreds each draw a different width', () => {
    // Measured 2026-09-24: 2/d (publish), 44/d (arrivals), 194/d
    // (boarding), 493/d (intake). A curve that put the last three at
    // the cap would say nothing about which rail carries the traffic.
    const widths = [2, 44, 194, 493].map(railWidth);
    for (let i = 1; i < widths.length; i++) expect(widths[i]! - widths[i - 1]!).toBeGreaterThanOrEqual(0.7);
    expect(widths[3]).toBeLessThan(RAIL_MAX_WIDTH);
  });

  it('says when a rail last crossed in the stack\'s own clock, and how long ago — and admits when nothing did', () => {
    const now = '2026-09-24T14:49:10Z';
    expect(crossedText(border({ last_crossed: '2026-09-24T14:46:00.356667+00:00' }), now)).toBe(
      '2026-09-24 14:46 UTC · 3m ago',
    );
    expect(crossedText(border({ last_crossed: '2026-09-24T11:40:00+00:00' }), now)).toBe(
      '2026-09-24 11:40 UTC · 3h ago',
    );
    expect(crossedText(border({ last_crossed: '2026-09-21T14:00:00+00:00' }), now)).toBe(
      '2026-09-21 14:00 UTC · 3d ago',
    );
    expect(crossedText(border({ last_crossed: null }), now)).toBe('nothing crossed in the two windows read');
    // A clock the payload did not carry is not "just now".
    expect(crossedText(border({ last_crossed: '2026-09-24T14:46:00+00:00' }), '')).toBe('2026-09-24 14:46 UTC');
  });

  it('says how many wait that the list does not name, since the server bounds the list and not the count', () => {
    expect(unlistedText(border({ waiting: 2 }))).toBe('+1 more waiting, not listed');
    expect(unlistedText(border({ waiting: 1 }))).toBe('');
    expect(unlistedText(border({ waiting: null }))).toBe('');
  });

  it('gives traffic a density band, and an unmeasured rate its own band — not the empty one', () => {
    expect(densityOf(null)).toBe('unknown');
    expect(densityOf(0)).toBe('none');
    expect(densityOf(1)).toBe('light');
    expect(densityOf(6)).toBe('steady');
    expect(densityOf(30)).toBe('heavy');
    expect(densityOf(null)).not.toBe(densityOf(0));
  });
});

describe('summaryLine — the activity bubbled up to the high-level view', () => {
  it('counts the crossings and names the troubled borders', () => {
    const line = summaryLine({
      window_hours: 24,
      now: '',
      borders: [border(), border({ from: 'track', to: 'arrivals', state: 'clear', waiting: 0 })],
    });
    expect(line).toContain('6 crossings in 24h');
    expect(line).toContain('dock → track');
  });

  it('says a border it could not read rather than leaving it out of the count', () => {
    const blind = border({
      rate: { ...border().rate, current: null, previous: null, samples: 0 },
      waiting: null,
    });
    const line = summaryLine({ window_hours: 24, now: '', borders: [blind] });
    expect(line).toContain('1 border unread');
  });
});

describe('the border set is the server\'s', () => {
  it('equals boss_jobs::borders::BORDERS (crates/core/boss-jobs/src/borders.rs), in order', () => {
    // A fact that lives twice gets an equality test (CLAUDE.md §9a):
    // the layout declares the borders the map DRAWS, the server
    // declares the borders it ANSWERS, and a rail drawn for a border
    // the server does not answer is a blank patch nobody would notice.
    const src = readFileSync(
      join(import.meta.dir, '..', '..', '..', '..', '..', 'crates', 'core', 'boss-jobs', 'src', 'borders.rs'),
      'utf8',
    );
    const block = src.match(/pub const BORDERS: \[BorderSpec; (\d+)\] = \[([\s\S]*?)\n\];/);
    expect(block, 'boss_jobs::borders::BORDERS is where the server declares them').not.toBeNull();
    const rows = [...block![2]!.matchAll(/from:\s*"([a-z-]+)",\s*to:\s*"([a-z-]+)"/g)].map(
      (m) => `${m[1]}→${m[2]}`,
    );
    expect(Number(block![1])).toBe(rows.length);
    expect(BORDERS.map((b) => `${b.from}→${b.to}`)).toEqual(rows);
  });
});
