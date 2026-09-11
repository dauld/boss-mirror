// The Crew Board's read model, under test.
//
// Every fixture below is a TRIMMED COPY OF A REAL PAYLOAD measured
// against the system of record (10.20.0.34:7900) on 2026-09-11, not an
// invention: the `automation:*` / `claude@algedonic.dev` / `emp-david`
// actor ids, the `abandoned` + branchless `ship-a-change` residue, the
// `ActiveGate` / `QueuedGate` / dock shapes from `/api/yard/status`, and
// the `/api/jobs/queue-age` row shape with its `exact` floor flag.
//
// The residue case is the one that earned its own describe block. On
// 2026-09-10 three `ship-a-change` packets existed with NO branch,
// sitting at `scope`, which were abandoned cadence sweeps rather than
// live builds (21edde87). "Opened but not yet gated" would have drawn
// all three as actors mid-build. The predicate this file pins is the
// answer to that.

import { describe, it, expect } from 'bun:test';
import { readFileSync } from 'node:fs';
import { isHumanActor } from '../../data/actor';
import {
  actorLane,
  actorCards,
  parseCars,
  parseGateRuns,
  parseYard,
  parseWaits,
  isBuilding,
  gatedBranches,
  pipelineTrack,
  takenNotProgressed,
  type Car,
} from './crew';

// ---------------------------------------------------------------------
// Fixtures — trimmed real payloads
// ---------------------------------------------------------------------

/// `GET /api/jobs?kind=ship-a-change` — four real shapes: a car parked
/// on the dock, a car that boarded and merged, a car that has a branch
/// and no gate-run anywhere (the only true BUILDING case), and the
/// branchless abandoned sweep that is not a car at all.
const CARS_RAW = {
  data: [
    {
      id: 'd1e1af0f-7c62-47de-9739-1d5eeb7108ab',
      kind: 'ship-a-change',
      status: 'open',
      title: 'An abandoned place in the gate queue is reported as troubled',
      owner_id: 'emp-david',
      simulated: false,
      metadata: {
        branch: 'fix/a-queued-gate-does-not-need-its-waiter',
        opened_at: '2026-09-11T03:53:12.679168361+00:00',
      },
      steps: [
        {
          kind: 'task',
          status: 'ready',
          assignee_id: 'claude@algedonic.dev',
          completed_by: null,
          completed_at: null,
          completed_on: null,
        },
        {
          kind: 'trigger',
          status: 'completed',
          assignee_id: null,
          completed_by: 'automation:rule:auto-park-on-gate-green',
          completed_at: '2026-09-11T03:53:12.000000Z',
          completed_on: '2026-09-11',
        },
      ],
    },
    {
      id: '2f0c0000-0000-0000-0000-000000000002',
      kind: 'ship-a-change',
      status: 'open',
      title: 'A dispatcher rule has one home: the authored directory',
      owner_id: 'emp-david',
      simulated: false,
      metadata: {
        branch: 'feat/a-dispatcher-rule-has-one-home',
        opened_at: '2026-09-11T03:32:48.000000+00:00',
        train: '6686d3a0-751f-4291-8371-b46b3ad74f10',
        merged: true,
      },
      steps: [
        {
          kind: 'task',
          status: 'completed',
          assignee_id: null,
          completed_by: 'automation:train-conductor',
          completed_at: '2026-09-11T03:50:00.000000Z',
          completed_on: '2026-09-11',
        },
      ],
    },
    {
      id: '3f0c0000-0000-0000-0000-000000000003',
      kind: 'ship-a-change',
      status: 'open',
      title: 'The converge feed gets a live tail',
      owner_id: 'emp-david',
      simulated: false,
      metadata: {
        branch: 'feat/the-converge-feed',
        opened_at: '2026-09-11T04:00:00.000000+00:00',
      },
      steps: [
        {
          kind: 'task',
          status: 'active',
          assignee_id: 'claude@algedonic.dev',
          completed_by: null,
          completed_at: null,
          completed_on: null,
        },
      ],
    },
    {
      // The residue: a cadence sweep, no branch, abandoned.
      id: '4f0c0000-0000-0000-0000-000000000004',
      kind: 'ship-a-change',
      status: 'closed',
      title: 'Cluster conformance sweep',
      owner_id: 'emp-david',
      simulated: false,
      metadata: {
        abandoned: true,
        abandon_reason: 'no change to make',
        spawned_by_rule: 'cluster-conformance-sweep',
      },
      steps: [
        {
          kind: 'scope',
          status: 'completed',
          assignee_id: null,
          completed_by: 'claude@algedonic.dev',
          completed_at: '2026-09-11T02:11:27.000000Z',
          completed_on: '2026-09-11',
        },
      ],
    },
  ],
};

/// `GET /api/jobs?kind=gate-run` — the gate registry. Two branches have
/// been gated; `feat/the-converge-feed` has not.
const GATE_RUNS_RAW = {
  data: [
    {
      id: '7cb68476-27f1-4949-a805-c339229d1bef',
      kind: 'gate-run',
      status: 'closed',
      title: 'Gate: fix/a-queued-gate-does-not-need-its-waiter',
      owner_id: 'emp-david',
      simulated: false,
      metadata: {
        branch: 'fix/a-queued-gate-does-not-need-its-waiter',
        outcome: 'passed',
        opened_at: '2026-09-11T03:35:47.446145639+00:00',
      },
      steps: [
        {
          kind: 'gate-verdict',
          status: 'completed',
          assignee_id: null,
          completed_by: 'automation:gate-runner',
          completed_at: '2026-09-11T03:48:59.720283Z',
          completed_on: '2026-09-11',
        },
      ],
    },
    {
      id: '8cb68476-27f1-4949-a805-c339229d1bec',
      kind: 'gate-run',
      status: 'closed',
      title: 'Gate: feat/a-dispatcher-rule-has-one-home',
      owner_id: 'emp-david',
      simulated: false,
      metadata: {
        branch: 'feat/a-dispatcher-rule-has-one-home',
        outcome: 'failed',
        opened_at: '2026-09-11T03:32:11.564113680+00:00',
      },
      steps: [
        {
          kind: 'gate-verdict',
          status: 'completed',
          assignee_id: null,
          completed_by: 'automation:gate-runner',
          completed_at: '2026-09-11T03:47:48.156508Z',
          completed_on: '2026-09-10',
        },
      ],
    },
  ],
};

/// `GET /api/yard/status` — trimmed to what the board reads.
const YARD_RAW = {
  now: '2026-09-11T04:05:26.763304939Z',
  gates: {
    capacity: 3,
    typical_seconds: 1033,
    active: [
      {
        branch: 'feat/the-yard-is-a-floor',
        packet_id: 'aa000000-0000-0000-0000-0000000000aa',
        since: '2026-09-11T03:55:00Z',
        stale: false,
      },
    ],
    queued: [
      {
        branch: 'fix/a-waiting-gate',
        packet_id: 'bb000000-0000-0000-0000-0000000000bb',
        queued_at: '2026-09-11T04:00:00Z',
        position: 1,
        waiting_seconds: 326,
        estimated_wait_seconds: 700,
      },
    ],
  },
  dock: [
    {
      id: 'd1e1af0f-7c62-47de-9739-1d5eeb7108ab',
      branch: 'fix/a-queued-gate-does-not-need-its-waiter',
      parked_since: '2026-09-11',
      title: 'An abandoned place in the gate queue is reported as troubled',
    },
  ],
  garage: [
    {
      branch: 'fix/the-boarding-block-says-depth-unknown',
      failed_check: 'test',
      packet_id: '7cb68476-27f1-4949-a805-c339229d1bef',
      sha: '366ab637a7080e94b895534ff9dfd50f6d5f946e',
      since: '2026-09-11T03:35:47.446145639+00:00',
    },
  ],
};

/// `GET /api/jobs/queue-age` — real row shape, including the `exact`
/// floor flag and an UNASSIGNED row that must never be drawn as taken.
const WAITS_RAW = {
  now: '2026-09-11T04:05:26Z',
  data: [
    {
      assignee_id: 'claude@algedonic.dev',
      exact: false,
      job_id: '508cc38c-360c-4e02-8847-7438b5d4ea04',
      job_kind: 'user-feedback',
      job_title: 'Experiments Tier 3: the shadow partition',
      simulated: false,
      since: '2026-08-22T18:51:49.962273Z',
      spec_slug: 'build',
      status: 'ready',
      step_id: '6cc308e3-2079-44aa-a1fe-3c80b4c1cf23',
      step_title: 'Build the change',
      waiting_days: 19.38722222222222,
      waiting_seconds: 1675056,
    },
    {
      // Unassigned: waiting, but nobody took it. Not "taken".
      assignee_id: null,
      exact: false,
      job_id: '8043c1f5-c6f2-45fa-9f09-55b05d325c2d',
      job_kind: 'protocol-retro',
      job_title: 'protocol-retro — 2026-08-31',
      simulated: false,
      since: '2026-08-31T06:10:42.691295Z',
      spec_slug: 'collect',
      status: 'ready',
      step_id: 'f2956312-d1ed-45a4-8cf0-fbd512cec64d',
      step_title: 'Collect train and cadence timings',
      waiting_days: 10.915775462962962,
      waiting_seconds: 943123,
    },
    {
      assignee_id: 'emp-david',
      exact: true,
      job_id: 'ac356440-2aab-4d0d-af09-93a6f6488ba6',
      job_kind: 'backlog-item',
      job_title: 'A decision waiting on David',
      simulated: false,
      since: '2026-09-11T03:50:00Z',
      spec_slug: 'decide',
      status: 'ready',
      step_id: 'cc000000-0000-0000-0000-0000000000cc',
      step_title: 'Decide',
      waiting_days: 0.0104,
      waiting_seconds: 900,
    },
    {
      // Simulated: the brewery's event clock runs ~1000x the wall, so
      // a simulated wait measured on a real clock is meaningless.
      assignee_id: 'emp-sim-brewer',
      exact: true,
      job_id: 'dd000000-0000-0000-0000-0000000000dd',
      job_kind: 'brew-batch',
      job_title: 'Brew batch 4711',
      simulated: true,
      since: '2026-08-01T00:00:00Z',
      spec_slug: 'mash',
      status: 'ready',
      step_id: 'ee000000-0000-0000-0000-0000000000ee',
      step_title: 'Mash in',
      waiting_days: 41,
      waiting_seconds: 3542400,
    },
  ],
};

// ---------------------------------------------------------------------
// Actor identity
// ---------------------------------------------------------------------

describe('actorLane — the four lanes the board draws', () => {
  it('does not re-derive the human/machine split — it delegates to isHumanActor', () => {
    // CLAUDE.md §9a. `data/actor.ts` says so in its own words: "This is
    // the one definition of that rule on the client — callers that need
    // 'is this the machine?' negate it rather than re-deriving it."
    // So every machine id must land in a machine lane and no other.
    for (const id of [
      'automation:gate-runner',
      'automation:rule:complete-marker-on-step-ready',
      'claude:opus-5[1m]',
    ]) {
      expect(isHumanActor(id)).toBe(false);
      expect(actorLane(id, false)).not.toBe('human');
    }
    expect(isHumanActor('emp-david')).toBe(true);
    expect(actorLane('emp-david', false)).toBe('human');
  });

  it('reads the automation: prefix every BOSS verb signs with', () => {
    // Measured on 200 recent packets, 2026-09-11: these are the real ids.
    expect(actorLane('automation:gate-runner', false)).toBe('automation');
    expect(actorLane('automation:train-conductor', false)).toBe('automation');
    expect(actorLane('automation:rule:complete-marker-on-step-ready', false)).toBe('automation');
    expect(actorLane('automation:boss-step', false)).toBe('automation');
    expect(actorLane('automation:ops-runner', false)).toBe('automation');
  });

  it('reads an emp- id as a human', () => {
    expect(actorLane('emp-david', false)).toBe('human');
  });

  it('reads the <mode>:<model> agent form as an agent', () => {
    // The ActorId union's agent spelling (crates/core/boss-core/src/actor.rs),
    // and what `agent_runs.actor_id` carries.
    expect(actorLane('claude:opus-5[1m]', false)).toBe('agent');
  });

  it('reads the address-shaped agent id the live data actually uses', () => {
    // This board found the defect and pinned it rather than papering over
    // it: `claude@algedonic.dev` held 250 step assignments and signed 5
    // completions on the system of record, yet `isHumanActor` called it a
    // human, because it carries no colon. The fix landed where the packet
    // said it belonged — in `data/actor.ts`, the one definition shared by
    // every surface asking this question (backlog a6b10413) — so the
    // delegation above now covers the address form too and this
    // assertion records the fix instead of the bug.
    expect(isHumanActor('claude@algedonic.dev')).toBe(false);
    expect(actorLane('claude@algedonic.dev', false)).toBe('agent');
  });

  it('files an actor working simulated packets in the sim lane', () => {
    // Sim is a property of the PACKET, not of the id: `emp-sim-brewer` is
    // a perfectly ordinary employee id. The brewery's event clock runs
    // ~1000x the wall, so its work cannot share a lane with real crew.
    expect(actorLane('emp-sim-brewer', true)).toBe('sim');
    expect(actorLane('automation:sim', true)).toBe('sim');
  });
});

// ---------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------

describe('parsers take the wire shape as it actually is', () => {
  it('parses cars, keeping the branch and the residue markers', () => {
    const cars = parseCars(CARS_RAW);
    expect(cars).toHaveLength(4);
    expect(cars[0]!.branch).toBe('fix/a-queued-gate-does-not-need-its-waiter');
    // The sweep: no branch, abandoned. Both facts survive parsing,
    // because the predicate needs both.
    const sweep = cars[3]!;
    expect(sweep.branch).toBeNull();
    expect(sweep.abandoned).toBe(true);
  });

  it('collects the branches the gate registry has seen', () => {
    expect(gatedBranches(parseGateRuns(GATE_RUNS_RAW))).toEqual(
      new Set([
        'fix/a-queued-gate-does-not-need-its-waiter',
        'feat/a-dispatcher-rule-has-one-home',
      ]),
    );
  });

  it('parses the yard lanes, including the queued line and its position', () => {
    const y = parseYard(YARD_RAW);
    expect(y.gatesCapacity).toBe(3);
    expect(y.active).toHaveLength(1);
    expect(y.active[0]!.branch).toBe('feat/the-yard-is-a-floor');
    expect(y.active[0]!.stale).toBe(false);
    expect(y.queued[0]!.position).toBe(1);
    expect(y.queued[0]!.waitingSeconds).toBe(326);
    expect(y.dock[0]!.branch).toBe('fix/a-queued-gate-does-not-need-its-waiter');
    expect(y.garage[0]!.failedCheck).toBe('test');
    expect(y.now).toBe('2026-09-11T04:05:26.763304939Z');
  });

  it('keeps the exact flag on a wait, because it marks a floor not a figure', () => {
    const waits = parseWaits(WAITS_RAW);
    expect(waits).toHaveLength(4);
    expect(waits[0]!.exact).toBe(false);
    expect(waits[2]!.exact).toBe(true);
  });
});

// ---------------------------------------------------------------------
// The BUILDING predicate — the one the residue problem forced
// ---------------------------------------------------------------------

describe('isBuilding — a branch with no gate-run behind it', () => {
  const cars = parseCars(CARS_RAW);
  const gated = gatedBranches(parseGateRuns(GATE_RUNS_RAW));
  const byBranch = (b: string | null): Car => cars.find((c) => c.branch === b)!;

  it('is true for an open car with a branch that the gate registry has never seen', () => {
    expect(isBuilding(byBranch('feat/the-converge-feed'), gated)).toBe(true);
  });

  it('is FALSE for a branchless packet, however open it is', () => {
    // The 2026-09-10 residue: three such packets sat at `scope` with no
    // branch. A build produces a branch; no branch is no build.
    expect(isBuilding(cars[3]!, gated)).toBe(false);
  });

  it('is false once the gate registry has seen the branch', () => {
    // It has moved on to GATING/PARKED/BOARDED — drawing it as BUILDING
    // too would double-count one car in two columns.
    expect(isBuilding(byBranch('fix/a-queued-gate-does-not-need-its-waiter'), gated)).toBe(false);
  });

  it('is false for an abandoned packet even if it somehow carries a branch', () => {
    const abandonedWithBranch: Car = {
      ...byBranch('feat/the-converge-feed'),
      abandoned: true,
    };
    expect(isBuilding(abandonedWithBranch, gated)).toBe(false);
  });

  it('is false once the car has boarded or merged', () => {
    const boarded: Car = { ...byBranch('feat/the-converge-feed'), train: 'some-train' };
    expect(isBuilding(boarded, gated)).toBe(false);
    const merged: Car = { ...byBranch('feat/the-converge-feed'), merged: true };
    expect(isBuilding(merged, gated)).toBe(false);
  });

  it('is false for a closed packet', () => {
    const closed: Car = { ...byBranch('feat/the-converge-feed'), open: false };
    expect(isBuilding(closed, gated)).toBe(false);
  });
});

// ---------------------------------------------------------------------
// The pipeline track
// ---------------------------------------------------------------------

describe('pipelineTrack — a gate-run and a dock row become a track position', () => {
  const track = pipelineTrack(
    parseCars(CARS_RAW),
    parseGateRuns(GATE_RUNS_RAW),
    parseYard(YARD_RAW),
  );

  it('puts the yard gate bays in GATING, naming the branch', () => {
    expect(track.gating.map((c) => c.branch)).toEqual([
      'feat/the-yard-is-a-floor',
      'fix/a-waiting-gate',
    ]);
    // The queued one is distinguished from the one holding a bay —
    // three busy bays with two waiting must not read as three busy bays.
    expect(track.gating[0]!.detail).toContain('bay');
    expect(track.gating[1]!.detail).toContain('queued');
  });

  it('puts the dock row in PARKED', () => {
    expect(track.parked.map((c) => c.branch)).toEqual([
      'fix/a-queued-gate-does-not-need-its-waiter',
    ]);
  });

  it('puts a car with a branch and no gate-run in BUILDING', () => {
    expect(track.building.map((c) => c.branch)).toEqual(['feat/the-converge-feed']);
  });

  it('puts a merged car in LANDED, not BOARDED', () => {
    expect(track.landed.map((c) => c.branch)).toEqual(['feat/a-dispatcher-rule-has-one-home']);
    expect(track.boarded).toEqual([]);
  });

  it('draws each car in exactly one column', () => {
    const all = [
      ...track.building,
      ...track.gating,
      ...track.parked,
      ...track.boarded,
      ...track.landed,
    ].map((c) => c.branch);
    expect(new Set(all).size).toBe(all.length);
  });

  it('never draws the branchless sweep anywhere on the track', () => {
    const all = [
      ...track.building,
      ...track.gating,
      ...track.parked,
      ...track.boarded,
      ...track.landed,
    ];
    expect(all.every((c) => c.branch !== '' && c.branch !== null)).toBe(true);
    expect(all.some((c) => c.title === 'Cluster conformance sweep')).toBe(false);
  });
});

// ---------------------------------------------------------------------
// Actor cards
// ---------------------------------------------------------------------

describe('actorCards — holds, output and last write, all measured', () => {
  const cards = actorCards({
    cars: parseCars(CARS_RAW),
    gateRuns: parseGateRuns(GATE_RUNS_RAW),
    waits: parseWaits(WAITS_RAW),
    today: '2026-09-11',
  });
  const card = (id: string) => cards.find((c) => c.id === id);

  it('lists an actor that completed a car-pipeline step today', () => {
    const runner = card('automation:gate-runner');
    expect(runner).toBeDefined();
    expect(runner!.lane).toBe('automation');
    // Two gate-verdicts in the fixture, but only ONE completed today —
    // the other is stamped 2026-09-10 and must not be counted.
    expect(runner!.completedToday).toBe(1);
  });

  it('counts today by the completion stamp, never by "it is in the window"', () => {
    expect(card('automation:gate-runner')!.completedToday).toBe(1);
    expect(card('automation:train-conductor')!.completedToday).toBe(1);
  });

  it('reports the last write as the latest completion stamp it actually saw', () => {
    expect(card('automation:train-conductor')!.lastWriteAt).toBe('2026-09-11T03:50:00.000000Z');
    expect(card('automation:gate-runner')!.lastWriteAt).toBe('2026-09-11T03:48:59.720283Z');
  });

  it('counts what an actor HOLDS from the open assigned steps, not from the cars', () => {
    // claude@algedonic.dev holds two ready steps in queue-age (one of
    // the four rows is unassigned, one is simulated and one is David's).
    expect(card('claude@algedonic.dev')!.holds).toBe(1);
    expect(card('emp-david')!.holds).toBe(1);
  });

  it('files an actor seen only in queue-age, with no output, as holding and idle', () => {
    const david = card('emp-david')!;
    expect(david.lane).toBe('human');
    expect(david.holds).toBe(1);
    expect(david.completedToday).toBe(0);
    // Never zero-as-a-timestamp: no write seen means null, and the
    // card has to render that as "not recorded".
    expect(david.lastWriteAt).toBeNull();
  });

  it('keeps a simulated actor in its own lane rather than mixing it with the real crew', () => {
    const sim = card('emp-sim-brewer');
    expect(sim).toBeDefined();
    expect(sim!.lane).toBe('sim');
  });

  it('invents no actor — every card id came from a completed_by or an assignee_id', () => {
    const seen = new Set<string>();
    for (const c of [...CARS_RAW.data, ...GATE_RUNS_RAW.data]) {
      for (const s of c.steps) {
        if (s.completed_by) seen.add(s.completed_by);
        if (s.assignee_id) seen.add(s.assignee_id);
      }
    }
    for (const w of WAITS_RAW.data) if (w.assignee_id) seen.add(w.assignee_id);
    for (const card_ of cards) expect(seen.has(card_.id)).toBe(true);
  });

  it('orders the busiest actor first so the board leads with who is working', () => {
    const counts = cards.map((c) => c.completedToday);
    expect([...counts].sort((a, b) => b - a)).toEqual(counts);
  });
});

// ---------------------------------------------------------------------
// Taken, not progressed
// ---------------------------------------------------------------------

describe('takenNotProgressed — claimed, and not moving', () => {
  const waits = parseWaits(WAITS_RAW);

  it('lists only steps somebody actually took', () => {
    const rows = takenNotProgressed(waits, 1);
    expect(rows.every((r) => r.assigneeId !== null)).toBe(true);
    // The unassigned protocol-retro step waits 10 days and is NOT here:
    // it is a queue problem, not a crew problem.
    expect(rows.some((r) => r.jobKind === 'protocol-retro')).toBe(false);
  });

  it('excludes simulated waits, whose clock runs ~1000x the wall', () => {
    expect(takenNotProgressed(waits, 1).some((r) => r.simulated)).toBe(false);
  });

  it('applies the staleness floor, so a step taken minutes ago is not an alarm', () => {
    // David's 15-minute-old decide step is taken and fine.
    expect(takenNotProgressed(waits, 1).some((r) => r.jobKind === 'backlog-item')).toBe(false);
    expect(takenNotProgressed(waits, 0).some((r) => r.jobKind === 'backlog-item')).toBe(true);
  });

  it('sorts longest-waiting first', () => {
    const days = takenNotProgressed(waits, 0).map((r) => r.waitingDays);
    expect([...days].sort((a, b) => b - a)).toEqual(days);
  });
});

// ---------------------------------------------------------------------
// The page, pinned at source level
// ---------------------------------------------------------------------
// `bun test` has no Svelte pass (apps/web/bunfig.toml), so the
// component cannot be mounted. The TriageBoard.test.ts idiom applies:
// read the source, strip comments, assert on executable code only.

const pageSource = readFileSync(new URL('./CrewBoardPage.svelte', import.meta.url), 'utf8');
const pageCode = pageSource
  .replace(/<!--[\s\S]*?-->/g, '')
  .replace(/\/\*[\s\S]*?\*\//g, '')
  .replace(/(^|[^:])\/\/.*$/gm, '$1');

/// The module source too, stripped the same way — the endpoints live in
/// `crew.ts`'s reads, not in the component.
const moduleCode = readFileSync(new URL('./crew.ts', import.meta.url), 'utf8')
  .replace(/\/\*[\s\S]*?\*\//g, '')
  .replace(/(^|[^:])\/\/.*$/gm, '$1');

describe('CrewBoardPage wiring', () => {
  it('reads only the four endpoints this board measured', () => {
    expect(moduleCode).toContain('/api/yard/status');
    expect(moduleCode).toContain('kind=ship-a-change');
    expect(moduleCode).toContain('kind=gate-run');
    expect(moduleCode).toContain('/api/jobs/queue-age');
  });

  it('windows the car reads in the QUERY STRING, not after the fetch', () => {
    // A limit is not a filter, and the server truncates before the
    // client sees a row — the TriageBoard's `closed_within` lesson.
    expect(moduleCode).toMatch(/kind=ship-a-change&limit=/);
    expect(moduleCode).toMatch(/kind=gate-run&limit=/);
  });

  it('ships NO writes-per-hour strip', () => {
    // David, 2026-09-11: "If the audit tail is the blocker, ship the
    // board without the writes strip rather than waiting for it."
    // Measured 2026-09-11: /api/events/* is gateway-fronted and answers
    // 401 from the pod, and it carries no per-actor dimension anyway.
    // A writes strip here could only be fabricated.
    expect(pageCode).not.toContain('/api/events');
    expect(moduleCode).not.toContain('/api/events');
    expect(pageCode).not.toContain('writesPerHour');
    expect(moduleCode).not.toContain('writesPerHour');
  });

  it('branches on the Remote failure case rather than rendering an empty state', () => {
    // The false-empty class data/remote.ts exists to kill.
    expect(pageCode).toContain("'failed'");
  });

  it('derives every figure from the read model, holding no hardcoded count', () => {
    // The rule this board is judged against: no invented numbers. The
    // page must not carry a bare digit where a count belongs.
    expect(pageCode).not.toMatch(/completedToday\s*[:=]\s*\d/);
    expect(pageCode).not.toMatch(/holds\s*[:=]\s*\d/);
  });
});
