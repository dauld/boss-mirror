import { describe, expect, it } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  FLOOR_LAYOUTS,
  FLOOR_PAD,
  FLOOR_REGIONS,
  MACHINE_REGION,
  WAGON_W,
  asFloorRegion,
  floorFrame,
  floorPlan,
  machineAt,
  regionFloorView,
} from './floor-slices';
import { REGION_CANVAS } from './region-canvas';
import { INTERIOR_REGIONS, regionOfStation } from './region-contents';
import { MACHINERY_STRIP_H } from './world-machines';
import type { DeliveryChannel } from './yard';
import { drawnWagons, type Bay, type Loco, type Scene, type Station, type Wagon } from './yard-floor';

// THE FLOOR, ONE REGION AT A TIME (design fe77a1d2, car 1; backlog
// c0565f48). YardMap drew the whole floor out of one `wagonXY` switch
// and one block of layout arithmetic, so no region could draw its own
// part of it. The layout is now six pure functions, one per floor
// region, keyed through STATION_REGION, and YardMap draws the union.
// This car changes nothing on screen, so what is pinned here is exactly
// that: the six slices together are what YardMap drew — the same
// wagon ids, the same bays, the same locomotives, at the same
// coordinates — and every wagon stands in exactly one slice.

const wagon = (
  id: string,
  station: Station,
  slot = 0,
  extra: Partial<Pick<Wagon, 'trainId' | 'siding'>> = {},
): Wagon => ({
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
  ...extra,
});

const bay = (index: number): Bay => ({
  index,
  busy: false,
  branch: null,
  packetId: null,
  wagonId: null,
  tag: null,
  since: null,
  elapsed: null,
  stale: false,
  progress: 0,
});

const loco = (id: string, stage: number, progress: number): Loco => ({
  id,
  n: null,
  title: `train ${id}`,
  stage,
  progress,
  blocked: null,
  channel: null,
  cars: [],
});

const onSiding = (id: string, slot: number, siding?: DeliveryChannel): Wagon =>
  wagon(id, 'arrivals', slot, siding ? { siding } : {});

/** A floor with something standing at every station: three bays, a
 *  queue two rows deep (so the mainline drops), a train with cars
 *  aboard, a car whose train is not on the floor, a full software
 *  siding with one wagon past the plate, and a wagon in each lane of
 *  the inspection band. */
const SCENE = {
  now: '2026-09-24T01:00:00Z',
  wagons: [
    wagon('a1', 'approach', 0),
    wagon('a2', 'approach', 1),
    wagon('q0', 'gate-queue', 0),
    wagon('q1', 'gate-queue', 1),
    wagon('q2', 'gate-queue', 2),
    wagon('q3', 'gate-queue', 3),
    wagon('q4', 'gate-queue', 4),
    wagon('g1', 'gate', 1),
    wagon('l0', 'limbo', 0),
    wagon('d0', 'dock', 0),
    wagon('d1', 'dock', 1),
    wagon('r0', 'garage', 0),
    wagon('t0', 'train', 0, { trainId: 'T' }),
    wagon('t1', 'train', 1, { trainId: 'T' }),
    wagon('t2', 'train', 0, { trainId: 'gone' }),
    onSiding('s0', 0, 'data'),
    onSiding('s1', 0, 'software'),
    onSiding('s2', 1),
    onSiding('s5', 5, 'software'),
    onSiding('s6', 6, 'software'),
    wagon('x0', 'cancelled', 0),
    wagon('i0', 'inspection-shed', 0),
    wagon('i1', 'inspection-shed', 8),
    wagon('e0', 'siding-event', 0),
    wagon('n0', 'siding-no-probe', 3),
  ],
  locos: [loco('T', 2, 0.5), loco('U', 6, 1)],
  bays: [bay(0), bay(1), bay(2)],
  signals: [],
  boardRows: [],
  machines: {},
} as unknown as Scene;

describe('the floor splits into one slice per region', () => {
  const plan = floorPlan(SCENE);

  it('has exactly the six floor regions STATION_REGION names, and no other', () => {
    expect(Object.keys(plan.slices).sort()).toEqual([...INTERIOR_REGIONS].sort());
    for (const [region, slice] of Object.entries(plan.slices)) expect(slice.region as string).toBe(region);
  });

  it('stands every drawn wagon in exactly one slice — the one its station maps to', () => {
    const drawn = drawnWagons(SCENE.wagons).drawn;
    for (const w of drawn) {
      const holding = Object.values(plan.slices).filter(s => s.wagons.some(p => p.wagon.id === w.id));
      expect(holding.map(s => s.region), w.id).toEqual([regionOfStation(w.station)]);
    }
  });

  it('draws together what YardMap drew: the same wagon ids in the same order, the same bays, the same locomotives', () => {
    const slices = Object.values(plan.slices);
    // The union is the drawn set — the wagon past the siding's plate is
    // counted on the plate and drawn in no slice, exactly as before.
    expect(plan.wagons.map(p => p.wagon.id)).toEqual(drawnWagons(SCENE.wagons).drawn.map(w => w.id));
    expect(plan.wagons.map(p => p.wagon.id)).not.toContain('s6');
    expect(slices.flatMap(s => s.wagons).length).toBe(plan.wagons.length);
    expect(slices.flatMap(s => s.bays.map(b => b.bay.index))).toEqual([0, 1, 2]);
    expect(slices.flatMap(s => s.locos.map(l => l.loco.id))).toEqual(['T', 'U']);
    // The bays are the gates region's machines; the locomotives run on the track.
    expect(plan.slices.gates.bays.length).toBe(3);
    expect(plan.slices.track.locos.length).toBe(2);
  });

  it('each region is its own function: calling one alone gives the slice the plan holds', () => {
    const frame = floorFrame(SCENE);
    for (const region of INTERIOR_REGIONS) {
      const layout = FLOOR_LAYOUTS[region as keyof typeof FLOOR_LAYOUTS];
      expect(layout(SCENE, frame), region).toEqual(plan.slices[region as keyof typeof plan.slices]);
    }
  });

  it('is pure: the same scene lays out the same floor', () => {
    expect(floorPlan(SCENE)).toEqual(plan);
  });
});

// The coordinates below were computed from YardMap.svelte as it stood
// on origin/main 518a4f8d (its `wagonXY`, `locoX` and layout block) for
// the scene above. They are the "nothing changes on screen" half of this
// car: a slice that moves a wagon by a pixel fails here.
describe('nothing moves on screen', () => {
  const plan = floorPlan(SCENE);
  const at = (id: string) => {
    const p = plan.wagons.find(q => q.wagon.id === id);
    return p ? [p.x, p.y] : null;
  };

  it('keeps the frame YardMap computed: the mainline drops under a two-row queue, the band grows under it', () => {
    const f = plan.frame;
    expect(f.width).toBe(1240);
    expect(f.mainY).toBe(330);
    expect(f.queueTop).toBe(234);
    expect(f.queueRows).toBe(2);
    expect(f.limboY).toBe(222);
    expect(f.cancelledY).toBe(552);
    expect(f.shedY).toBe(596);
    expect(f.shedRows).toBe(1);
    expect(f.eventY).toBe(652);
    expect(f.noProbeY).toBe(706);
    expect(f.laneBottom).toBe(748);
    expect(f.height).toBe(758);
  });

  it('keeps an empty floor at the height it always had', () => {
    const empty = floorPlan({ ...SCENE, wagons: [], locos: [], bays: [] } as Scene);
    expect(empty.frame.mainY).toBe(250);
    expect(empty.frame.height).toBe(678);
    expect(empty.frame.limboY).toBe(118);
  });

  it('stands the gates region where it stood: approach, queue rows, bays, limbo', () => {
    expect(at('a1')).toEqual([34, 328]);
    expect(at('a2')).toEqual([108, 328]);
    expect(at('q0')).toEqual([30, 234]);
    expect(at('q3')).toEqual([252, 234]);
    expect(at('q4')).toEqual([30, 268]);
    expect(at('g1')).toEqual([240, 138]);
    expect(at('l0')).toEqual([380, 222]);
    expect(plan.slices.gates.bays.map(b => b.y)).toEqual([70, 122, 174]);
  });

  it('stands the dock, the garage and the track where they stood', () => {
    expect(at('d0')).toEqual([412, 328]);
    expect(at('d1')).toEqual([486, 328]);
    expect(at('r0')).toEqual([436, 398]);
    expect(plan.slices.track.locos.map(l => [l.x, l.y])).toEqual([
      [820, 328],
      [1056, 328],
    ]);
    // Cars trail their locomotive; a car whose train is not on the
    // floor stands behind the first signal.
    expect(at('t0')).toEqual([744, 328]);
    expect(at('t1')).toEqual([670, 328]);
    expect(at('t2')).toEqual([544, 328]);
  });

  it('stands the arrivals sidings and the inspection band where they stood', () => {
    expect(at('s0')).toEqual([640, 390]);
    expect(at('s1')).toEqual([640, 466]);
    expect(at('s2')).toEqual([714, 466]);
    expect(at('s5')).toEqual([1010, 466]);
    expect(at('x0')).toEqual([640, 552]);
    expect(at('i0')).toEqual([640, 610]);
    expect(at('i1')).toEqual([640, 644]);
    expect(at('e0')).toEqual([640, 666]);
    expect(at('n0')).toEqual([862, 720]);
  });
});

// ---------------------------------------------------------------------
// CAR 2 (design fe77a1d2): A REGION MAP DRAWS ITS OWN SLICE.
// ---------------------------------------------------------------------
//
// Until this car /it/yard/<region> drew the region's wagons twice — a
// plate on the region map, and a wagon on the whole six-region floor
// YardMap drew under it — and only the second could be clicked. The
// region map now draws that region's slice of the floor, clickable, and
// the whole floor is gone. `floorPlan` is what YardMap drew (the block
// above pins it to YardMap's own coordinates), so the acceptance check
// is held against it: every wagon YardMap drew in a region's stations
// is on that region's map, inside its canvas, and a button selecting
// it.

describe("a region map draws its own slice of the floor", () => {
  const plan = floorPlan(SCENE);
  const drawnIn = (region: string) =>
    drawnWagons(SCENE.wagons)
      .drawn.filter(w => regionOfStation(w.station) === region)
      .map(w => w.id);

  it('draws every wagon YardMap drew in the region — and none of another region', () => {
    for (const region of FLOOR_REGIONS) {
      const view = regionFloorView(region, SCENE);
      expect(view.slice.wagons.map(p => p.wagon.id), region).toEqual(drawnIn(region));
      expect(view.slice, region).toEqual(plan.slices[region]);
    }
    // Together the six maps hold exactly what the one floor held.
    const all = FLOOR_REGIONS.flatMap(r => regionFloorView(r, SCENE).slice.wagons.map(p => p.wagon.id));
    expect([...all].sort()).toEqual(plan.wagons.map(p => p.wagon.id).sort());
  });

  it('draws each mark inside its canvas and above the machinery strip', () => {
    for (const region of FLOOR_REGIONS) {
      const v = regionFloorView(region, SCENE);
      const floorBottom = v.height - MACHINERY_STRIP_H;
      const inside = (what: string, x: number, top: number, right: number, bottom: number) => {
        expect(x, what).toBeGreaterThanOrEqual(0);
        expect(right, what).toBeLessThanOrEqual(v.width);
        expect(top + v.dy, what).toBeGreaterThanOrEqual(0);
        expect(bottom + v.dy, what).toBeLessThanOrEqual(floorBottom);
      };
      for (const p of v.slice.wagons) inside(`${region}/${p.wagon.id}`, p.x, p.y - 10, p.x + WAGON_W, p.y + 16);
      for (const p of v.slice.locos) inside(`${region}/train ${p.loco.id}`, p.x, p.y - 36, p.x + 50, p.y + 19);
      for (const p of v.slice.bays) inside(`${region}/bay ${p.bay.index}`, 226, p.y - 22, 376, p.y + 32);
    }
  });

  it('is as wide as a region canvas, and only as tall as its own slice needs', () => {
    for (const region of FLOOR_REGIONS) {
      const v = regionFloorView(region, SCENE);
      expect(v.width, region).toBe(REGION_CANVAS.width);
      expect(v.height, region).toBe(v.band.bottom - v.band.top + 2 * FLOOR_PAD + MACHINERY_STRIP_H);
      expect(v.dy, region).toBe(FLOOR_PAD - v.band.top);
    }
    // The dock is one stretch of the mainline, not the whole floor's height.
    expect(regionFloorView('dock', SCENE).height).toBeLessThan(plan.frame.height);
  });

  it('grows a region to hold what stands in it, rather than cutting it off', () => {
    // Twelve cars in limbo stack upward from the last bay, past the
    // gates' sign: the band reaches up to the highest of them.
    const limbo = Array.from({ length: 12 }, (_, i) => wagon(`lb${i}`, 'limbo', i));
    const crowded = { ...SCENE, wagons: [...SCENE.wagons, ...limbo] } as Scene;
    const v = regionFloorView('gates', crowded);
    const top = Math.min(...v.slice.wagons.map(p => p.y - 10));
    expect(top).toBeLessThan(0);
    expect(v.band.top).toBe(top);
    expect(top + v.dy).toBe(FLOOR_PAD);
  });

  it('stands the three machines no station keys in a region, inside its band', () => {
    // The server already places the conductor on the track and the
    // converge runner in arrivals (boss_jobs::regions); the cluster
    // tower reports the build that runner deployed, so it stands beside it.
    expect(MACHINE_REGION).toEqual({ runner: 'arrivals', cluster: 'arrivals', conductor: 'track' });
    const f = floorFrame(SCENE);
    const at = machineAt(f);
    const boxes = {
      runner: { top: at.runner.y - 29, bottom: at.runner.y + 46 },
      cluster: { top: at.cluster.y, bottom: at.cluster.y + 76 },
      conductor: { top: at.conductor.y - 30, bottom: at.conductor.y + 62 },
    };
    for (const [m, b] of Object.entries(boxes)) {
      const v = regionFloorView(MACHINE_REGION[m as keyof typeof MACHINE_REGION], SCENE);
      expect(b.top, m).toBeGreaterThanOrEqual(v.band.top);
      expect(b.bottom, m).toBeLessThanOrEqual(v.band.bottom);
    }
  });

  it('names only the six floor regions', () => {
    expect([...FLOOR_REGIONS].sort() as string[]).toEqual([...INTERIOR_REGIONS].sort());
    expect(asFloorRegion('dock')).toBe('dock');
    expect(asFloorRegion('receiving')).toBeNull();
    expect(asFloorRegion('atlantis')).toBeNull();
  });
});

// The region map draws FROM the view, so the checks above are a claim
// about the picture rather than about a module nothing renders — and
// every mark on it is a button into the entity panel, as YardMap's
// were. Pinned at source in the yard-page-*.test.ts idiom; the mocked
// spec (it-region-map) clicks one.
describe('the region map renders its slice, and every mark on it selects', () => {
  const floor = readFileSync(join(import.meta.dir, 'RegionFloor.svelte'), 'utf8');
  const map = readFileSync(join(import.meta.dir, 'RegionMap.svelte'), 'utf8');
  const page = readFileSync(join(import.meta.dir, 'YardPage.svelte'), 'utf8');
  const strip = (s: string) => s.replace(/<!--[\s\S]*?-->/g, '');

  it('draws its wagons, bays and locomotives from the view, keeping no placement of its own', () => {
    expect(floor).toContain("from './floor-slices'");
    expect(floor).toMatch(/\{#each view\.slice\.wagons as p \(p\.wagon\.id\)\}/);
    expect(floor).toMatch(/\{#each view\.slice\.bays as p \(p\.bay\.index\)\}/);
    expect(floor).toMatch(/\{#each view\.slice\.locos as p \(p\.loco\.id\)\}/);
    expect(floor).not.toContain('function wagonXY');
    expect(floor).not.toContain('function locoX');
  });

  it('makes every wagon, bay and locomotive a button that selects it', () => {
    for (const key of ['`car:${w.id}`', '`bay:${b.index}`', '`train:${l.id}`']) {
      expect(floor, key).toContain(`onclick={pick(${key})}`);
      expect(floor, key).toContain(`onkeydown={pickKey(${key})}`);
    }
    expect(floor).toMatch(/class="token wagon[^"]*"[\s\S]*?role="button"[\s\S]*?tabindex="0"/);
  });

  it('is mounted by the region map with the selection, in place of the plates', () => {
    expect(map).toContain("import RegionFloor from './RegionFloor.svelte'");
    expect(strip(map)).toMatch(/<RegionFloor[^>]*\{selected\}[^>]*\{onselect\}/);
    expect(map).not.toContain('interiorLayout');
  });

  it('leaves no second map under it: the yard page draws none, and YardMap is gone', () => {
    expect(strip(page)).not.toContain('<YardMap');
    expect(page).not.toContain("import YardMap");
    expect(existsSync(join(import.meta.dir, 'YardMap.svelte'))).toBe(false);
  });
});
