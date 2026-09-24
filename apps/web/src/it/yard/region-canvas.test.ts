// A REGION MAP OWNS ITS CANVAS (backlog ca37478f). The contract these
// pin is the one David's correction asks for: what a region shows is
// laid out for the region, not for the slot its rectangle occupies on
// the world line.

import { describe, expect, it } from 'bun:test';

import { REGION_CANVAS, regionCanvas } from './region-canvas';
import { platformLayout, type Platform } from './world-interior';
import { MACHINERY_STRIP_H } from './world-machines';
import { TERRITORIES, WORLD, territoryOf } from './world';

// The layout these are measured through is `platformLayout`, the one
// left that divides a region's canvas: the wagon plates' grid
// (`interiorLayout`) was deleted with design fe77a1d2 car 3, when the
// region map began drawing its slice of the floor itself — that
// slice's own room is pinned in floor-slices.test.ts.
const platform = (name: string): Platform => ({
  key: name,
  name,
  standing: 3,
  bound: null,
  rate: 1,
  flag: { from: 'tail', n: 0 },
  note: '',
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

  it('fits strictly more than the same region did on the world line', () => {
    const platforms = Array.from({ length: 60 }, (_, i) => platform(`s${i}`));
    for (const t of TERRITORIES) {
      const onTheLine = platformLayout(t, platforms);
      const onItsOwnCanvas = platformLayout(regionCanvas(t.name), platforms);
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

// THE CANVAS IS HELD AGAINST WHAT IT HAS TO HOLD (backlog ba83225e).
//
// Until 2026-09-22 the canvas's 420 left room for the machinery strip
// by arithmetic coincidence: floor((420 - 64 - 8) / 24) = 14 platform
// rows ending at y 400, the strip's top edge exactly. `contentsBox`
// (backlog 3a916816) took the strip out of the division, but nothing
// held the canvas's HEIGHT against the constants it is divided by —
// and the layout forces a first row (`Math.max(1, …)`), so a canvas
// shrunk in the coming redesign would still draw one, over the strip.
// These are asserted on what the layout actually places, not on a copy
// of its constants, so a change to either side is caught here.

describe('the region canvas leaves room for its contents AND the whole strip', () => {
  const platforms = Array.from({ length: 60 }, (_, i) => platform(`s${i}`));

  for (const t of TERRITORIES) {
    const canvas = regionCanvas(t.name);
    const stripTop = canvas.y + canvas.h - MACHINERY_STRIP_H;

    it(`${t.name}: every platform row ends above the strip`, () => {
      const { placed } = platformLayout(canvas, platforms);
      expect(placed.length).toBeGreaterThan(0);
      for (const p of placed) expect(p.y + p.h, p.platform.name).toBeLessThanOrEqual(stripTop);
    });
  }
});

// THE "+N MORE" NOTE IS PLACED FROM THE LAYOUT, NOT THE CANVAS EDGE
// (backlog ba83225e). RegionMap drew it at REGION_CANVAS.height - 6 —
// y 414, inside the machinery strip — and it met no glyph only because
// the note is right-anchored and the glyphs fill from the left. The
// note now sits on the baseline just above the contents box, in the
// band the head clamp keeps (at least 24 px), which neither the rows
// nor the strip can reach. Each layout says where its own count goes,
// because each layout is what knows where its rows start.
describe('the overflow note', () => {
  /** A 9 px note's ascent, rounded up: `.tiny` is 9 px, and "+N more"
   *  has no descender, so the baseline is its lowest pixel. */
  const NOTE_ASCENT = 9;
  const platforms = Array.from({ length: 60 }, (_, i) => platform(`s${i}`));

  for (const t of TERRITORIES) {
    const canvas = regionCanvas(t.name);
    const stripTop = canvas.y + canvas.h - MACHINERY_STRIP_H;

    it(`${t.name}: sits inside the outline, above every row, and clear of the strip`, () => {
      const decks = platformLayout(canvas, platforms);
      const rows = [{ note: decks.note, tops: decks.placed.map((p) => p.y) }];
      for (const { note, tops } of rows) {
        expect(note.y).toBeLessThan(stripTop);
        expect(note.y - NOTE_ASCENT).toBeGreaterThanOrEqual(canvas.y);
        expect(note.x).toBeLessThanOrEqual(canvas.x + canvas.w);
        expect(tops.length).toBeGreaterThan(0);
        for (const top of tops) expect(note.y).toBeLessThanOrEqual(top);
      }
    });
  }
});
