// The factory floor: the Train Yard as a living production line.
//
// David, 2026-09-05: "improve the fun factor of watching the Train Yard
// ... add animation for the cars moving around and make it look more
// like Factorio." He watches this page all day; a car that visibly
// rolls from the approach to a gate machine to the assembly track and
// out on a train is the same pipeline the board below renders in a
// table, told as motion instead of rows.
//
// This module is the PURE half: it maps the yard read-model into a
// left-to-right line of stations, each holding wagons (cars) and — at
// the gates — machines. The Svelte component animates it; a car keeps
// its stable id across the 10s poll, so when its station changes the
// keyed render slides it out of one bay and into the next, and the
// movement is the state change made visible. No new data: the same
// `YardState` and gate slots the board already fetches.

import type { YardState } from './yard';
import type { GateSlot } from './yard-status';

/** One car (or a whole train) as a token on the factory floor. */
export type Wagon = Readonly<{
  /** Stable across polls: the car id, the train id, or the branch. A
   *  wagon that keeps its id when its station changes is animated as a
   *  move rather than a leave-and-enter. */
  id: string;
  /** Short label — a branch or a train title. */
  label: string;
  /** Protocol kind, for the shared hue (a wagon is the same color as
   *  its packet card everywhere else). */
  kind: string;
  sim: boolean;
  /** What the wagon is doing, which the component paints as an accent
   *  and, for a train, an animation. */
  tone: WagonTone;
  /** A locomotive pulling `cars` wagons, rather than a lone car. */
  isTrain: boolean;
  /** Cars aboard, when this is a train. */
  cars: number;
  /** A one-line status the component can show on hover / beneath. */
  detail: string;
  /** The packet to open on click, when there is one. */
  packetId: string | null;
}>;

export type WagonTone =
  | 'queued' // inbound, waiting to gate (publish rows)
  | 'gating' // in a gate machine right now
  | 'red' // last gate red
  | 'green' // gated green, parked or awaiting board
  | 'boarding' // on the dock / assembling
  | 'ci' // train assembled, CI running
  | 'blocked' // train stuck (red CI / trouble)
  | 'moving' // departed, in transit
  | 'arrived'; // landed

export type Machine = Readonly<{
  id: string;
  busy: boolean;
  label: string;
  branch: string | null;
  since: string | null;
  packetId: string | null;
}>;

export type FactoryStation = Readonly<{
  key: string;
  label: string;
  wagons: readonly Wagon[];
  /** Only the gates station has machines. */
  machines: readonly Machine[];
}>;

const shorten = (branch: string): string => {
  // Drop the feat/ fix/ prefix for the wagon label; the belt is narrow.
  const slash = branch.indexOf('/');
  return slash >= 0 ? branch.slice(slash + 1) : branch;
};

/** Map the yard read-model to the factory line, left to right in the
 *  order a change travels. Pure: same inputs, same line. */
export function factoryStations(
  yard: YardState,
  slots: readonly GateSlot[],
): FactoryStation[] {
  // APPROACH — inbound rows that have not reached the dock. A row being
  // gated right now is drawn in a GATES machine, not here, so the two
  // never double-count (the board makes the same split).
  const approach: Wagon[] = yard.approach
    .filter(a => a.state !== 'gated-green' || true)
    .map(a => ({
      id: a.id,
      label: shorten(a.branch),
      kind: 'gate-run',
      sim: false,
      tone:
        a.state === 'gated-red'
          ? ('red' as const)
          : a.state === 'gated-green'
            ? ('green' as const)
            : ('queued' as const),
      isTrain: false,
      cars: 0,
      detail:
        a.state === 'publishing'
          ? 'publishing'
          : a.state === 'gated-red'
            ? 'gate red — rework'
            : 'gated green',
      packetId: a.id,
    }));

  // GATES — the machines. N bays (the live policy capacity), each empty
  // or working a car. The working ones also ride as wagons so a narrow
  // floor still shows what is inside.
  const machines: Machine[] = slots.map((s, i) => ({
    id: `gate-${i}`,
    busy: s.kind === 'occupied',
    label: `gate ${i + 1}`,
    branch: s.kind === 'occupied' ? shorten(s.gate.branch) : null,
    since: s.kind === 'occupied' ? s.gate.since : null,
    packetId: s.kind === 'occupied' ? s.gate.packet_id : null,
  }));
  const gating: Wagon[] = slots
    .filter((s): s is Extract<GateSlot, { kind: 'occupied' }> => s.kind === 'occupied')
    .map(s => ({
      id: s.gate.packet_id,
      label: shorten(s.gate.branch),
      kind: 'gate-run',
      sim: false,
      tone: 'gating' as const,
      isTrain: false,
      cars: 0,
      detail: `gating since ${s.gate.since}`,
      packetId: s.gate.packet_id,
    }));

  // DOCK — parked cars, gated green, waiting for a train window.
  const dock: Wagon[] = yard.dock.map(c => ({
    id: c.id,
    label: shorten(c.branch),
    kind: c.kind,
    sim: c.sim,
    tone: 'boarding' as const,
    isTrain: false,
    cars: 0,
    detail: c.skipReason ? `held: ${c.skipReason}` : 'parked — awaiting a window',
    packetId: c.id,
  }));

  // ASSEMBLY — trains still in the yard (boarding / assembled, CI
  // running), drawn as a locomotive pulling its consist.
  const assembly: Wagon[] = yard.inFlight
    .filter(t => t.status === 'BOARDING' || t.status === 'BOARDED')
    .map(t => ({
      id: t.id,
      label: t.title.replace(/^PR train\s*/i, ''),
      kind: 'pr-train',
      sim: false,
      tone: (t.trouble
        ? 'blocked'
        : t.lamp === 'failing'
          ? 'blocked'
          : t.status === 'BOARDED'
            ? 'ci'
            : 'boarding') as WagonTone,
      isTrain: true,
      cars: t.cars.length,
      detail: t.trouble ? 'TROUBLE' : t.status === 'BOARDED' ? 'CI running' : 'boarding',
      packetId: t.id,
    }));

  // TRANSIT — departed, merged, rolling to the cluster.
  const transit: Wagon[] = yard.inFlight
    .filter(t => t.status === 'DEPARTED')
    .map(t => ({
      id: t.id,
      label: t.title.replace(/^PR train\s*/i, ''),
      kind: 'pr-train',
      sim: false,
      tone: 'moving' as const,
      isTrain: true,
      cars: t.cars.length,
      detail: t.deployed ? 'deployed — converging' : 'merged — deploying',
      packetId: t.id,
    }));

  // ARRIVED — recent landings, the payoff.
  const arrived: Wagon[] = yard.arrivals.map(t => ({
    id: t.id,
    label: t.title.replace(/^PR train\s*/i, ''),
    kind: 'pr-train',
    sim: false,
    tone: 'arrived' as const,
    isTrain: true,
    cars: t.cars.length,
    detail: 'arrived',
    packetId: t.id,
  }));

  return [
    { key: 'approach', label: 'APPROACH', wagons: approach, machines: [] },
    { key: 'gates', label: 'GATES', wagons: gating, machines },
    { key: 'dock', label: 'DOCK', wagons: dock, machines: [] },
    { key: 'assembly', label: 'ASSEMBLY', wagons: assembly, machines: [] },
    { key: 'transit', label: 'TRANSIT', wagons: transit, machines: [] },
    { key: 'arrived', label: 'ARRIVED', wagons: arrived, machines: [] },
  ];
}

/** True when the whole floor is empty — the component shows an idle
 *  factory rather than six empty bays. */
export function floorIsIdle(stations: readonly FactoryStation[]): boolean {
  return stations.every(s => s.wagons.length === 0 && s.machines.every(m => !m.busy));
}
