import { describe, expect, it } from 'bun:test';
import { REGION_NAMES } from './regions';
import { BORDERS, TERRITORIES, WORLD, borderPath, territoryOf, wrapWords } from './world';

// THE WORLD'S LAYOUT IS DATA (design d2154293, car 1): the
// regions are territories in one coordinate space, laid out along the
// packet flow, and the borders between them are declared, not drawn by
// hand. These pin the layout to the server's map — every region the
// server answers has a territory, every border joins two declared
// territories — so a ninth region or a typo'd border is a failing test
// and not a blank patch on the map.

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
        const apart = t.x + t.w <= o.x || o.x + o.w <= t.x || t.y + t.h <= o.y || o.y + o.h <= t.y;
        expect(apart, `${t.name} overlaps ${o.name}`).toBe(true);
      }
    }
  });

  it('lays the line out along the packet flow — receiving, marshalling, shop-floor, dock, gates, track, arrivals, shed — left to right, the garage a siding under gates and track', () => {
    const line = ['receiving', 'marshalling', 'shop-floor', 'dock', 'gates', 'track', 'arrivals', 'shed'] as const;
    const xs = line.map((n) => territoryOf(n)!.x);
    expect([...xs].sort((a, b) => a - b)).toEqual(xs);
    const garage = territoryOf('garage')!;
    const gates = territoryOf('gates')!;
    const track = territoryOf('track')!;
    expect(garage.y).toBeGreaterThanOrEqual(gates.y + gates.h);
    expect(garage.x).toBeLessThan(track.x);
    expect(garage.x + garage.w).toBeGreaterThan(gates.x + gates.w);
  });
});

describe('the borders join declared territories', () => {
  it('every border names two different declared territories, once', () => {
    const names = new Set(TERRITORIES.map((t) => t.name));
    const seen = new Set<string>();
    for (const b of BORDERS) {
      expect(names.has(b.from), `${b.from} → ${b.to}: ${b.from} is not a territory`).toBe(true);
      expect(names.has(b.to), `${b.from} → ${b.to}: ${b.to} is not a territory`).toBe(true);
      expect(b.from).not.toBe(b.to);
      const key = `${b.from}→${b.to}`;
      expect(seen.has(key), `${key} declared twice`).toBe(false);
      seen.add(key);
    }
  });

  it('carries the flow: each hop of the line is a border, and the garage hangs off gates and track', () => {
    const keys = BORDERS.map((b) => `${b.from}→${b.to}`);
    for (const hop of [
      'receiving→marshalling',
      // The shop floor split one hop in two (backlog 94c6ffd0): a run
      // OPENS on a packet, and its car PARKS some hours later.
      'marshalling→shop-floor',
      'shop-floor→dock',
      'dock→gates',
      'gates→track',
      'track→arrivals',
      'arrivals→shed',
      // The crossing OUT of the world (design cb38d806): what arrived
      // on main is what a publish proposes to the public mirror.
      'arrivals→publish',
      'gates→garage',
      'track→garage',
    ]) {
      expect(keys, hop).toContain(hop);
    }
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
    // It leaves arrivals downwards, the way the garage does.
    const p = borderPath(arrivals, publish);
    expect(p.y1).toBe(arrivals.y + arrivals.h);
    expect(p.y2).toBe(publish.y);
  });

  it('a border path runs from the facing edge of one territory to the facing edge of the other', () => {
    // Side by side: leaves the right edge, arrives at the left edge.
    const dock = territoryOf('dock')!;
    const gates = territoryOf('gates')!;
    const p = borderPath(dock, gates);
    expect(p.x1).toBe(dock.x + dock.w);
    expect(p.x2).toBe(gates.x);
    expect(p.y1).toBe(p.y2);
    // Stacked: leaves the bottom edge, arrives at the top edge, in the
    // middle of the two territories' shared width.
    const garage = territoryOf('garage')!;
    const q = borderPath(gates, garage);
    expect(q.y1).toBe(gates.y + gates.h);
    expect(q.y2).toBe(garage.y);
    expect(q.x1).toBe(q.x2);
    expect(q.x1).toBeGreaterThanOrEqual(Math.max(gates.x, garage.x));
    expect(q.x1).toBeLessThanOrEqual(Math.min(gates.x + gates.w, garage.x + garage.w));
    expect(p.d.startsWith('M')).toBe(true);
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
