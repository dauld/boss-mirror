import { describe, expect, it } from 'bun:test';
import { PLANT_H, plantLayout } from './plant';
import type { Machine } from './regions';

// THE PLANT STRIP (design 62de32ae, decision 11; car E on backlog
// c3105b2a). The host runners serve every region, so they stand along
// the map's edge rather than as unlabelled 10px squares in receiving —
// where the review of 2026-09-24 found them and read the grouping as
// arbitrary. Each machine is a glyph AND its name and state in words;
// what does not fit is counted.

const machine = (id: string, state: Machine['state'], name = `${id} runner`): Machine => ({
  id: `runner:host:${id}`,
  name,
  state,
  why: `${state} for a reason`,
});

describe('plantLayout — the plant along the map\'s edge', () => {
  it('puts a failed machine first, then an unknown one, then the rest — what matters is never the one cut', () => {
    const laid = plantLayout([machine('a', 'idle'), machine('b', 'failed'), machine('c', 'unknown')], 1240);
    expect(laid.placed.map((p) => p.machine.id)).toEqual(['runner:host:b', 'runner:host:c', 'runner:host:a']);
    expect(laid.hidden).toBe(0);
  });

  it('writes each machine\'s name and its state in words beside its glyph, left to right without overlap', () => {
    const laid = plantLayout([machine('forge', 'idle'), machine('boss-gcp', 'running')], 1240);
    expect(laid.placed.map((p) => p.label)).toEqual(['boss-gcp runner · running', 'forge runner · idle']);
    const [a, b] = laid.placed;
    expect(a!.textX).toBeGreaterThan(a!.x);
    expect(b!.x).toBeGreaterThan(a!.textX + a!.label.length * 5);
    expect(a!.y + a!.h).toBeLessThanOrEqual(PLANT_H);
  });

  it('counts the machines a narrow strip cannot hold rather than dropping them', () => {
    const many = Array.from({ length: 40 }, (_, i) => machine(`host-${i}`, 'idle'));
    const laid = plantLayout(many, 600);
    expect(laid.placed.length).toBeGreaterThan(0);
    expect(laid.placed.length + laid.hidden).toBe(40);
    const last = laid.placed[laid.placed.length - 1]!;
    expect(last.textX + last.label.length * 6).toBeLessThanOrEqual(600);
  });
});
