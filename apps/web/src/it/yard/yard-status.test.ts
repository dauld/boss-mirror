import { describe, expect, test } from 'bun:test';
import {
  blockLabel,
  boardsWhen,
  conductorReading,
  elapsedText,
  gateSlots,
  journeyText,
  lastRuleReading,
  parseYardStatus,
  phaseLabel,
  trainTone,
  type BoardingPredicate,
  type ConductorHealth,
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

  test('deserializes the conductor block — liveness, last rule and its exit code', () => {
    // The server has sent this since the-board-does-not-lie; the page
    // dropped it on the floor, so a dead conductor drew a healthy yard.
    const s = parseYardStatus({
      conductor: {
        last_seen: '2026-09-07T17:50:00Z',
        silent_for_minutes: 47,
        expected_every_minutes: 10,
        silent: true,
        last_verb: 'train-reconcile',
        last_rc: 3,
      },
    });
    expect(s.conductor).toEqual({
      last_seen: '2026-09-07T17:50:00Z',
      silent_for_minutes: 47,
      expected_every_minutes: 10,
      silent: true,
      last_verb: 'train-reconcile',
      last_rc: 3,
    });
  });

  test('a payload without a conductor block parses with conductor null — an older server', () => {
    const s = parseYardStatus({ boarding: { dock_depth: 0, at_times: [], summary: 'x' } });
    expect(s.conductor).toBeNull();
    expect(parseYardStatus({ conductor: null }).conductor).toBeNull();
  });

  test('a conductor block with unknowns keeps them null — never a fabricated reading', () => {
    // No firing, no clock: the server sends nulls and silent:false
    // ("I cannot tell" must not be dressed up as health or alarm).
    const s = parseYardStatus({ conductor: { silent: false } });
    expect(s.conductor).toEqual({
      last_seen: null,
      silent_for_minutes: null,
      expected_every_minutes: null,
      silent: false,
      last_verb: null,
      last_rc: null,
    });
  });

  test('the boarding predicate carries every field the conductor block renders', () => {
    const s = parseYardStatus({
      boarding: {
        dock_threshold: 4,
        cooldown_minutes: 120,
        at_times: ['06:00'],
        dock_depth: 5,
        threshold_met: true,
        summary: 'Boards at 4 parked cars; 5 car(s) parked now.',
      },
    });
    expect(s.boarding).toEqual({
      dock_threshold: 4,
      cooldown_minutes: 120,
      at_times: ['06:00'],
      dock_depth: 5,
      threshold_met: true,
      summary: 'Boards at 4 parked cars; 5 car(s) parked now.',
    });
  });
});

// The CONDUCTOR block's readings — pure, so "what does a silent
// conductor look like" is a test, not a screenshot.
const health = (over: Partial<ConductorHealth> = {}): ConductorHealth => ({
  last_seen: '2026-09-07T17:50:00Z',
  silent_for_minutes: 3,
  expected_every_minutes: 10,
  silent: false,
  last_verb: 'train-reconcile',
  last_rc: 0,
  ...over,
});

describe('conductorReading', () => {
  test('an alive conductor reads last-seen against its own declared heartbeat', () => {
    expect(conductorReading(health())).toEqual({
      tone: 'ok',
      text: 'last seen 3m ago · expects every 10m',
    });
  });

  test('a silent conductor is an ERROR reading that names the silence', () => {
    const r = conductorReading(health({ silent: true, silent_for_minutes: 47 }));
    expect(r.tone).toBe('err');
    expect(r.text).toBe('SILENT — 47m since it last fired · expects every 10m');
  });

  test('no firing on record is unknown, spelled as unknown — not health, not alarm', () => {
    const r = conductorReading(
      health({ last_seen: null, silent_for_minutes: null, last_verb: null, last_rc: null }),
    );
    expect(r.tone).toBe('warn');
    expect(r.text).toBe('no firing on record — liveness unknown');
  });

  test('an older server with no conductor block says so, muted', () => {
    const r = conductorReading(null);
    expect(r.tone).toBe('muted');
    expect(r.text).toBe('no liveness reading — this server does not report the conductor');
  });

  test('a heartbeat with no declared interval still reads last-seen', () => {
    expect(conductorReading(health({ expected_every_minutes: null })).text).toBe(
      'last seen 3m ago',
    );
  });
});

describe('lastRuleReading', () => {
  // `last_verb` carries the RULE NAME today (train-reconcile), not a
  // verb — the label says "rule" so the words stay anchored to the fact.
  test('a clean last pass reads ok with its rule and rc', () => {
    expect(lastRuleReading(health())).toEqual({ tone: 'ok', text: 'train-reconcile · rc 0' });
  });

  test('a failing last pass is an error the rc names', () => {
    expect(lastRuleReading(health({ last_rc: 3 }))).toEqual({
      tone: 'err',
      text: 'train-reconcile · rc 3 — the last pass failed',
    });
  });

  test('a rule with no recorded rc reads muted, rc unknown', () => {
    expect(lastRuleReading(health({ last_rc: null }))).toEqual({
      tone: 'muted',
      text: 'train-reconcile · rc unknown',
    });
  });

  test('no rule on record, and no block at all, both read muted', () => {
    expect(lastRuleReading(health({ last_verb: null })).text).toBe('no rule on record');
    expect(lastRuleReading(null).text).toBe('no rule on record');
  });
});

describe('boardsWhen', () => {
  const predicate = (over: Partial<BoardingPredicate> = {}): BoardingPredicate => ({
    dock_threshold: 4,
    cooldown_minutes: 120,
    at_times: [],
    dock_depth: 2,
    threshold_met: false,
    summary: 'Boards at 4 parked cars (then a 120m cooldown); 2 car(s) parked now.',
    ...over,
  });

  test('below threshold: states the RULE — depth and cooldown — never a time', () => {
    const text = boardsWhen(predicate());
    expect(text).toBe('2/4 parked — boards when the dock reaches 4 and the cooldown (120m) clears');
    // The board rule is depth-triggered; there is no next-fire clock to
    // show, and inventing one is the defect this page exists to avoid.
    expect(text).not.toMatch(/\d\d:\d\d|next at|in \d+m/);
  });

  test('threshold met: says so, and that the cooldown is what it waits on', () => {
    expect(boardsWhen(predicate({ dock_depth: 5, threshold_met: true }))).toBe(
      'threshold met — 5/4 parked; boards when the cooldown (120m) clears',
    );
  });

  test('no cooldown configured: no cooldown clause', () => {
    expect(boardsWhen(predicate({ cooldown_minutes: null }))).toBe(
      '2/4 parked — boards when the dock reaches 4',
    );
    expect(boardsWhen(predicate({ cooldown_minutes: null, dock_depth: 4, threshold_met: true }))).toBe(
      "threshold met — 4/4 parked; boards on the conductor's next pass",
    );
  });

  test('a clock rule beside the depth rule is quoted verbatim from the registry', () => {
    expect(boardsWhen(predicate({ at_times: ['06:00', '18:00'] }))).toBe(
      '2/4 parked — boards when the dock reaches 4 and the cooldown (120m) clears · or by the clock at 06:00 / 18:00 UTC',
    );
  });

  test('no depth rule: the server summary stands, or says nothing is configured', () => {
    expect(boardsWhen(predicate({ dock_threshold: null, threshold_met: null, summary: 'Boards at 06:00 UTC.' })))
      .toBe('Boards at 06:00 UTC.');
    expect(boardsWhen(predicate({ dock_threshold: null, threshold_met: null, summary: '' })))
      .toBe('no boarding rule configured');
  });
});

describe('elapsedText', () => {
  const now = Date.parse('2026-09-07T18:00:00Z');
  test('minutes under an hour, hours above — the journeyText idiom', () => {
    expect(elapsedText('2026-09-07T17:48:00Z', now)).toBe('12m');
    expect(elapsedText('2026-09-07T15:30:00Z', now)).toBe('2.5h');
  });
  test('an absent or unparseable stamp is null — never a fabricated zero', () => {
    expect(elapsedText(null, now)).toBeNull();
    expect(elapsedText(undefined, now)).toBeNull();
    expect(elapsedText('not a stamp', now)).toBeNull();
    expect(elapsedText('', now)).toBeNull();
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
