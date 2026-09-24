import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { FLOOR_LAYOUTS, floorFrame, floorPlan } from './floor-slices';
import { INTERIOR_REGIONS, regionOfStation } from './region-contents';
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

// YardMap draws FROM the slices, so the equality above is a claim about
// the picture rather than about a module nothing renders. Pinned at
// source in the yard-page-*.test.ts idiom.
describe('YardMap draws the six slices', () => {
  const src = readFileSync(join(import.meta.dir, 'YardMap.svelte'), 'utf8');

  it('reads its layout from floorPlan and keeps no placement of its own', () => {
    expect(src).toContain("from './floor-slices'");
    expect(src).toMatch(/floorPlan\(scene\)/);
    expect(src).not.toContain('function wagonXY');
    expect(src).not.toContain('function locoX');
  });

  it('draws its wagons, bays and locomotives from the plan', () => {
    expect(src).toMatch(/\{#each plan\.wagons as /);
    expect(src).toMatch(/\{#each gates\.bays as /);
    expect(src).toMatch(/\{#each track\.locos as /);
  });
});
