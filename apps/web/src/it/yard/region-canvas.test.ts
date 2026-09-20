// A REGION MAP OWNS ITS CANVAS (backlog ca37478f). The contract these
// pin is the one David's correction asks for: what a region shows is
// laid out for the region, not for the slot its rectangle occupies on
// the world line.

import { describe, expect, it } from 'bun:test';

import { REGION_CANVAS, regionCanvas } from './region-canvas';
import { interiorLayout } from './region-contents';
import { TERRITORIES, WORLD, territoryOf } from './world';
import type { Wagon } from './yard-floor';

const wagon = (id: string): Wagon => ({
  id,
  tag: id,
  title: `car ${id}`,
  branch: `fix/${id}`,
  head: null,
  kind: 'backlog-item',
  sim: false,
  station: 'dock',
  slot: 0,
  trainId: null,
  tone: 'static',
  lamp: 'off',
  status: 'standing',
  since: null,
});

describe('the region canvas', () => {
  it('is as wide as the world, so the two views scale alike', () => {
    expect(REGION_CANVAS.width).toBe(WORLD.width);
  });

  it('gives every region the SAME room, whatever its slot on the line', () => {
    // The defect the swap removes: on the world line a territory's
    // rect is its slot, so a narrow region laid out its contents in a
    // narrow box for a reason that had nothing to do with the region.
    const rects = TERRITORIES.map((t) => regionCanvas(t.name));
    const widths = new Set(rects.map((r) => r.w));
    const heights = new Set(rects.map((r) => r.h));
    expect(widths.size).toBe(1);
    expect(heights.size).toBe(1);

    // …and that room is not the room the world gave it.
    const slots = new Set(TERRITORIES.map((t) => t.w));
    expect(slots.size).toBeGreaterThan(1);
  });

  it('fits strictly more wagons than the same region did on the world line', () => {
    const wagons = Array.from({ length: 60 }, (_, i) => wagon(`c${i}`));
    for (const t of TERRITORIES) {
      const onTheLine = interiorLayout(t, wagons);
      const onItsOwnCanvas = interiorLayout(regionCanvas(t.name), wagons);
      expect(onItsOwnCanvas.placed.length).toBeGreaterThan(onTheLine.placed.length);
      expect(onItsOwnCanvas.hidden).toBeLessThan(onTheLine.hidden);
    }
  });

  it('starts at the origin — a region map is not a window onto the world', () => {
    const r = regionCanvas('dock');
    expect(r.x).toBe(0);
    expect(r.y).toBe(0);
    // The world's own dock sits somewhere along the line; the region
    // view does not inherit that offset.
    expect(territoryOf('dock')?.x).toBeGreaterThan(0);
  });
});
