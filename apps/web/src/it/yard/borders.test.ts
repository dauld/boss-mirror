import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  densityOf,
  machineText,
  parseBorders,
  rateText,
  summaryLine,
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
  from: 'gates',
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
    expect(line).toContain('gates → track');
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
