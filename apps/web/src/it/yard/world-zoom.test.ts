import { describe, expect, it } from 'bun:test';
import { REGION_NAMES } from './regions';
import { TERRITORIES, WORLD, territoryOf } from './world';
import {
  INTERIOR_REGIONS,
  WORLD_BOX,
  ZOOM_MS,
  easeInOut,
  hasInterior,
  interiorLayout,
  interiorWagons,
  lerpBox,
  regionOfStation,
  viewBoxText,
  zoomBoxOf,
} from './world-zoom';
import type { Scene, Station, Wagon } from './yard-floor';

// CLICK IS A ZOOM, NOT A DEPARTURE (design d2154293, car 3). The world
// is one coordinate space and one SVG; zooming into a territory is a
// viewBox that walks from the whole world to that territory's rect,
// and the territory then shows WHAT IS MOVING INSIDE IT — the wagons
// standing at the stations that territory covers. These pin the
// arithmetic of both halves: the box the zoom walks to, and which
// wagons belong to which territory. Nothing here touches a DOM, so a
// broken zoom is a failing unit test before it is a broken picture.

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

describe('the box a click zooms to', () => {
  it('holds the whole world when nothing is zoomed, and a territory when one is', () => {
    expect(zoomBoxOf(null)).toEqual(WORLD_BOX);
    expect(WORLD_BOX).toEqual({ x: 0, y: 0, w: WORLD.width, h: WORLD.height });
    const dock = territoryOf('dock')!;
    const box = zoomBoxOf('dock');
    // The territory, with room around it — the zoom must not crop the
    // outline it zoomed INTO, and the rails leaving it stay in view.
    expect(box.x).toBeLessThan(dock.x);
    expect(box.y).toBeLessThan(dock.y);
    expect(box.x + box.w).toBeGreaterThan(dock.x + dock.w);
    expect(box.y + box.h).toBeGreaterThan(dock.y + dock.h);
    // And it is a real zoom: strictly smaller than the world.
    expect(box.w).toBeLessThan(WORLD_BOX.w);
    expect(box.h).toBeLessThan(WORLD_BOX.h);
  });

  it('every territory has a zoom box, and a name the world does not know stays at the world', () => {
    for (const t of TERRITORIES) expect(zoomBoxOf(t.name).w, t.name).toBeLessThan(WORLD_BOX.w);
    expect(zoomBoxOf('siding')).toEqual(WORLD_BOX);
    expect(zoomBoxOf('')).toEqual(WORLD_BOX);
  });

  it('walks from one box to the other and arrives exactly', () => {
    const a = WORLD_BOX;
    const b = zoomBoxOf('gates');
    expect(lerpBox(a, b, 0)).toEqual(a);
    expect(lerpBox(a, b, 1)).toEqual(b);
    const half = lerpBox(a, b, 0.5);
    expect(half.w).toBeCloseTo((a.w + b.w) / 2, 6);
    expect(half.x).toBeCloseTo((a.x + b.x) / 2, 6);
    // The walk is monotone: the box only ever gets smaller on the way in.
    const widths = [0, 0.25, 0.5, 0.75, 1].map((k) => lerpBox(a, b, k).w);
    expect([...widths].sort((x, y) => y - x)).toEqual(widths);
  });

  it('eases in and out, pinned at both ends', () => {
    expect(easeInOut(0)).toBe(0);
    expect(easeInOut(1)).toBe(1);
    expect(easeInOut(0.5)).toBeCloseTo(0.5, 6);
    expect(easeInOut(0.25)).toBeLessThan(0.25);
    expect(easeInOut(0.75)).toBeGreaterThan(0.75);
    expect(ZOOM_MS).toBeGreaterThan(0);
  });

  it('prints a viewBox SVG accepts — four numbers, no exponent, no NaN', () => {
    expect(viewBoxText(WORLD_BOX)).toBe('0 0 1240 400');
    const text = viewBoxText(lerpBox(WORLD_BOX, zoomBoxOf('dock'), 1 / 3));
    expect(text.split(' ')).toHaveLength(4);
    for (const n of text.split(' ')) expect(Number.isFinite(Number(n)), n).toBe(true);
    expect(text).not.toContain('e');
  });
});

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

describe('the interior lays out inside the territory it belongs to', () => {
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
