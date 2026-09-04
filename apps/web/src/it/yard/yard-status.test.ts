import { describe, expect, test } from 'bun:test';
import {
  blockLabel,
  gateSlots,
  journeyText,
  parseYardStatus,
  phaseLabel,
  trainTone,
  type TrainStatus,
} from './yard-status';

describe('parseYardStatus', () => {
  test('deserializes a full payload once, shaping each section', () => {
    const raw = {
      trains: [
        {
          id: 't1',
          title: 'train #200',
          phase: 'deploying',
          at_step: 'Deployed to the playground',
          block: {
            kind: 'deploy-blocked',
            reason: 'deploy tree busy — will retry',
            since: '2026-09-03T06:46:00Z',
          },
          ci_result: 'green',
          pr_url: 'https://forge/pr/1',
          car_count: 3,
        },
      ],
      dock: [{ id: 'c1', title: 'A fix', branch: 'feat/a', parked_since: '2026-09-03' }],
      boarding: {
        dock_threshold: 4,
        cooldown_minutes: 120,
        at_times: ['06:00', '18:00'],
        dock_depth: 2,
        threshold_met: false,
        summary: 'Boards at 4 parked cars or 06:00 / 18:00 UTC; 2 car(s) parked now.',
      },
      recent: [{ id: 'r1', title: 'train #199', outcome: 'arrived', journey_seconds: 1800 }],
      stranded: [{ branch: 'feat/stranded' }],
      gates: {
        capacity: 4,
        active: [{ branch: 'feat/gating', packet_id: 'p1', since: '2026-09-03' }],
      },
      garage: [{ branch: 'feat/broken', failed_check: 'test', since: '2026-09-03' }],
      policy: { stall_hours: 6, max_red_trains: 2 },
      now: '2026-09-03T12:00:00Z',
    };
    const s = parseYardStatus(raw);
    expect(s.trains).toHaveLength(1);
    expect(s.trains[0]!.block).toEqual({
      kind: 'deploy-blocked',
      reason: 'deploy tree busy — will retry',
      since: '2026-09-03T06:46:00Z',
    });
    expect(s.boarding.dock_threshold).toBe(4);
    expect(s.boarding.at_times).toEqual(['06:00', '18:00']);
    expect(s.dock).toHaveLength(1);
    expect(s.recent[0]!.outcome).toBe('arrived');
    expect(s.stranded[0]!.branch).toBe('feat/stranded');
    expect(s.gates.capacity).toBe(4);
    expect(s.gates.active[0]!.branch).toBe('feat/gating');
    expect(s.garage[0]!.failed_check).toBe('test');
    expect(s.policy.stall_hours).toBe(6);
  });

  test('an absent gates/garage section degrades — never a throw', () => {
    // A build talking to a backend without the sections must still render.
    const s = parseYardStatus({ boarding: { dock_depth: 0, at_times: [], summary: 'x' } });
    expect(s.gates.capacity).toBe(0);
    expect(s.gates.active).toEqual([]);
    expect(s.garage).toEqual([]);
  });

  test('a garaged car with no named check keeps failed_check null', () => {
    const s = parseYardStatus({
      garage: [{ branch: 'feat/x', failed_check: null, since: '2026-09-03' }],
    });
    expect(s.garage[0]!.failed_check).toBeNull();
  });

  test('a missing block is null, not an error', () => {
    const s = parseYardStatus({
      trains: [{ id: 't', title: 'x', phase: 'boarding', car_count: 0 }],
      boarding: { dock_depth: 0, at_times: [], summary: 'x' },
    });
    expect(s.trains[0]!.block).toBeNull();
  });

  test('an unknown block kind renders as no-known-block, never a throw', () => {
    const s = parseYardStatus({
      trains: [{ id: 't', title: 'x', phase: 'deploying', block: { kind: 'from-the-future' }, car_count: 0 }],
      boarding: { dock_depth: 0, at_times: [], summary: 'x' },
    });
    expect(s.trains[0]!.block).toBeNull();
  });

  test('a null boarding block still parses to a well-formed predicate', () => {
    const s = parseYardStatus({ boarding: null });
    expect(s.boarding.dock_threshold).toBeNull();
    expect(s.boarding.at_times).toEqual([]);
  });

  test('a non-object throws so an outage renders failed, not empty', () => {
    expect(() => parseYardStatus('not the payload')).toThrow();
  });

  test('null policy fields stay null — never a fabricated default', () => {
    const s = parseYardStatus({ policy: {} });
    expect(s.policy.stall_hours).toBeNull();
    expect(s.policy.max_red_trains).toBeNull();
  });
});

describe('phaseLabel', () => {
  test('spells every phase for a human', () => {
    expect(phaseLabel('awaiting-ci')).toBe('awaiting CI');
    expect(phaseLabel('converging')).toBe('awaiting cluster convergence');
    expect(phaseLabel('deploying')).toBe('deploying');
  });
});

describe('blockLabel', () => {
  test('a deploy block leads with its reason — the buried fact, surfaced', () => {
    expect(
      blockLabel({ kind: 'deploy-blocked', reason: 'tree busy', since: null }),
    ).toBe('DEPLOY BLOCKED — tree busy');
  });
  test('a red CI names the failing check when known', () => {
    expect(blockLabel({ kind: 'ci-red', checks: 'test:FAILURE' })).toBe('CI RED — test:FAILURE');
    expect(blockLabel({ kind: 'ci-red', checks: null })).toBe('CI RED');
  });
  test('converge-overdue and stalled read plainly', () => {
    expect(blockLabel({ kind: 'converge-overdue' })).toContain('CONVERGE OVERDUE');
    expect(blockLabel({ kind: 'stalled', since: 'x' })).toContain('STALLED');
  });
  test('no block is null', () => {
    expect(blockLabel(null)).toBeNull();
  });
});

describe('trainTone', () => {
  const base: TrainStatus = {
    id: 't',
    title: 'x',
    phase: 'deploying',
    at_step: null,
    block: null,
    ci_result: null,
    pr_url: null,
    car_count: 0,
  };
  test('a blocked train is an error tone', () => {
    expect(trainTone({ ...base, block: { kind: 'converge-overdue' } })).toBe('err');
  });
  test('an in-flight train is active', () => {
    expect(trainTone(base)).toBe('active');
  });
  test('an arrived train is muted', () => {
    expect(trainTone({ ...base, phase: 'arrived' })).toBe('muted');
  });
});

describe('journeyText', () => {
  test('minutes under an hour, hours above, dash when unknown', () => {
    expect(journeyText(1800)).toBe('30m');
    expect(journeyText(5400)).toBe('1.5h');
    expect(journeyText(null)).toBe('—');
  });
});

describe('gateSlots', () => {
  const gate = (branch: string) => ({ branch, packet_id: `p-${branch}`, since: '2026-09-03' });

  test('fills the first slots and leaves the rest empty', () => {
    const slots = gateSlots({ capacity: 3, active: [gate('feat/a')] });
    expect(slots).toHaveLength(3);
    expect(slots[0]).toEqual({ kind: 'occupied', gate: gate('feat/a') });
    expect(slots[1]).toEqual({ kind: 'empty' });
    expect(slots[2]).toEqual({ kind: 'empty' });
  });

  test('an empty pipeline is all-free slots at capacity', () => {
    const slots = gateSlots({ capacity: 4, active: [] });
    expect(slots).toHaveLength(4);
    expect(slots.every((s) => s.kind === 'empty')).toBe(true);
  });

  test('over-admission (a count race) widens rather than hiding a running gate', () => {
    // More active than capacity: every running gate stays visible — a
    // slot the operator cannot see is worse than one past the bound.
    const slots = gateSlots({ capacity: 2, active: [gate('a'), gate('b'), gate('c')] });
    expect(slots).toHaveLength(3);
    expect(slots.every((s) => s.kind === 'occupied')).toBe(true);
  });

  test('zero capacity with no gates is no slots', () => {
    expect(gateSlots({ capacity: 0, active: [] })).toEqual([]);
  });
});
