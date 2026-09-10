import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  blockLabel,
  boardHold,
  boardsWhen,
  clockText,
  conductorReading,
  elapsedText,
  gateSlots,
  journeyText,
  lastVerbReading,
  parseYardStatus,
  phaseLabel,
  queueLabel,
  etaDetail,
  etaReading,
  trainTone,
  type BoardingPredicate,
  type ConductorHealth,
  type TrainEta,
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
          boarded_at: '2026-09-03T06:30:00Z',
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
      held: [
        {
          branch: 'fix/the-pod-installs-claude-with-a-retry',
          reason: 'rolls the dev pod — lands at a David-timed restart',
          since: '2026-09-08T18:00:00Z',
        },
      ],
      gates: {
        capacity: 4,
        active: [{ branch: 'feat/gating', packet_id: 'p1', since: '2026-09-03' }], queued: [], typical_seconds: null },
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
    expect(s.trains[0]!.boarded_at).toBe('2026-09-03T06:30:00Z');
    expect(s.boarding.dock_threshold).toBe(4);
    expect(s.boarding.at_times).toEqual(['06:00', '18:00']);
    expect(s.dock).toHaveLength(1);
    expect(s.recent[0]!.outcome).toBe('arrived');
    expect(s.stranded[0]!.branch).toBe('feat/stranded');
    expect(s.held).toEqual([
      {
        branch: 'fix/the-pod-installs-claude-with-a-retry',
        reason: 'rolls the dev pod — lands at a David-timed restart',
        since: '2026-09-08T18:00:00Z',
        // This payload predates the packet fields the approach lane
        // draws with: '' opens nothing, null draws no head.
        packet_id: '',
        sha: null,
      },
    ]);
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
    expect(s.held).toEqual([]);
  });

  // HELD is the other half of stranded (69daaba2): a green the operator
  // gated and deliberately kept off the dock must not read as a green
  // someone forgot. The list is separate from `stranded`, and a hold
  // recorded without a reason still says so rather than reading blank.
  test('a held green is its own list, never a stranded one, and a blank reason is named as such', () => {
    const s = parseYardStatus({
      stranded: [],
      held: [{ branch: 'fix/held', reason: '', since: '2026-09-08' }],
    });
    expect(s.stranded).toEqual([]);
    expect(s.held).toEqual([
      { branch: 'fix/held', reason: 'no reason recorded', since: '2026-09-08', packet_id: '', sha: null },
    ]);
  });

  // A held CAR is a different answer from a held GREEN: the green has no
  // car yet (release = file one), the car is standing on the dock
  // (release = clear the marker). Two lanes, so a surface never has to
  // guess which one a row is — and a server that predates the lane sends
  // nothing, which reads as nothing held, never as a missing reading.
  test('held cars are their own lane, shaped like dock rows, and absent means empty', () => {
    const s = parseYardStatus({
      dock: [{ id: 'c1', title: 'A free car', branch: 'fix/free', parked_since: '2026-09-09' }],
      held: [{ branch: 'fix/green', reason: 'lands at the restart', since: '2026-09-08' }],
      held_cars: [
        { id: 'c2', title: 'A held car', branch: 'fix/held', parked_since: '2026-09-08', reason: '' },
      ],
    });
    expect(s.dock.map(d => d.branch)).toEqual(['fix/free']);
    expect(s.held.map(h => h.branch)).toEqual(['fix/green']);
    expect(s.held_cars).toEqual([
      {
        id: 'c2',
        title: 'A held car',
        branch: 'fix/held',
        parked_since: '2026-09-08',
        // A hold written with no text still says so rather than reading
        // blank — the same words the held-green lane uses.
        reason: 'no reason recorded',
      },
    ]);
    expect(parseYardStatus({}).held_cars).toEqual([]);
  });

  // Every lane row names the gate-run packet behind it and the head it
  // gated, because the approach lane DRAWS these rows — a wagon with a
  // packet to open and a head to label. Recovering them client-side from
  // a window of gate-runs is what grew a second, weaker copy of "is this
  // green spent?" and drew a phantom wagon for a re-railed branch all
  // day on 2026-09-10 (CLAUDE.md §9a).
  test('each lane row carries its packet and head; a server that sends neither degrades', () => {
    const s = parseYardStatus({
      stranded: [{ branch: 'feat/x', packet_id: 'f802558d', sha: 'abc123', since: '2026-09-09T18:00:00Z' }],
      garage: [{ branch: 'fix/r', failed_check: 'test', since: '2026-09-09', packet_id: 'g-r', sha: 'def456' }],
      limbo: [{ branch: 'fix/l', verdict: 'lost', since: '2026-09-09', packet_id: 'g-l', sha: '' }],
    });
    expect(s.stranded).toEqual([
      { branch: 'feat/x', packet_id: 'f802558d', sha: 'abc123', since: '2026-09-09T18:00:00Z' },
    ]);
    expect(s.garage[0]!.packet_id).toBe('g-r');
    expect(s.garage[0]!.sha).toBe('def456');
    // A blank sha is no sha — never a head drawn from an empty string.
    expect(s.limbo[0]).toEqual({
      branch: 'fix/l', verdict: 'lost', since: '2026-09-09', packet_id: 'g-l', sha: null,
    });
    // An older server sends no limbo lane at all: an empty lane, never a
    // throw, and never a fabricated row.
    expect(parseYardStatus({ stranded: [] }).limbo).toEqual([]);
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
        last_verb: 'reconcile',
        last_rc: 3,
      },
    });
    expect(s.conductor).toEqual({
      last_seen: '2026-09-07T17:50:00Z',
      silent_for_minutes: 47,
      expected_every_minutes: 10,
      silent: true,
      last_verb: 'reconcile',
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
      held_because: null,
      cooldown_remaining_minutes: null,
      last_board_at: null,
      next_board: null,
    });
  });

  test('the boarding hold parses when the server sends it', () => {
    const b = parseYardStatus({
      boarding: {
        dock_threshold: 4,
        dock_depth: 5,
        at_times: [],
        summary: 'x',
        held_because: 'cooldown — 12 min left',
        cooldown_remaining_minutes: 12,
        last_board_at: '2026-09-07T20:41:00Z',
        next_board: 'boards on the next tick once the cooldown clears (12 min)',
      },
    }).boarding;
    expect(b.held_because).toBe('cooldown — 12 min left');
    expect(b.cooldown_remaining_minutes).toBe(12);
    expect(b.last_board_at).toBe('2026-09-07T20:41:00Z');
    expect(b.next_board).toBe('boards on the next tick once the cooldown clears (12 min)');
  });

  test('an absent hold is null in every field — never a hold that is not there', () => {
    // An older server sends no hold; a clear dock sends held_because
    // null WITH a next_board. The two must stay distinguishable.
    const older = parseYardStatus({ boarding: { dock_depth: 5, at_times: [], summary: 'x' } })
      .boarding;
    expect(older.held_because).toBeNull();
    expect(older.cooldown_remaining_minutes).toBeNull();
    expect(older.last_board_at).toBeNull();
    expect(older.next_board).toBeNull();
    const clear = parseYardStatus({
      boarding: {
        dock_depth: 5,
        at_times: [],
        summary: 'x',
        held_because: null,
        next_board: 'boards on the next tick',
      },
    }).boarding;
    expect(clear.held_because).toBeNull();
    expect(clear.next_board).toBe('boards on the next tick');
  });
});

// The CONDUCTOR block's readings — pure, so "what does a silent
// conductor look like" is a test, not a screenshot.
const health = (over: Partial<ConductorHealth> = {}): ConductorHealth => ({
  last_seen: '2026-09-07T17:50:00Z',
  silent_for_minutes: 3,
  expected_every_minutes: 10,
  silent: false,
  last_verb: 'reconcile',
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

describe('lastVerbReading', () => {
  // `last_verb` is the heartbeat rule's VERB (reconcile), read from its
  // registry row — it used to carry the rule's name under a "rule" label.
  test('a clean last pass reads ok with its verb and rc', () => {
    expect(lastVerbReading(health())).toEqual({ tone: 'ok', text: 'reconcile · rc 0' });
  });

  test('a failing last pass is an error the rc names', () => {
    expect(lastVerbReading(health({ last_rc: 3 }))).toEqual({
      tone: 'err',
      text: 'reconcile · rc 3 — the last pass failed',
    });
  });

  test('a verb with no recorded rc reads muted, rc unknown', () => {
    expect(lastVerbReading(health({ last_rc: null }))).toEqual({
      tone: 'muted',
      text: 'reconcile · rc unknown',
    });
  });

  test('no verb on record, and no block at all, both read muted', () => {
    expect(lastVerbReading(health({ last_verb: null })).text).toBe('no verb on record');
    expect(lastVerbReading(null).text).toBe('no verb on record');
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
    held_because: null,
    cooldown_remaining_minutes: null,
    last_board_at: null,
    next_board: null,
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

// The "boards" row: why the dock is not boarding, from the server's
// hold. Twice on 2026-09-07 the operator watched a full dock not board
// and asked why; the answer was in the conductor's journal.
describe('boardHold', () => {
  const predicate = (over: Partial<BoardingPredicate> = {}): BoardingPredicate => ({
    dock_threshold: 4,
    cooldown_minutes: 45,
    at_times: [],
    dock_depth: 5,
    threshold_met: true,
    summary: 'x',
    held_because: null,
    cooldown_remaining_minutes: null,
    last_board_at: null,
    next_board: 'boards on the next tick',
    ...over,
  });

  test('a hold is the primary line, the next sentence beneath, the last board as a clock time', () => {
    const v = boardHold(
      predicate({
        held_because: 'cooldown — 12 min left',
        cooldown_remaining_minutes: 12,
        last_board_at: '2026-09-07T20:41:00Z',
        next_board: 'boards on the next tick once the cooldown clears (12 min)',
      }),
    );
    expect(v).toEqual({
      primary: { tone: null, text: 'held: cooldown — 12 min left' },
      next: 'boards on the next tick once the cooldown clears (12 min)',
      lastBoard: '20:41 UTC',
    });
  });

  test("a clear dock boards on the next tick — the server's sentence, in ok, never a time", () => {
    const v = boardHold(predicate());
    expect(v).toEqual({
      primary: { tone: 'ok', text: 'boards on the next tick' },
      next: null,
      lastBoard: null,
    });
    expect(v!.primary.text).not.toMatch(/\d\d:\d\d|next at|in \d+m/);
  });

  test('no depth rule reads muted — nothing boards on depth, so nothing is promised', () => {
    const v = boardHold(
      predicate({
        dock_threshold: null,
        threshold_met: null,
        next_board: 'no depth rule is configured — nothing boards on dock depth',
      }),
    );
    expect(v!.primary).toEqual({
      tone: 'muted',
      text: 'no depth rule is configured — nothing boards on dock depth',
    });
  });

  test('an older server that sends no hold gets none — the page states the rule instead', () => {
    expect(boardHold(predicate({ held_because: null, next_board: null }))).toBeNull();
  });
});

describe('clockText', () => {
  test('a stamp reads as the UTC clock time it names, in the at_times idiom', () => {
    expect(clockText('2026-09-07T20:41:00Z')).toBe('20:41 UTC');
    expect(clockText('2026-09-07T20:41:00+00:00')).toBe('20:41 UTC');
    expect(clockText('2026-09-07T03:05:09.123Z')).toBe('03:05 UTC');
  });
  test('absent or unparseable is null — never a fabricated time', () => {
    expect(clockText(null)).toBeNull();
    expect(clockText('')).toBeNull();
    expect(clockText('not a stamp')).toBeNull();
  });
});

// The template renders the hold through `boardHold` and nothing else —
// no time of day of its own. Pinned on the source, in the
// yard-page-order idiom, so the W1 hook cannot quietly reopen.
describe('the conductor block renders the server hold', () => {
  const src = readFileSync(join(import.meta.dir, 'YardPage.svelte'), 'utf8');

  test('the boards row is the hold — primary line, next beneath, last board as a clock time', () => {
    expect(src).toContain('boardHold(status.data.boarding)');
    expect(src).toContain('{hold.primary.text}');
    expect(src).toContain('{hold.next}');
    expect(src).toContain('last board {hold.lastBoard}');
  });

  test('the hook that waited on the Rust change is closed', () => {
    expect(src).not.toContain('HOOK (needs a Rust change');
  });

  test('the last-verb row is labelled by the fact it shows', () => {
    expect(src).toContain('lastVerbReading(conductor)');
    expect(src).not.toContain('lastRuleReading');
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
    boarded_at: null,
    eta: { kind: 'unknown', reason: 'not under test' },
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
  const gate = (branch: string) => ({ branch, packet_id: `p-${branch}`, since: '2026-09-03', stale: false });

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

describe('an active gate carries the server\'s stale flag', () => {
  test('stale is read as sent; absent reads false, never a fabricated alarm', () => {
    const s = parseYardStatus({
      gates: { capacity: 3, active: [
        { branch: 'feat/a', packet_id: 'p1', since: '2026-09-07', stale: true },
        { branch: 'feat/b', packet_id: 'p2', since: '2026-09-07' },
      ], queued: [], typical_seconds: null },
    });
    expect(s.gates.active.map(g => g.stale)).toEqual([true, false]);
  });
});

describe('the gate QUEUE — runs waiting for a slot', () => {
  // A queued run is correctly kept out of the bays, and until this
  // landed nothing else showed it: three busy bays with two waiting read
  // exactly like three busy bays, so an operator could not tell a queued
  // gate from one that never launched.
  test('the queue is parsed in the order the server set, with its estimate', () => {
    const s = parseYardStatus({
      gates: {
        capacity: 3,
        active: [{ branch: 'feat/gating', packet_id: 'p1', since: '2026-09-07T23:00:00Z' }],
        typical_seconds: 900,
        queued: [
          {
            branch: 'feat/first',
            packet_id: 'q1',
            queued_at: '2026-09-07T23:05:00Z',
            position: 1,
            waiting_seconds: 600,
            estimated_wait_seconds: 300,
          },
          {
            branch: 'feat/second',
            packet_id: 'q2',
            queued_at: '2026-09-07T23:07:00Z',
            position: 2,
            waiting_seconds: 480,
            estimated_wait_seconds: 1200,
          },
        ],
      },
    });
    expect(s.gates.queued.map(q => q.branch)).toEqual(['feat/first', 'feat/second']);
    expect(s.gates.queued.map(q => q.position)).toEqual([1, 2]);
    expect(s.gates.queued[0]!.waiting_seconds).toBe(600);
    expect(s.gates.queued[0]!.estimated_wait_seconds).toBe(300);
    expect(s.gates.typical_seconds).toBe(900);
  });

  test('an older server sends no queue — empty lane, no measurement, never a fabricated wait', () => {
    const s = parseYardStatus({ gates: { capacity: 3, active: [], queued: [], typical_seconds: null } });
    expect(s.gates.queued).toEqual([]);
    expect(s.gates.typical_seconds).toBeNull();
  });

  test('a wait the server could not derive stays unknown', () => {
    const s = parseYardStatus({
      gates: {
        capacity: 3,
        active: [],
        queued: [{ branch: 'feat/x', packet_id: 'q1', queued_at: '2026-09-07T23:05:00Z', position: 1 }],
      },
    });
    expect(s.gates.queued[0]!.waiting_seconds).toBeNull();
    expect(s.gates.queued[0]!.estimated_wait_seconds).toBeNull();
  });

  test('the lane line states the place, the wait and the measured estimate', () => {
    const q = {
      branch: 'feat/x',
      packet_id: 'q1',
      queued_at: '2026-09-07T23:05:00Z',
      position: 2,
      waiting_seconds: 720,
      estimated_wait_seconds: 1080,
    } as const;
    expect(queueLabel(q)).toBe('#2 in line · waiting 12m · est. ~18m');
    expect(queueLabel({ ...q, estimated_wait_seconds: 0 })).toBe('#2 in line · waiting 12m · a slot is free now');
    expect(queueLabel({ ...q, estimated_wait_seconds: null })).toBe('#2 in line · waiting 12m');
    expect(queueLabel({ ...q, waiting_seconds: null, estimated_wait_seconds: null })).toBe('#2 in line');
  });
});

// The ETA on a train in flight. The gates panel beside it has carried a
// measured `typical_seconds` for months; trains never got the
// equivalent, and the cost landed on the operator, who had to ask a
// human whether a 20-minute transit was normal — twice.
//
// MEASURED 2026-09-10 against the live record (1,014 pr-trains): 696
// cancelled, 254 arrived, 143 with a readable board→merge leg, 105 with
// a readable merge→arrival leg. board→merge p10/median/p90 = 890 / 1206
// / 1709s; merge→arrival = 589 / 1183 / 4799s.
describe('the ETA on a train in flight', () => {
  const estimate = (over: Partial<Extract<TrainEta, { kind: 'estimate' }>> = {}) =>
    ({
      kind: 'estimate',
      leg: 'boarding → arrival',
      remaining_seconds: 2100,
      remaining_low_seconds: 1200,
      remaining_high_seconds: 4500,
      sample_size: 105,
      basis: 'median of recent arrivals — boarding→merge from 143, merge→arrival from 105',
      overdue: false,
      ...over,
    }) as TrainEta;

  test('parses the estimate off the train row', () => {
    const s = parseYardStatus({
      trains: [
        {
          id: 't1',
          title: 'train #200',
          phase: 'awaiting-ci',
          eta: {
            kind: 'estimate',
            leg: 'boarding → arrival',
            remaining_seconds: 2100,
            remaining_low_seconds: 1200,
            remaining_high_seconds: 4500,
            sample_size: 105,
            basis: 'median of recent arrivals',
            overdue: false,
          },
        },
      ],
    });
    const eta = s.trains[0]!.eta;
    expect(eta.kind).toBe('estimate');
    if (eta.kind !== 'estimate') throw new Error('unreachable');
    expect(eta.remaining_seconds).toBe(2100);
    expect(eta.leg).toBe('boarding → arrival');
    expect(eta.sample_size).toBe(105);
  });

  // A server that does not send one, and a kind this build does not
  // model, both read as "no estimate" with a reason — never as a
  // fabricated zero, and never by failing the whole page.
  test('an absent or unknown eta reads as no estimate, with a reason', () => {
    const missing = parseYardStatus({ trains: [{ id: 't1', title: 'x', phase: 'boarding' }] });
    const eta = missing.trains[0]!.eta;
    expect(eta.kind).toBe('unknown');
    if (eta.kind !== 'unknown') throw new Error('unreachable');
    expect(eta.reason.length).toBeGreaterThan(0);

    const weird = parseYardStatus({
      trains: [{ id: 't1', title: 'x', phase: 'boarding', eta: { kind: 'from-the-future' } }],
    });
    expect(weird.trains[0]!.eta.kind).toBe('unknown');
  });

  // The SPREAD is published, not flattened. A single number would
  // over-claim: within one car count the measured arrivals ranged 770s
  // to 4,274s, so the label carries the 10th–90th band beside the median.
  test('the reading states the median AND the spread', () => {
    const r = etaReading(estimate());
    expect(r.text).toBe('~35m left (20m–1.3h)');
    expect(r.tone).toBe('ok');
  });

  // "A troubled packet must look troubled" (CLAUDE.md §Diagnosis): past
  // the 90th percentile of everything measured, the chip stops counting
  // down and says so.
  test('a train past the measured spread reads overdue, in an alarm tone', () => {
    const r = etaReading(estimate({ overdue: true, remaining_seconds: 0 }));
    expect(r.text).toContain('overdue');
    expect(r.tone).toBe('err');
    expect(r.text).not.toContain('~0m left');
  });

  // No estimate is a stated refusal, never a blank or a zero.
  test('no estimate says so, and the reason is readable', () => {
    const r = etaReading({
      kind: 'unknown',
      reason: 'too little history to measure — 3 arrived train(s) in the window, 3 with a readable leg, 10 needed',
    });
    expect(r.text).toBe('no ETA');
    expect(r.tone).toBe('muted');
    expect(etaDetail({ kind: 'unknown', reason: 'too little history — 3 of 10' })).toBe(
      'too little history — 3 of 10',
    );
  });

  // The LEG is on the tooltip, because board→arrival and merge→arrival
  // are materially different lengths (a median of 2,389s against 1,183s)
  // and a reader who has to guess which one has no estimate.
  test('the detail names the leg and the basis', () => {
    const d = etaDetail(estimate());
    expect(d).toContain('boarding → arrival');
    expect(d).toContain('median of recent arrivals');
    expect(d).toContain('105');
  });

  test('the detail of a merged train names the merge leg', () => {
    const d = etaDetail(estimate({ leg: 'merge → arrival', basis: 'median of 105 legs' }));
    expect(d).toContain('merge → arrival');
  });
});
