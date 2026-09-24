// THE YARD FLOOR, LAID OUT ONE REGION AT A TIME (design fe77a1d2, car
// 1; backlog c0565f48).
//
// Until this car YardMap placed every token on the floor out of one
// `wagonXY` switch and one block of layout arithmetic inside the
// component, so the only thing that could draw the dock's sidings was
// the component that draws all six regions at once — which is why
// /it/yard/<region> stacks the whole floor under the region's own map.
// The design's end state is a region map that draws ITS slice of the
// floor; this module is the first step of it and changes nothing on
// screen.
//
// Two parts, because the regions are not independent vertically:
//
//   - `floorFrame` is the one place their heights meet. The gate bays
//     and the queue lane decide where the mainline runs; the arrivals
//     sidings hang under the mainline; the inspection band hangs under
//     the cancelled siding and grows with its lanes. That cascade is
//     the frame, computed once.
//   - one layout function per floor region (`FLOOR_LAYOUTS`), each a
//     pure function from the Scene and the frame to that region's
//     marks: the wagons standing at its stations, and the gates' bays
//     and the track's locomotives. The stations each function places
//     are keyed through STATION_REGION (`StationOf`), so a station
//     placed under the wrong region, or left unplaced, is a type error
//     rather than a wagon drawn nowhere.
//
// YardMap draws the union of the six at the coordinates it always drew
// (floor-slices.test.ts pins both halves). Car 2 has a region map call
// its own region's function; the static machines YardMap draws that
// no station keys — the deploy runner, the cluster tower and the
// conductor's clock — are not assigned a region here, and that
// assignment is car 2's to make.

import { DELIVERY_CHANNELS, type DeliveryChannel } from './yard';
import { regionOfStation, type FloorRegion, type StationOf } from './region-contents';
import { drawnWagons, type Bay, type Loco, type Scene, type Wagon } from './yard-floor';

// ---- the floor's measures: one definition each, read by YardMap ----

/** The floor's width; everything hangs off the mainline's y, which
 *  drops when the policy allows more than three bays. */
export const VIEW_W = 1240;
/** A wagon body is WAGON_W wide (an eleven-character nameplate at 9px
 *  mono fits), and slots step WAGON_STEP so neighbours never cover it. */
export const WAGON_W = 70;
export const WAGON_STEP = 74;
/** One signal per stage along the mainline. */
export const STAGE_X: readonly number[] = [620, 700, 780, 860, 940, 1000, 1056];
const BAY_H = 52;
// The QUEUE LANE — a holding siding between the mainline and the gate
// branch, drawn only when something waits. Runs stand in it in their
// place in line, four to a row, and the mainline drops to make room:
// the yard grows a lane rather than hiding one.
const QUEUE_PER_ROW = 4;
export const QUEUE_ROW_H = 34;
// THE ARRIVALS SIDINGS (design c6bd173e, car 1): four rows off one
// ladder down the left, one per delivery channel in DELIVERY_CHANNELS
// order — data, config, software, infra — then the cancelled siding a
// step apart, and the inspection band under them. A siding is ONE row
// of up to ARRIVALS_DRAWN wagons at the inspection lanes' columns,
// with its own "+N" plate at the row's end: a full software siding
// hides no data wagon. The single stack that stood here grew two
// columns downward (2026-09-08); four labelled rows across the same
// width stay readable at 1280 px.
const SIDING_ROW_H = 38;
/** Below the mainline's own sign ("The track · …" at mainY + 32). */
const SIDING_TOP = 60;
// THE INSPECTION SHED AND ITS TWO SIDINGS — the band under the
// arrivals ladder. Three lanes, each as tall as it needs to be: the
// yard GROWS a lane rather than hiding wagons, the way the gate queue
// does, so nothing here is ever capped or counted away.
const LANE_COLS = 8;
export const LANE_ROW_H = 34;
const LANE_X = 640;

/** The top of gate bay `i`'s shed row. */
export const bayY = (i: number): number => 70 + i * BAY_H;

/** Where the regions meet: every y one region hands the next. */
export type FloorFrame = Readonly<{
  width: number;
  height: number;
  mainY: number;
  queueTop: number;
  /** Rows the queue lane needs; 0 when nothing waits for a bay. */
  queueRows: number;
  limboY: number;
  cancelledY: number;
  shedY: number;
  /** Rows the inspection lane needs — the shed building's height. */
  shedRows: number;
  eventY: number;
  noProbeY: number;
  laneBottom: number;
}>;

const laneRows = (n: number): number => Math.max(1, Math.ceil(n / LANE_COLS));

/** The y of a queue-lane row. */
export const queueY = (f: FloorFrame, row: number): number => f.queueTop + row * QUEUE_ROW_H;

/** The y of arrivals siding `i`, in DELIVERY_CHANNELS order; the
 *  cancelled siding sits a step below the last. */
export const sidingY = (f: FloorFrame, i: number): number => f.mainY + SIDING_TOP + i * SIDING_ROW_H;

export function floorFrame(scene: Scene): FloorFrame {
  const nBays = scene.bays.length;
  const standing = (station: Wagon['station']): number => scene.wagons.filter(w => w.station === station).length;
  const queueRows = Math.ceil(standing('gate-queue') / QUEUE_PER_ROW);
  const mainY = Math.max(250, 70 + nBays * BAY_H + 24 + (queueRows > 0 ? queueRows * QUEUE_ROW_H + 12 : 0));
  const cancelledY = mainY + SIDING_TOP + DELIVERY_CHANNELS.length * SIDING_ROW_H + 10;
  // Below the cancelled siding's wheels, with room for the shed's sign —
  // the band must not sit on another machine's click area.
  const shedY = cancelledY + 44;
  const shedRows = laneRows(standing('inspection-shed'));
  const eventY = shedY + shedRows * LANE_ROW_H + 22;
  const noProbeY = eventY + laneRows(standing('siding-event')) * LANE_ROW_H + 20;
  const laneBottom = noProbeY + laneRows(standing('siding-no-probe')) * LANE_ROW_H + 8;
  return {
    width: VIEW_W,
    height: Math.max(mainY + 150, laneBottom + 10),
    mainY,
    queueTop: 70 + nBays * BAY_H + 8,
    queueRows,
    limboY: (nBays > 0 ? bayY(nBays - 1) : 70) + 48,
    cancelledY,
    shedY,
    shedRows,
    eventY,
    noProbeY,
    laneBottom,
  };
}

// ---- the slices ----

export type PlacedWagon = Readonly<{ wagon: Wagon; x: number; y: number }>;
/** A gate bay and the top of its shed row. */
export type PlacedBay = Readonly<{ bay: Bay; y: number }>;
export type PlacedLoco = Readonly<{ loco: Loco; x: number; y: number }>;

/** One region's part of the floor, in floor coordinates. */
export type FloorSlice = Readonly<{
  region: FloorRegion;
  wagons: readonly PlacedWagon[];
  bays: readonly PlacedBay[];
  locos: readonly PlacedLoco[];
}>;

/** Where a locomotive stands: between its stage's signal and the next,
 *  by its progress through the stage. */
function locoX(l: Loco): number {
  const a = STAGE_X[l.stage] ?? STAGE_X[STAGE_X.length - 1] ?? 0;
  const b = STAGE_X[l.stage + 1] ?? a;
  return a + (b - a) * Math.max(0, Math.min(1, l.progress));
}

type At = readonly [number, number];
type Placer = (w: Wagon, f: FloorFrame, scene: Scene) => At;
/** A region's placement: one entry per station STATION_REGION gives
 *  it — every one of them, and none of another region's. */
type Placers<R extends FloorRegion> = Readonly<Record<StationOf<R>, Placer>>;

const laneAt = (top: number, slot: number): At => [
  LANE_X + (slot % LANE_COLS) * WAGON_STEP,
  top + 14 + Math.floor(slot / LANE_COLS) * LANE_ROW_H,
];

const sidingIndex = (ch: DeliveryChannel | undefined): number =>
  Math.max(0, DELIVERY_CHANNELS.indexOf(ch ?? 'software'));

const GATES: Placers<'gates'> = {
  approach: (w, f) => [34 + w.slot * WAGON_STEP, f.mainY - 2],
  'gate-queue': (w, f) => [30 + (w.slot % QUEUE_PER_ROW) * WAGON_STEP, queueY(f, Math.floor(w.slot / QUEUE_PER_ROW))],
  gate: w => [240, bayY(w.slot) + 16],
  limbo: (w, f) => [380, f.limboY - w.slot * 24],
};
const DOCK: Placers<'dock'> = {
  dock: (w, f) => [412 + w.slot * WAGON_STEP, f.mainY - 2],
};
const GARAGE: Placers<'garage'> = {
  garage: (w, f) => [436 + w.slot * WAGON_STEP, f.mainY + 68],
};
const TRACK: Placers<'track'> = {
  // A car aboard trails its locomotive; one whose train is not on the
  // floor stands behind the first signal.
  train: (w, f, scene) => {
    const l = w.trainId ? scene.locos.find(x => x.id === w.trainId) : undefined;
    return [(l ? locoX(l) : (STAGE_X[0] ?? 0)) - (WAGON_W + 6) - w.slot * WAGON_STEP, f.mainY - 2];
  },
};
const ARRIVALS: Placers<'arrivals'> = {
  arrivals: (w, f) => [LANE_X + w.slot * WAGON_STEP, sidingY(f, sidingIndex(w.siding))],
  cancelled: (w, f) => [LANE_X + w.slot * WAGON_STEP, f.cancelledY],
};
const SHED: Placers<'shed'> = {
  'inspection-shed': (w, f) => laneAt(f.shedY, w.slot),
  'siding-event': (w, f) => laneAt(f.eventY, w.slot),
  'siding-no-probe': (w, f) => laneAt(f.noProbeY, w.slot),
};

/** The drawn wagons standing in `region`, placed. The wagons past a
 *  siding's plate (`drawnWagons`) are in no slice, as they were in no
 *  place on the floor. */
function wagonsIn<R extends FloorRegion>(region: R, placers: Placers<R>, scene: Scene, f: FloorFrame): readonly PlacedWagon[] {
  return drawnWagons(scene.wagons)
    .drawn.filter(w => regionOfStation(w.station) === region)
    .map(w => {
      // The filter above is what makes this station one of R's.
      const [x, y] = placers[w.station as StationOf<R>](w, f, scene);
      return { wagon: w, x, y };
    });
}

type Layout = (scene: Scene, f: FloorFrame) => FloorSlice;

/** One layout function per floor region. A region missing here, or a
 *  seventh, is a type error: the key set is STATION_REGION's values. */
export const FLOOR_LAYOUTS: Readonly<Record<FloorRegion, Layout>> = {
  gates: (scene, f) => ({
    region: 'gates',
    wagons: wagonsIn('gates', GATES, scene, f),
    bays: scene.bays.map(bay => ({ bay, y: bayY(bay.index) })),
    locos: [],
  }),
  dock: (scene, f) => ({ region: 'dock', wagons: wagonsIn('dock', DOCK, scene, f), bays: [], locos: [] }),
  garage: (scene, f) => ({ region: 'garage', wagons: wagonsIn('garage', GARAGE, scene, f), bays: [], locos: [] }),
  track: (scene, f) => ({
    region: 'track',
    wagons: wagonsIn('track', TRACK, scene, f),
    bays: [],
    locos: scene.locos.map(loco => ({ loco, x: locoX(loco), y: f.mainY - 2 })),
  }),
  arrivals: (scene, f) => ({ region: 'arrivals', wagons: wagonsIn('arrivals', ARRIVALS, scene, f), bays: [], locos: [] }),
  shed: (scene, f) => ({ region: 'shed', wagons: wagonsIn('shed', SHED, scene, f), bays: [], locos: [] }),
};

export type FloorPlan = Readonly<{
  frame: FloorFrame;
  slices: Readonly<Record<FloorRegion, FloorSlice>>;
  /** Every slice's wagons, in the scene's order — the order YardMap has
   *  always drawn them in, so the paint order where two overlap (a long
   *  consist trailing back over the dock) is the one it was. */
  wagons: readonly PlacedWagon[];
}>;

/** The whole floor: the frame, all six slices, and their union. */
export function floorPlan(scene: Scene): FloorPlan {
  const frame = floorFrame(scene);
  const slices = {
    gates: FLOOR_LAYOUTS.gates(scene, frame),
    dock: FLOOR_LAYOUTS.dock(scene, frame),
    garage: FLOOR_LAYOUTS.garage(scene, frame),
    track: FLOOR_LAYOUTS.track(scene, frame),
    arrivals: FLOOR_LAYOUTS.arrivals(scene, frame),
    shed: FLOOR_LAYOUTS.shed(scene, frame),
  };
  const order = new Map(scene.wagons.map((w, i) => [w.id, i] as const));
  const wagons = Object.values(slices)
    .flatMap(s => s.wagons)
    .sort((a, b) => (order.get(a.wagon.id) ?? 0) - (order.get(b.wagon.id) ?? 0));
  return { frame, slices, wagons };
}
