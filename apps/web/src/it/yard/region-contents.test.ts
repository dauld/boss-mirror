import { describe, expect, it } from 'bun:test';
import { REGION_NAMES } from './regions';
import { territoryOf } from './world';
import { INTERIOR_REGIONS, hasInterior, regionOfStation } from './region-contents';
import type { Station } from './yard-floor';

// WHAT A REGION CONTAINS (backlog ca37478f). A click no longer walks a
// camera into a territory — the world map is REPLACED by that region's
// own map — so what is pinned here is the part that survived: which
// stations belong to which region. The camera's arithmetic (zoomBoxOf,
// lerpBox, easeInOut, viewBoxText) is deleted, and so are its tests;
// so are the wagon plates' (interiorWagons, interiorLayout — design
// fe77a1d2 car 3), since the region map draws its slice of the floor
// itself and floor-slices.test.ts pins where each wagon lands.

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
});

