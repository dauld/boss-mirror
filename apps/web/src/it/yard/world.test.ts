import { describe, expect, it } from 'bun:test';
import { REGION_NAMES } from './regions';
import { BORDERS } from '../../../tests/fixtures/yard';
import { LINE, TERRITORIES, WORLD, railOf, railWriting, territoryOf, territoryText, wrapName, wrapWords, type Box } from './world';
import { MACHINERY_STRIP_H } from './world-machines';

// THE WORLD'S LAYOUT IS DATA (design d2154293, car 1): the
// regions are territories in one coordinate space, laid out along the
// packet flow. These pin the layout to the server's map — every region
// the server answers has a territory — so a ninth region is a failing
// test and not a blank patch on the map. Which borders the map draws is
// the server's answer, not this file's (car R3 of design e765b3fc): the
// rails below are laid for the borders the server answers today, from
// the test fixture that stands in for it.

const apart = (a: Box, b: Box): boolean =>
  a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;

const rails = BORDERS.map((b) => ({ key: `${b.from}→${b.to}`, rail: railOf(territoryOf(b.from)!, territoryOf(b.to)!) }));

describe('the territories are the server\'s regions', () => {
  it('every region the server answers has exactly one territory, and no territory names a region it does not', () => {
    const names = TERRITORIES.map((t) => t.name);
    expect([...names].sort()).toEqual([...REGION_NAMES].sort());
    expect(new Set(names).size).toBe(names.length);
    for (const name of REGION_NAMES) expect(territoryOf(name)?.name).toBe(name);
  });

  it('every territory lies inside the world and overlaps no other', () => {
    for (const t of TERRITORIES) {
      expect(t.x, t.name).toBeGreaterThanOrEqual(0);
      expect(t.y, t.name).toBeGreaterThanOrEqual(0);
      expect(t.x + t.w, t.name).toBeLessThanOrEqual(WORLD.width);
      expect(t.y + t.h, t.name).toBeLessThanOrEqual(WORLD.height);
      for (const o of TERRITORIES) {
        if (o === t) continue;
        expect(apart(t, o), `${t.name} overlaps ${o.name}`).toBe(true);
      }
    }
  });

  // THE ORDER A CAR WALKS (design 62de32ae decision 3): built on the
  // shop floor, gated, parked on the dock, boarded, arrived, proven. The
  // line put the dock before the gates until 2026-09-24, so a car
  // followed across the map walked backwards through two regions.
  it('lays the line out in the order a car walks it — receiving, marshalling, shop-floor, gates, dock, track, arrivals, shed — left to right', () => {
    const line = ['receiving', 'marshalling', 'shop-floor', 'gates', 'dock', 'track', 'arrivals', 'shed'] as const;
    expect(LINE).toEqual(line);
    const xs = line.map((n) => territoryOf(n)!.x);
    expect([...xs].sort((a, b) => a - b)).toEqual(xs);
    expect(new Set(xs).size).toBe(xs.length);
  });

  it('hangs the garage under the gates and the track, spanning both', () => {
    const garage = territoryOf('garage')!;
    const gates = territoryOf('gates')!;
    const track = territoryOf('track')!;
    expect(garage.y).toBeGreaterThanOrEqual(gates.y + gates.h);
    expect(garage.x).toBe(gates.x);
    expect(garage.x + garage.w).toBe(track.x + track.w);
  });

  it('hangs the publish dock off arrivals, on the sidings row below the line', () => {
    const publish = territoryOf('publish')!;
    const arrivals = territoryOf('arrivals')!;
    const shed = territoryOf('shed')!;
    const garage = territoryOf('garage')!;
    expect(publish.y).toBeGreaterThanOrEqual(arrivals.y + arrivals.h);
    expect(publish.y).toBe(garage.y);
    expect(publish.x).toBe(arrivals.x);
    expect(publish.x + publish.w).toBe(shed.x + shed.w);
  });
});

// BORDERS ARE DRAWN, NOT TOOLTIPPED (design 62de32ae decision 6). A
// rail between two boxes on one line of eight had about 24 units to
// carry a count, a rate and a machine — so the rails leave the line
// and run ABOVE it, station to station, where each has most of a slot's
// width to write its machine's name on. Every number here is derived
// from the territories by one rule; none is placed by hand.
describe('the rails have length and room to write on', () => {
  it('a line rail leaves the top of its territory and arrives at the top of the next, running above both', () => {
    const shop = territoryOf('shop-floor')!;
    const gates = territoryOf('gates')!;
    const r = railOf(shop, gates);
    expect(r.kind).toBe('line');
    expect(r.d.startsWith(`M${r.from.x} ${shop.y}`)).toBe(true);
    expect(r.from.y).toBe(shop.y);
    expect(r.to.y).toBe(gates.y);
    // Out of the right half of the one, into the left half of the next.
    expect(r.from.x).toBeGreaterThan(shop.x + shop.w / 2);
    expect(r.to.x).toBeLessThan(gates.x + gates.w / 2);
    // The badge stands ON the rail, above the gap between the two.
    expect(r.mid.y).toBeLessThan(shop.y);
    expect(r.mid.x).toBeGreaterThan(shop.x + shop.w);
    expect(r.mid.x).toBeLessThan(gates.x);
  });

  it('every line rail is at least four times as long as the gap it used to be squeezed into', () => {
    for (const { key, rail } of rails.filter((r) => r.rail.kind === 'line')) {
      const [from, to] = key.split('→');
      const gap = territoryOf(to!)!.x - (territoryOf(from!)!.x + territoryOf(from!)!.w);
      expect(rail.length, key).toBeGreaterThanOrEqual(4 * gap);
    }
  });

  it('a siding drops straight from the bottom of its territory to the top of the siding, inside both', () => {
    const gates = territoryOf('gates')!;
    const garage = territoryOf('garage')!;
    const r = railOf(gates, garage);
    expect(r.kind).toBe('siding');
    expect(r.from.y).toBe(gates.y + gates.h);
    expect(r.to.y).toBe(garage.y);
    expect(r.from.x).toBe(r.to.x);
    expect(r.from.x).toBeGreaterThanOrEqual(Math.max(gates.x, garage.x));
    expect(r.from.x).toBeLessThanOrEqual(Math.min(gates.x + gates.w, garage.x + garage.w));
  });

  it('no rail\'s writing lies over a territory or over another rail\'s writing, and all of it is inside the world', () => {
    for (const { key, rail } of rails) {
      const l = rail.label;
      expect(l.x, key).toBeGreaterThanOrEqual(0);
      expect(l.y, key).toBeGreaterThanOrEqual(0);
      expect(l.x + l.w, key).toBeLessThanOrEqual(WORLD.width);
      expect(l.y + l.h, key).toBeLessThanOrEqual(WORLD.height);
      for (const t of TERRITORIES) expect(apart(l, t), `${key}'s label lies over ${t.name}`).toBe(true);
      for (const o of rails) {
        if (o.key === key) continue;
        expect(apart(l, o.rail.label), `${key}'s label lies over ${o.key}'s`).toBe(true);
      }
    }
  });

  it('every label has room for the machine\'s name, its lamp and the rate: four lines of the map\'s type', () => {
    for (const { key, rail } of rails) {
      expect(rail.label.h, key).toBeGreaterThanOrEqual(4 * 12);
      expect(rail.label.w, key).toBeGreaterThanOrEqual(110);
    }
  });
});

describe('railWriting — where a rail\'s words go, inside its label', () => {
  const inside = (p: { x: number; y: number }, b: Box): boolean =>
    p.x >= b.x && p.x <= b.x + b.w && p.y >= b.y && p.y <= b.y + b.h;

  it('puts every line of writing inside the rail\'s label box, for a one- and a two-line name', () => {
    for (const { key, rail } of rails) {
      for (const n of [1, 2]) {
        const w = railWriting(rail, n, 20);
        expect(w.names.length, key).toBe(n);
        for (const p of [...w.names, w.lamp, w.status, w.rate]) expect(inside(p, rail.label), `${key}: ${JSON.stringify(p)}`).toBe(true);
      }
    }
  });

  it('writes a line rail\'s name and lamp above the rail and its rate below, and a siding\'s beside it, clear of the badge', () => {
    const line = railOf(territoryOf('gates')!, territoryOf('dock')!);
    const lw = railWriting(line, 2, 12);
    expect(Math.max(...lw.names.map((p) => p.y), lw.status.y)).toBeLessThan(line.mid.y - 8);
    expect(lw.rate.y).toBeGreaterThan(line.mid.y + 8);
    expect(lw.names[0]!.y).toBeLessThan(lw.names[1]!.y);

    const side = railOf(territoryOf('gates')!, territoryOf('garage')!);
    const sw = railWriting(side, 2, 12);
    // Everything starts right of the badge, which is at most 16 wide
    // either side of the rail.
    for (const p of [...sw.names, sw.lamp, sw.status, sw.rate]) expect(p.x).toBeGreaterThan(side.mid.x + 12);
  });
});

// A TERRITORY'S WORDS FIT ITS OUTLINE. Car A put the KPI and the band
// that decided the state inside every territory; the line's boxes hold
// them in one column, and the sidings — wide and short — in two. A
// verdict cut off by the machinery strip, or printed below the box, is
// a verdict nobody reads (the review found the shed's why running past
// its outline, finding 12).
describe('territoryText — where a territory\'s words go', () => {
  const lines = (at: { y: number }, n: number): ReadonlyArray<number> => Array.from({ length: n }, (_, i) => at.y + i * 12);

  it('keeps every line inside its territory and above the machinery strip', () => {
    for (const t of TERRITORIES) {
      const lt = territoryText(t);
      const floor = t.y + t.h - MACHINERY_STRIP_H - 2;
      const all = [
        lt.name.y, lt.count.y, lt.state.y,
        ...lines(lt.kpi, lt.kpiLines), ...lines(lt.trend, lt.trendLines), lt.metric.y,
        ...lines(lt.verdict, lt.verdictLines),
      ];
      for (const y of all) {
        expect(y, t.name).toBeGreaterThan(t.y);
        expect(y, t.name).toBeLessThanOrEqual(floor);
      }
      for (const at of [lt.name, lt.count, lt.state, lt.kpi, lt.metric, lt.trend, lt.verdict]) {
        expect(at.x, t.name).toBeGreaterThan(t.x);
        expect(at.x, t.name).toBeLessThan(t.x + t.w);
      }
    }
  });

  it('never stacks one block of words on another', () => {
    for (const t of TERRITORIES) {
      const lt = territoryText(t);
      const last = (at: { y: number }, n: number) => at.y + (n - 1) * 12;
      // The left column, top to bottom.
      expect(lt.count.y, t.name).toBeGreaterThan(lt.name.y);
      expect(lt.state.y, t.name).toBeGreaterThan(lt.count.y);
      expect(lt.kpi.y, t.name).toBeGreaterThan(lt.state.y);
      const sameColumn = lt.metric.x === lt.kpi.x;
      if (sameColumn) {
        expect(lt.metric.y, t.name).toBeGreaterThan(last(lt.kpi, lt.kpiLines));
      } else {
        // The right column starts past the left one's words.
        expect(lt.metric.x, t.name).toBeGreaterThanOrEqual(lt.kpi.x + lt.chars * 5.8);
      }
      expect(lt.trend.y, t.name).toBeGreaterThan(lt.metric.y);
      expect(lt.verdict.y, t.name).toBeGreaterThan(last(lt.trend, lt.trendLines));
    }
  });

  it('gives every territory three lines for its verdict — the band, and for trouble the why after it', () => {
    for (const t of TERRITORIES) expect(territoryText(t).verdictLines, t.name).toBeGreaterThanOrEqual(3);
  });
});

describe('wrapWords — a why inside its outline', () => {
  it('breaks on spaces to the width, keeps a long word whole, and caps the lines with an ellipsis', () => {
    expect(wrapWords('1 bay holds a corpse', 12, 3)).toEqual(['1 bay holds', 'a corpse']);
    expect(wrapWords('averyveryverylongword here', 8, 3)).toEqual(['averyveryverylongword', 'here']);
    expect(wrapWords('one two three four five six seven', 9, 2)).toEqual(['one two', 'three fo…']);
    expect(wrapWords('', 10, 2)).toEqual([]);
  });
});

describe('wrapName — a machine\'s name on its rail', () => {
  it('keeps a name that fits on one line whole', () => {
    expect(wrapName('train-board-on-dock-depth', 26, 2)).toEqual(['train-board-on-dock-depth']);
    expect(wrapName('the gate runner', 26, 2)).toEqual(['the gate runner']);
  });

  it('breaks a long registry name AFTER a hyphen, so each line still reads as part of the name', () => {
    expect(wrapName('publish-github-pr-on-open-pr-ready', 22, 2)).toEqual(['publish-github-pr-on-', 'open-pr-ready']);
    expect(wrapName('train-board-on-dock-depth', 12, 3)).toEqual(['train-board-', 'on-dock-', 'depth']);
  });

  it('caps the lines with an ellipsis rather than running off the rail', () => {
    expect(wrapName('a-b-c-d-e-f-g-h', 4, 2)).toEqual(['a-b-', 'c-d…']);
    expect(wrapName('', 10, 2)).toEqual([]);
  });
});
