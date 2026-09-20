import { describe, expect, it } from 'bun:test';
import { REGION_NAMES } from './regions';
import { TERRITORIES, WORLD, territoryOf } from './world';
import {
  INTERIOR_REGIONS,
  hasInterior,
  interiorLayout,
  interiorWagons,
  regionOfStation,
} from './region-contents';
import type { Scene, Station, Wagon } from './yard-floor';

// WHAT A REGION CONTAINS, AND WHERE IT GOES IN THE RECT (backlog
// ca37478f). A click no longer walks a camera into a territory — the
// world map is REPLACED by that region's own map — so what is pinned
// here is the part that survived: which wagons belong to which region,
// and where each plate lands inside whatever rect it is handed. The
// camera's arithmetic (zoomBoxOf, lerpBox, easeInOut, viewBoxText) is
// deleted, and so are its tests. Nothing here touches a DOM, so a
// broken layout is a failing unit test before it is a broken picture.

const wagon = (id: string, station: Station, slot = 0): Wagon => ({
  id,
  tag: id,
  title: `car ${id}`,
  branch: `fix/${id}`,
  head: null,
  kind: 'backlog-item',
  sim: false,
  station,
  slot,
  trainId: null,
  tone: 'static',
  lamp: 'off',
  status: 'standing',
  since: null,
});

const sceneOf = (wagons: readonly Wagon[]): Scene =>
  ({ now: '2026-09-19T05:00:00Z', wagons, locos: [], bays: [], signals: [], boardRows: [], machines: {} }) as unknown as Scene;

describe('what is moving inside a territory', () => {
  it('sends every station a wagon can stand at to exactly one territory', () => {
    const stations: readonly Station[] = [
      'approach', 'gate-queue', 'gate', 'limbo', 'dock', 'garage', 'train',
      'arrivals', 'cancelled', 'inspection-shed', 'siding-event', 'siding-no-probe',
    ];
    for (const s of stations) {
      const region = regionOfStation(s);
      expect(REGION_NAMES, s).toContain(region);
      expect(territoryOf(region), s).toBeDefined();
    }
    // The gates territory is the whole approach — publishing, queued and
    // in a bay — as regions.ts already says the gates floor is.
    expect(regionOfStation('gate-queue')).toBe('gates');
    expect(regionOfStation('limbo')).toBe('gates');
    // A withdrawn car stands on the cancelled siding, which is drawn in
    // the arrivals yard: the terminal tracks are one territory.
    expect(regionOfStation('cancelled')).toBe('arrivals');
    expect(regionOfStation('siding-no-probe')).toBe('shed');
    expect(regionOfStation('train')).toBe('track');
  });

  it('the regions with an interior are the yard regions, not the two with pages of their own', () => {
    expect([...INTERIOR_REGIONS].sort()).toEqual(['arrivals', 'dock', 'garage', 'gates', 'shed', 'track']);
    expect(hasInterior('dock')).toBe(true);
    expect(hasInterior('receiving')).toBe(false);
    expect(hasInterior('marshalling')).toBe(false);
    expect(hasInterior('nowhere')).toBe(false);
  });

  it('picks the wagons standing in the territory, in the order the floor stands them', () => {
    const scene = sceneOf([
      wagon('c', 'gate', 2),
      wagon('a', 'approach', 1),
      wagon('b', 'gate-queue', 0),
      wagon('d', 'dock', 0),
      wagon('e', 'train', 0),
    ]);
    expect(interiorWagons(scene, 'gates').map((w) => w.id)).toEqual(['a', 'b', 'c']);
    expect(interiorWagons(scene, 'dock').map((w) => w.id)).toEqual(['d']);
    expect(interiorWagons(scene, 'track').map((w) => w.id)).toEqual(['e']);
    expect(interiorWagons(scene, 'shed')).toEqual([]);
    // A region with no interior draws none, even if a station mapped to it.
    expect(interiorWagons(scene, 'receiving')).toEqual([]);
  });
});

describe('the interior lays out inside the rect it is handed', () => {
  const dock = territoryOf('dock')!;

  it('places every plate inside the outline, in a grid, and counts what did not fit', () => {
    const many = Array.from({ length: 40 }, (_, i) => wagon(`w${i}`, 'dock', i));
    const laid = interiorLayout(dock, many);
    expect(laid.placed.length).toBeGreaterThan(0);
    expect(laid.placed.length).toBeLessThan(many.length);
    expect(laid.hidden).toBe(many.length - laid.placed.length);
    for (const p of laid.placed) {
      expect(p.x, p.wagon.id).toBeGreaterThanOrEqual(dock.x);
      expect(p.y, p.wagon.id).toBeGreaterThanOrEqual(dock.y);
      expect(p.x + p.w, p.wagon.id).toBeLessThanOrEqual(dock.x + dock.w);
      expect(p.y + p.h, p.wagon.id).toBeLessThanOrEqual(dock.y + dock.h);
    }
    // The plates do not sit on one another.
    for (const a of laid.placed) {
      for (const b of laid.placed) {
        if (a === b) continue;
        const apart = a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
        expect(apart, `${a.wagon.id} sits on ${b.wagon.id}`).toBe(true);
      }
    }
    // Reading order: left to right, then down.
    const rows = new Map<number, number[]>();
    for (const p of laid.placed) rows.set(p.y, [...(rows.get(p.y) ?? []), p.x]);
    for (const xs of rows.values()) expect([...xs].sort((m, n) => m - n)).toEqual(xs);
  });

  it('hides nothing when everything fits, and is empty for an empty region', () => {
    const two = [wagon('a', 'dock', 0), wagon('b', 'dock', 1)];
    const laid = interiorLayout(dock, two);
    expect(laid.placed.map((p) => p.wagon.id)).toEqual(['a', 'b']);
    expect(laid.hidden).toBe(0);
    const none = interiorLayout(dock, []);
    expect(none.placed).toEqual([]);
    expect(none.hidden).toBe(0);
  });

  it('lays out in every territory, including the wide low garage', () => {
    const many = Array.from({ length: 12 }, (_, i) => wagon(`w${i}`, 'garage', i));
    for (const t of TERRITORIES) {
      const laid = interiorLayout(t, many);
      expect(laid.placed.length, t.name).toBeGreaterThan(0);
      for (const p of laid.placed) {
        expect(p.x + p.w, `${t.name}/${p.wagon.id}`).toBeLessThanOrEqual(t.x + t.w);
        expect(p.y + p.h, `${t.name}/${p.wagon.id}`).toBeLessThanOrEqual(t.y + t.h);
      }
    }
  });
});
