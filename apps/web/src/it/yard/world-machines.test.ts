import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { parseRegions, type Machine, type MachineState } from './regions';
import { territoryOf } from './world';
import {
  MACHINERY_STRIP_H,
  machineOrder,
  machineTitle,
  machineryLabel,
  machineryStrip,
} from './world-machines';

// THE MACHINERY GLYPHS (design d2154293, car 5). The packet's pin, in
// two halves: every machine the read answers is DRAWN (or counted, and
// never a failed one), and the state set is CLOSED — a fifth state is a
// throw at the parse, not a glyph that quietly reads as calm.

const machine = (over: Partial<Machine> = {}): Machine => ({
  id: 'gate-bay-1',
  name: 'bay 1',
  state: 'running',
  why: 'gating feat/x since 11:00',
  ...over,
});

const STATES: ReadonlyArray<MachineState> = ['running', 'idle', 'failed', 'unknown'];

describe('the state set is closed', () => {
  it('parses every state the server can answer, and throws on one it cannot', () => {
    const payload = (state: string) => ({
      window_hours: 24,
      regions: [
        {
          name: 'gates',
          count: 1,
          state: 'clear',
          why: '1 of 4 bays in use',
          trend: { metric: 'gate duration', unit: 'minutes', current: 11, previous: 9, samples: 4, previous_samples: 3 },
          machines: [{ id: 'gate-bay-1', name: 'bay 1', state, why: 'free' }],
        },
      ],
    });
    for (const state of STATES) {
      const parsed = parseRegions(payload(state));
      expect(parsed.regions[0]!.machines[0]!.state).toBe(state);
    }
    expect(() => parseRegions(payload('fine'))).toThrow(/unknown state/);
    // An older server sends no machines at all: no glyphs, never
    // invented idle ones.
    const older = parseRegions({
      window_hours: 24,
      regions: [
        {
          name: 'gates',
          count: 1,
          state: 'clear',
          why: '1 of 4 bays in use',
          trend: { metric: 'gate duration', unit: 'minutes', current: null, previous: null, samples: 0, previous_samples: 0 },
        },
      ],
    });
    expect(older.regions[0]!.machines).toEqual([]);
  });
});

describe('machineryStrip — every machine the read answers is drawn', () => {
  const gates = territoryOf('gates')!;

  it('places one glyph per machine, in a row inside the territory, none overlapping', () => {
    const machines = [
      machine({ id: 'gate-bay-1', state: 'running' }),
      machine({ id: 'gate-bay-2', state: 'idle', why: 'free' }),
      machine({ id: 'gate-bay-3', state: 'idle', why: 'free' }),
    ];
    const { placed, hidden } = machineryStrip(gates, machines);
    expect(hidden).toBe(0);
    expect(placed.length).toBe(machines.length);
    expect(placed.map((p) => p.machine.id)).toEqual(machines.map((m) => m.id));
    for (const p of placed) {
      expect(p.x).toBeGreaterThanOrEqual(gates.x);
      expect(p.x + p.w).toBeLessThanOrEqual(gates.x + gates.w);
      expect(p.y).toBeGreaterThanOrEqual(gates.y + gates.h - MACHINERY_STRIP_H);
      expect(p.y + p.h).toBeLessThanOrEqual(gates.y + gates.h);
    }
    for (let i = 1; i < placed.length; i += 1) {
      expect(placed[i]!.x).toBeGreaterThanOrEqual(placed[i - 1]!.x + placed[i - 1]!.w);
    }
  });

  it('counts what does not fit and never cuts a failure or an unknown', () => {
    // Forty stations against a territory that holds a handful: the
    // ones that get cut are the ones nothing is wrong with.
    const many = [
      ...Array.from({ length: 40 }, (_, i) => machine({ id: `station:i${i}`, state: 'idle' })),
      machine({ id: 'station:zz-broken', state: 'failed', why: '7 standing — over its WIP limit' }),
      machine({ id: 'station:zz-blind', state: 'unknown', why: 'the flow cube is blind to it' }),
    ];
    const { placed, hidden } = machineryStrip(gates, many);
    expect(placed.length + hidden).toBe(many.length);
    expect(hidden).toBeGreaterThan(0);
    expect(placed[0]!.machine.state).toBe('failed');
    expect(placed[1]!.machine.state).toBe('unknown');
  });

  it('orders by what matters, then by id — a glyph keeps its place between reads', () => {
    const a = machine({ id: 'b', state: 'idle' });
    const b = machine({ id: 'a', state: 'idle' });
    const c = machine({ id: 'c', state: 'failed' });
    expect(machineOrder([a, b, c]).map((m) => m.id)).toEqual(['c', 'a', 'b']);
    // Same machines, read again in a different order: same drawing.
    expect(machineOrder([c, b, a]).map((m) => m.id)).toEqual(['c', 'a', 'b']);
  });

  it('says a region with no machinery has none, and counts the rest by state', () => {
    expect(machineryLabel([])).toBe('no machinery of ours works here');
    expect(machineryLabel([machine({ state: 'unknown' }), machine({ id: 'x', state: 'idle' })])).toBe(
      '2 machines: 1 unknown, 1 idle',
    );
    expect(machineTitle(machine({ state: 'failed', name: 'conductor', why: 'SILENT' }))).toBe(
      'conductor · failed — SILENT',
    );
  });
});

describe('the map draws what the strip places', () => {
  const src = readFileSync(join(import.meta.dir, 'WorldMap.svelte'), 'utf8');

  it('renders a glyph per placed machine, with the overflow counted', () => {
    expect(src).toContain('machineryStrip');
    expect(src).toContain('data-machine=');
    expect(src).toContain('machinery.hidden');
  });

  it('gives every state of the closed set a class of its own — no state falls through to idle', () => {
    for (const state of STATES) {
      expect(src, `the glyph styles ${state}`).toContain(`.glyph.${state}`);
    }
    // Unknown is a different SHAPE, not a different shade: the dashed
    // housing and the mark are what tell it from idle at world scale.
    expect(src).toContain('stroke-dasharray');
  });
});
