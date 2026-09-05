import { describe, expect, it } from 'bun:test';
import { factoryStations, floorIsIdle } from './yard-factory';
import type { YardState } from './yard';
import type { GateSlot } from './yard-status';

// The mapping is the testable half; the animation is the component's.
// These pin that each stage of the yard lands in the right station,
// that a train reads as a locomotive with its car count, and that a
// clean floor reads idle.

const yardOf = (over: Partial<YardState>): YardState =>
  ({
    inFlight: [],
    dock: [],
    dockStation: { source: 'derived' },
    arrivals: [],
    cancelled: [],
    delivery: [],
    awaitingProof: [],
    approach: [],
    ...over,
  }) as unknown as YardState;

const approachRow = (id: string, state: string, branch: string) =>
  ({ id, branch, sha: null, state, opened_on: '2026-09-05', note: null }) as never;
const car = (id: string, branch: string, over: Record<string, unknown> = {}) =>
  ({ id, kind: 'ship-a-change', branch, title: branch, tags: [], sim: false, ...over }) as never;
const train = (id: string, status: string, over: Record<string, unknown> = {}) =>
  ({
    id,
    title: `PR train ${id}`,
    status,
    lamp: 'pending',
    cars: [car('c1', 'fix/a'), car('c2', 'fix/b')],
    live: true,
    outcome: 'unknown',
    trouble: null,
    deployed: null,
    ...over,
  }) as never;

const station = (sts: ReturnType<typeof factoryStations>, key: string) =>
  sts.find(s => s.key === key)!;

const busySlot = (branch: string): GateSlot =>
  ({ kind: 'occupied', gate: { branch, packet_id: `g-${branch}`, since: '3m' } }) as GateSlot;
const freeSlot: GateSlot = { kind: 'empty' } as GateSlot;

describe('factoryStations', () => {
  it('lays out the six stations in the order a change travels', () => {
    const s = factoryStations(yardOf({}), []);
    expect(s.map(x => x.key)).toEqual([
      'approach',
      'gates',
      'dock',
      'assembly',
      'transit',
      'arrived',
    ]);
  });

  it('a publish row is a queued wagon in APPROACH; a red one reads red', () => {
    const s = factoryStations(
      yardOf({
        approach: [
          approachRow('p1', 'publishing', 'fix/pub'),
          approachRow('r1', 'gated-red', 'fix/red'),
        ],
      }),
      [],
    );
    const ap = station(s, 'approach').wagons;
    expect(ap.map(w => w.tone)).toEqual(['queued', 'red']);
    expect(ap.every(w => !w.isTrain)).toBe(true);
  });

  it('an occupied gate is a machine that is busy AND a gating wagon', () => {
    const s = factoryStations(yardOf({}), [busySlot('fix/x'), freeSlot, freeSlot]);
    const g = station(s, 'gates');
    expect(g.machines.map(m => m.busy)).toEqual([true, false, false]);
    expect(g.wagons.length).toBe(1);
    expect(g.wagons[0]?.tone).toBe('gating');
    expect(g.wagons[0]?.packetId).toBe('g-fix/x');
  });

  it('dock cars are boarding wagons; a held one keeps its reason', () => {
    const s = factoryStations(
      yardOf({ dock: [car('d1', 'fix/dock', { skipReason: 'track occupied' })] }),
      [],
    );
    const d = station(s, 'dock').wagons;
    expect(d[0]?.tone).toBe('boarding');
    expect(d[0]?.detail).toContain('track occupied');
  });

  it('an assembling train is a locomotive with its car count; a departed one is in TRANSIT', () => {
    const s = factoryStations(
      yardOf({
        inFlight: [train('t1', 'BOARDED', { lamp: 'green' }), train('t2', 'DEPARTED')],
        arrivals: [train('t3', 'ARRIVED')],
      }),
      [],
    );
    const asm = station(s, 'assembly').wagons;
    expect(asm.length).toBe(1);
    expect(asm[0]?.isTrain).toBe(true);
    expect(asm[0]?.cars).toBe(2);
    expect(asm[0]?.tone).toBe('ci');
    expect(station(s, 'transit').wagons[0]?.tone).toBe('moving');
    expect(station(s, 'arrived').wagons[0]?.tone).toBe('arrived');
  });

  it('a troubled train reads blocked wherever it sits', () => {
    const s = factoryStations(
      yardOf({ inFlight: [train('t1', 'BOARDED', { trouble: { kind: 'stalled' } })] }),
      [],
    );
    expect(station(s, 'assembly').wagons[0]?.tone).toBe('blocked');
  });

  it('an empty yard with only free gates is idle', () => {
    expect(floorIsIdle(factoryStations(yardOf({}), [freeSlot, freeSlot]))).toBe(true);
    expect(floorIsIdle(factoryStations(yardOf({}), [busySlot('fix/x')]))).toBe(false);
    expect(floorIsIdle(factoryStations(yardOf({ dock: [car('d', 'fix/d')] }), []))).toBe(false);
  });
});
