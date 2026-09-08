import { describe, expect, test } from 'bun:test';
import {
  ARRIVALS_DRAWN,
  STAGES,
  boardedAtFromTitle,
  drawnWagons,
  journeyStops,
  parseSelection,
  prNumber,
  scene,
  sinceText,
  uniqueTags,
  wagonTag,
} from './yard-floor';
import type { ApproachRow, CarRow, TrainRow, YardState } from './yard';
import type { YardStatus } from './yard-status';

// The floor is the testable half of the map: where every wagon stands,
// which bay is busy, how far along the line each locomotive is, and
// the order the departure board reads. The SVG only draws what this
// says. Every placement below is a real state the yard has been in.

const NOW = Date.parse('2026-09-07T23:20:00Z');
const NOW_ISO = '2026-09-07T23:20:00.000Z';

const yardOf = (over: Partial<YardState> = {}): YardState => ({
  inFlight: [],
  dock: [],
  dockStation: { source: 'derived' },
  arrivals: [],
  cancelled: [],
  delivery: [],
  awaitingProof: [],
  approach: [],
  cars: [],
  packets: { trains: [], gateRuns: [] },
  ...over,
});

const statusOf = (over: Partial<YardStatus> = {}): YardStatus => ({
  trains: [],
  dock: [],
  boarding: {
    dock_threshold: 1,
    cooldown_minutes: 45,
    at_times: [],
    dock_depth: 0,
    threshold_met: false,
    summary: '',
    held_because: null,
    cooldown_remaining_minutes: null,
    last_board_at: null,
    next_board: null,
  },
  recent: [],
  stranded: [],
  held: [],
  gates: { capacity: 3, active: [] },
  garage: [],
  policy: { stall_hours: 2, max_red_trains: 2 },
  conductor: null,
  now: NOW_ISO,
  ...over,
});

const car = (id: string, branch: string, over: Partial<CarRow> = {}): CarRow => ({
  id,
  kind: 'ship-a-change',
  branch,
  title: `Car ${id}`,
  tags: [],
  sim: false,
  skipReason: null,
  head: 'abc1234',
  ...over,
});

const trainRow = (id: string, status: TrainRow['status'], over: Partial<TrainRow> = {}): TrainRow => ({
  id,
  title: 'PR train 2026-09-07 23:37',
  prUrl: 'http://10.20.0.15:3000/david/boss/pulls/259',
  status,
  lamp: 'pending',
  mergeRef: null,
  deployed: null,
  convergingSince: null,
  cars: [],
  live: true,
  outcome: 'unknown',
  arrivedAt: { ms: 0, at: '', basis: 'opened_on' },
  eta: { kind: 'phase', phase: 'ci' },
  trouble: null,
  cancelRequested: null,
  cancelRefused: false,
  ...over,
});

const approachRow = (
  id: string,
  branch: string,
  state: ApproachRow['state'],
  over: Partial<ApproachRow> = {},
): ApproachRow => ({
  id,
  branch,
  sha: null,
  state,
  opened_on: '2026-09-07',
  note: null,
  hold: null,
  verdict: null,
  ...over,
});

const gate = (branch: string, packet_id: string, stale = false) => ({
  branch,
  packet_id,
  since: '2026-09-07T23:05:00Z',
  stale,
});

const wagon = (s: ReturnType<typeof scene>, id: string) => {
  const w = s.wagons.find(x => x.id === id);
  if (!w) throw new Error(`no wagon ${id} — have ${s.wagons.map(x => x.id).join(', ')}`);
  return w;
};

describe('wagonTag — the short name painted on a wagon', () => {
  test('drops the branch prefix and the filler words, keeps what fits in eleven', () => {
    expect(wagonTag('feat/the-yard-has-a-cancel-button')).toBe('yard-cancel');
    expect(wagonTag('fix/a-day-of-arrivals-does-not-empty-the-yard')).toBe('day-empty');
    expect(wagonTag('fix/boot-never-refuses-over-an-unviable-workflow')).toBe('boot-never');
    expect(wagonTag('feat/the-break-glass-door-has-its-keys')).toBe('break-glass');
    expect(wagonTag('docs/x')).toBe('x');
  });

  test('a single word longer than eleven is cut, never padded with a second', () => {
    expect(wagonTag('feat/internationalization-of-things')).toBe('internation');
    expect(wagonTag('internationalization-of-things').length).toBeLessThanOrEqual(11);
  });

  test('a branch made only of filler words keeps its own words', () => {
    expect(wagonTag('fix/the-a')).toBe('the-a');
  });

  test('is deterministic and never longer than eleven', () => {
    const branches = [
      'feat/the-conductor-says-why-it-is-not-boarding',
      'fix/a-merged-train-does-not-hold-the-track',
      'feat/claude-persists-across-restarts',
      'fix/the-dev-pod-reclaims-its-build-cache',
      'x',
      '',
    ];
    for (const b of branches) {
      expect(wagonTag(b)).toBe(wagonTag(b));
      expect(wagonTag(b).length).toBeLessThanOrEqual(11);
    }
  });
});

describe('uniqueTags — one nameplate per branch on the floor', () => {
  const says = 'feat/the-conductor-says-why-it-is-not-boarding';
  const honours = 'feat/the-conductor-honours-a-cancel-request';

  test('two branches that share a base part on their second word, both renamed, deterministic in any order', () => {
    expect(wagonTag(says)).toBe(wagonTag(honours));
    const t = uniqueTags([says, honours]);
    expect(t.get(says)).toBe('conduc-says');
    expect(t.get(honours)).toBe('con-honours');
    expect(uniqueTags([honours, says])).toEqual(t);
    for (const tag of t.values()) expect(tag.length).toBeLessThanOrEqual(11);
  });

  test('an uncontested base keeps its name, and a replacement never takes one', () => {
    const t = uniqueTags([says, honours, 'feat/conduc-says-x', 'fix/a-day-of-jobs-lists-newest-first']);
    expect(t.get('fix/a-day-of-jobs-lists-newest-first')).toBe('day-jobs');
    expect(t.get('feat/conduc-says-x')).toBe('conduc-says');
    expect(t.get(says)).toBe('conduct-why');
    expect(new Set(t.values()).size).toBe(4);
  });

  test('the floor paints the unique names on the wagons and the bay labels', () => {
    const s = scene(
      yardOf({ dock: [car('c1', says)], cars: [car('c1', says), car('c2', honours)] }),
      statusOf({ gates: { capacity: 3, active: [gate(honours, 'g1')] } }),
      NOW,
    );
    expect(wagon(s, 'c1').tag).toBe('conduc-says');
    expect(wagon(s, 'c2').tag).toBe('con-honours');
    expect(s.bays[0]?.tag).toBe('con-honours');
  });
});

describe('prNumber + boardedAtFromTitle', () => {
  test('reads the PR number off the forge URL, either spelling', () => {
    expect(prNumber('http://10.20.0.15:3000/david/boss/pulls/259')).toBe(259);
    expect(prNumber('https://github.com/x/y/pull/12')).toBe(12);
    expect(prNumber(null)).toBeNull();
    expect(prNumber('http://forge/no-number')).toBeNull();
  });

  test("reads the conductor's boarding minute off the train title, as UTC", () => {
    expect(boardedAtFromTitle('PR train 2026-09-07 23:37')).toBe('2026-09-07T23:37:00.000Z');
    expect(boardedAtFromTitle('a train with no stamp')).toBeNull();
  });
});

describe('sinceText', () => {
  test('an instant reads as elapsed; a bare date reads as that date; nothing reads as a dash', () => {
    expect(sinceText('2026-09-07T23:05:00Z', NOW)).toBe('15m');
    expect(sinceText('2026-09-07', NOW)).toBe('Sep 7, 2026');
    expect(sinceText(null, NOW)).toBe('—');
    expect(sinceText('', NOW)).toBe('—');
  });
});

describe('the gate bays', () => {
  test('a gating branch with an open car is that car, in bay 1, working', () => {
    const s = scene(
      yardOf({ cars: [car('c1', 'feat/x')] }),
      statusOf({ gates: { capacity: 3, active: [gate('feat/x', 'g1')] } }),
      NOW,
    );
    const w = wagon(s, 'c1');
    expect(w.station).toBe('gate');
    expect(w.slot).toBe(0);
    expect(w.lamp).toBe('working');
    expect(w.tone).toBe('ok');
    expect(w.status).toContain('gating');
    expect(s.bays).toHaveLength(3);
    expect(s.bays[0]).toMatchObject({ busy: true, branch: 'feat/x', packetId: 'g1', wagonId: 'c1', stale: false });
    expect(s.bays[1]?.busy).toBe(false);
    // No packet-id wagon rides beside the car: one branch, one wagon.
    expect(s.wagons.filter(x => x.branch === 'feat/x')).toHaveLength(1);
  });

  test('a gating branch with no car yet is the gate packet itself', () => {
    const s = scene(yardOf(), statusOf({ gates: { capacity: 3, active: [gate('feat/y', 'g9')] } }), NOW);
    expect(wagon(s, 'g9').station).toBe('gate');
    expect(wagon(s, 'g9').tag).toBe('y');
  });

  test("a gate's since as a bare date draws no elapsed and no progress; as an instant it draws both", () => {
    const dated = scene(yardOf(), statusOf({ gates: { capacity: 3, active: [{ ...gate('feat/y', 'g9'), since: '2026-09-07' }] } }), NOW);
    expect(dated.bays[0]).toMatchObject({ elapsed: 'Sep 7, 2026', progress: 0 });
    expect(wagon(dated, 'g9').status).toBe('gating · Sep 7, 2026');
    const timed = scene(yardOf(), statusOf({ gates: { capacity: 3, active: [{ ...gate('feat/y', 'g9'), since: '2026-09-07T23:14:00Z' }] } }), NOW);
    expect(timed.bays[0]).toMatchObject({ elapsed: '6m' });
    expect(timed.bays[0]?.progress).toBeCloseTo(6 / 12, 5);
    // Past the usual it is full, not over: the bar is a drawing scale.
    expect(scene(yardOf(), statusOf({ gates: { capacity: 3, active: [gate('feat/y', 'g9')] } }), NOW).bays[0]?.progress).toBe(1);
  });

  test('a stale gate warns — a dead Job looks like a slow one from here, and the bay says so', () => {
    const s = scene(yardOf(), statusOf({ gates: { capacity: 3, active: [gate('feat/y', 'g9', true)] } }), NOW);
    expect(wagon(s, 'g9').lamp).toBe('warn');
    expect(wagon(s, 'g9').tone).toBe('warn');
    expect(s.bays[0]?.stale).toBe(true);
  });

  test('more active gates than capacity widen the bays rather than hide a run', () => {
    const s = scene(
      yardOf(),
      statusOf({ gates: { capacity: 1, active: [gate('a', 'g1'), gate('b', 'g2')] } }),
      NOW,
    );
    expect(s.bays).toHaveLength(2);
    expect(wagon(s, 'g2').slot).toBe(1);
  });
});

describe('the dock and the garage', () => {
  test('a parked car sits on the dock in server order, since the server\'s parked_since', () => {
    const s = scene(
      yardOf({ dock: [car('c1', 'fix/a'), car('c2', 'fix/b', { skipReason: 'track occupied' })] }),
      statusOf({
        dock: [
          { id: 'c1', title: 'Car c1', branch: 'fix/a', parked_since: '2026-09-07T22:00:00Z' },
          { id: 'c2', title: 'Car c2', branch: 'fix/b', parked_since: '2026-09-07T22:30:00Z' },
        ],
      }),
      NOW,
    );
    expect(wagon(s, 'c1')).toMatchObject({ station: 'dock', slot: 0, lamp: 'ok', tone: 'ok', since: '2026-09-07T22:00:00Z' });
    expect(wagon(s, 'c1').status).toContain('parked');
    expect(wagon(s, 'c2').slot).toBe(1);
    expect(wagon(s, 'c2').status).toContain('track occupied');
  });

  test('a parked car being re-gated is ONE wagon, in the bay, keeping its car id', () => {
    const c = car('c1', 'fix/a');
    const s = scene(
      yardOf({ dock: [c], cars: [c] }),
      statusOf({ gates: { capacity: 3, active: [gate('fix/a', 'g1')] } }),
      NOW,
    );
    expect(s.wagons.filter(w => w.branch === 'fix/a')).toHaveLength(1);
    expect(wagon(s, 'c1').station).toBe('gate');
  });

  test('a red gate puts the car in the garage with the check that failed', () => {
    const s = scene(
      yardOf({
        cars: [car('c1', 'fix/red')],
        approach: [approachRow('g1', 'fix/red', 'gated-red', { verdict: 'failed' })],
      }),
      statusOf({ garage: [{ branch: 'fix/red', failed_check: 'clippy, test', since: '2026-09-07' }] }),
      NOW,
    );
    const w = wagon(s, 'c1');
    expect(w.station).toBe('garage');
    expect(w.tone).toBe('red');
    expect(w.lamp).toBe('err');
    expect(w.status).toContain('clippy, test');
  });

  test('a garaged branch the approach no longer lists still stands in the garage', () => {
    // The approach drops closed verdicts after two days; the server's
    // garage keeps the latest run per branch. The floor shows both.
    const s = scene(
      yardOf(),
      statusOf({ garage: [{ branch: 'fix/old-red', failed_check: null, since: '2026-09-01' }] }),
      NOW,
    );
    const w = wagon(s, 'garage:fix/old-red');
    expect(w.station).toBe('garage');
    expect(w.status).toContain('run died outside a check');
    expect(s.machines.garage.count).toBe(1);
  });

  test('a lost verdict stands at the gate exit, not in the garage — the change was never judged', () => {
    const s = scene(
      yardOf({ approach: [approachRow('g1', 'fix/lost', 'gated-red', { verdict: 'lost' })] }),
      statusOf(),
      NOW,
    );
    const w = wagon(s, 'g1');
    expect(w.station).toBe('limbo');
    expect(w.tone).toBe('warn');
    expect(w.lamp).toBe('warn');
    expect(w.status).toContain('re-gate');
  });
});

describe('the approach siding', () => {
  test('a stranded green, a held car and a publish request each stand on the approach and say which they are', () => {
    const s = scene(
      yardOf({
        approach: [
          approachRow('p1', 'feat/pub', 'publishing', { note: 'emp-david' }),
          approachRow('g1', 'feat/stranded', 'gated-green'),
          approachRow('g2', 'feat/held', 'held', { hold: 'waiting on #240' }),
        ],
      }),
      statusOf({ stranded: [{ branch: 'feat/stranded' }] }),
      NOW,
    );
    expect(wagon(s, 'p1')).toMatchObject({ station: 'approach', tone: 'static', lamp: 'working' });
    expect(wagon(s, 'p1').status).toContain('publishing');
    expect(wagon(s, 'g1')).toMatchObject({ station: 'approach', tone: 'warn', lamp: 'warn' });
    expect(wagon(s, 'g1').status).toContain('stranded');
    expect(wagon(s, 'g2')).toMatchObject({ station: 'approach', tone: 'ok', lamp: 'off' });
    expect(wagon(s, 'g2').status).toBe('held — waiting on #240');
    // An operator's marker that opens with its own "held:" is not said twice.
    const twice = scene(
      yardOf({ approach: [approachRow('g3', 'feat/h', 'held', { hold: 'held: rolls the dev pod' })] }),
      statusOf(),
      NOW,
    );
    expect(wagon(twice, 'g3').status).toBe('held — rolls the dev pod');
    expect(s.wagons.filter(w => w.station === 'approach').map(w => w.slot)).toEqual([0, 1, 2]);
    expect(s.machines.approach.label).toBe('1 stranded · 1 publishing · 1 held');
  });
});

describe('the track — wagons behind a locomotive', () => {
  const aboard = (status: TrainRow['status'], over: Partial<TrainRow> = {}) =>
    scene(
      yardOf({
        inFlight: [trainRow('t1', status, { cars: [car('c1', 'fix/a'), car('c2', 'fix/b')], ...over })],
      }),
      statusOf(),
      NOW,
    );

  test('a boarding train is at the PR signal; its cars ride behind it in consist order', () => {
    const s = aboard('BOARDING');
    expect(s.locos).toHaveLength(1);
    expect(s.locos[0]).toMatchObject({ id: 't1', n: 259, stage: 0, progress: 0, blocked: null, cars: ['c1', 'c2'] });
    expect(wagon(s, 'c1')).toMatchObject({ station: 'train', trainId: 't1', slot: 0, lamp: 'working', tone: 'ok' });
    expect(wagon(s, 'c2').slot).toBe(1);
    expect(wagon(s, 'c1').status).toBe('aboard #259 · PR');
  });

  test('each stage of the line is a stage index over PR · CI · merge · deploy · converge · arrived', () => {
    expect(STAGES).toEqual(['PR', 'CI', 'merge', 'deploy', 'converge', 'arrived']);
    expect(aboard('BOARDED', { lamp: 'pending' }).locos[0]?.stage).toBe(1);
    expect(aboard('BOARDED', { lamp: 'green' }).locos[0]?.stage).toBe(2);
    expect(aboard('DEPARTED').locos[0]?.stage).toBe(3);
    expect(aboard('CONVERGING').locos[0]?.stage).toBe(4);
  });

  test('progress within a stage comes from the ETA leg where it has a reading, else 0', () => {
    const eta = { kind: 'eta', phase: 'ci', atMs: NOW + 1, basis: 'median of last 4 arrivals', progress: 0.4 } as const;
    expect(aboard('BOARDED', { lamp: 'pending', eta }).locos[0]?.progress).toBe(0.4);
    const deploying = { ...eta, phase: 'deploying', progress: 0.5 } as const;
    expect(aboard('DEPARTED', { eta: deploying }).locos[0]?.progress).toBe(0.5);
    // Awaiting merge sits AT the merge signal: nothing about a green
    // lamp says how close the merge is.
    expect(aboard('BOARDED', { lamp: 'green', eta: { ...eta, phase: 'merging', progress: 0.9 } }).locos[0]?.progress).toBe(0);
    expect(aboard('BOARDED', { lamp: 'pending' }).locos[0]?.progress).toBe(0);
  });

  test('a blocked train says why, from the server block first, and its cars read red', () => {
    const s = scene(
      yardOf({
        inFlight: [
          trainRow('t1', 'BOARDED', { lamp: 'failing', trouble: { kind: 'ci-red' }, cars: [car('c1', 'fix/a')] }),
        ],
      }),
      statusOf({
        trains: [
          { id: 't1', title: 'PR train 2026-09-07 23:37', phase: 'awaiting-ci', at_step: 'CI', block: { kind: 'ci-red', checks: 'clippy' }, ci_result: 'failing', pr_url: null, car_count: 1, boarded_at: null },
        ],
      }),
      NOW,
    );
    expect(s.locos[0]?.blocked).toBe('CI RED — clippy');
    expect(wagon(s, 'c1')).toMatchObject({ tone: 'red', lamp: 'err' });
    expect(wagon(s, 'c1').status).toBe('aboard #259 · blocked at CI');
    // The board's trouble stands in when the server sent no block.
    const t = aboard('BOARDED', { lamp: 'failing', trouble: { kind: 'ci-red' } });
    expect(t.locos[0]?.blocked).toBe('CI RED');
  });

  test('a train with no PR yet has no number; the title stands in on the board', () => {
    const s = aboard('BOARDING', { prUrl: null });
    expect(s.locos[0]?.n).toBeNull();
    expect(s.boardRows[0]?.where).toBe('Track · PR train 2026-09-07 23:37 at PR');
  });

  test("a car aboard is 'since' the server's boarded_at, else the conductor's boarding minute read off the train title", () => {
    expect(wagon(aboard('BOARDED'), 'c1').since).toBe('2026-09-07T23:37:00.000Z');
    const served = scene(
      yardOf({ inFlight: [trainRow('t1', 'BOARDED', { cars: [car('c1', 'fix/a')] })] }),
      statusOf({
        trains: [{ id: 't1', title: 'PR train 2026-09-07 23:37', phase: 'awaiting-ci', at_step: 'ci', block: null, ci_result: null, pr_url: null, car_count: 1, boarded_at: '2026-09-07T23:37:12Z' }],
      }),
      NOW,
    );
    expect(wagon(served, 'c1').since).toBe('2026-09-07T23:37:12Z');
  });
});

describe('the signals along the track', () => {
  test('no train: every lamp off', () => {
    expect(scene(yardOf(), statusOf(), NOW).signals).toEqual(['off', 'off', 'off', 'off', 'off', 'off']);
  });

  test('the lead train lights the signals: passed green, current pulsing, blocked red', () => {
    const s = scene(yardOf({ inFlight: [trainRow('t1', 'DEPARTED')] }), statusOf(), NOW);
    expect(s.signals).toEqual(['ok', 'ok', 'ok', 'now', 'off', 'off']);
    const b = scene(
      yardOf({ inFlight: [trainRow('t1', 'BOARDED', { lamp: 'failing', trouble: { kind: 'ci-red' } })] }),
      statusOf(),
      NOW,
    );
    expect(b.signals).toEqual(['ok', 'err', 'off', 'off', 'off', 'off']);
  });

  test('with two trains open, the one furthest along leads', () => {
    const s = scene(
      yardOf({ inFlight: [trainRow('t1', 'BOARDED'), trainRow('t2', 'CONVERGING')] }),
      statusOf(),
      NOW,
    );
    expect(s.signals).toEqual(['ok', 'ok', 'ok', 'ok', 'now', 'off']);
  });
});

describe('the arrivals yard', () => {
  const landed = (id: string, at: string, mergeRef = 'b641f3a') =>
    trainRow(id, 'ARRIVED', {
      live: false,
      outcome: 'arrived',
      mergeRef,
      arrivedAt: { ms: Date.parse(at), at, basis: 'completed_at' },
      cars: [car(`${id}-c`, `fix/${id}`)],
    });

  test('a landed car stacks in the arrivals yard, newest first, dimmed on the board', () => {
    const s = scene(
      yardOf({ arrivals: [landed('t9', '2026-09-07T23:00:00Z'), landed('t8', '2026-09-07T20:00:00Z')] }),
      statusOf(),
      NOW,
    );
    expect(wagon(s, 't9-c')).toMatchObject({ station: 'arrivals', slot: 0, lamp: 'ok', tone: 'ok', trainId: 't9', since: '2026-09-07T23:00:00Z' });
    expect(wagon(s, 't9-c').status).toBe('landed in b641f3a · arrived');
    expect(wagon(s, 't8-c').slot).toBe(1);
    const rows = s.boardRows.filter(r => r.landed);
    expect(rows.map(r => r.id)).toEqual(['t9-c', 't8-c']);
    expect(rows[0]?.where).toBe('Arrivals');
  });

  test('landed-in-the-last-24h is the arrivals machine\'s count', () => {
    const s = scene(
      yardOf({ arrivals: [landed('t9', '2026-09-07T23:00:00Z'), landed('t1', '2026-09-05T23:00:00Z')] }),
      statusOf(),
      NOW,
    );
    expect(s.machines.arrivals.landed).toBe(1);
    expect(s.machines.arrivals.label).toBe('1 landed · 24h');
  });

  test('the map draws the newest few landed wagons and counts the rest; the board keeps them all', () => {
    const arrivals = Array.from({ length: ARRIVALS_DRAWN + 5 }, (_, i) =>
      landed(`t${i}`, new Date(Date.parse('2026-09-07T23:00:00Z') - i * 60_000).toISOString()),
    );
    const s = scene(yardOf({ arrivals }), statusOf(), NOW);
    expect(s.wagons.filter(w => w.station === 'arrivals')).toHaveLength(ARRIVALS_DRAWN + 5);
    const { drawn, hidden } = drawnWagons(s.wagons);
    expect(drawn.filter(w => w.station === 'arrivals').map(w => w.id)).toEqual(
      Array.from({ length: ARRIVALS_DRAWN }, (_, i) => `t${i}-c`),
    );
    expect(hidden).toBe(5);
    expect(s.boardRows.filter(r => r.landed)).toHaveLength(ARRIVALS_DRAWN + 5);
    // Nothing in flight is ever hidden.
    const busy = scene(yardOf({ dock: [car('d1', 'fix/a')], arrivals }), statusOf(), NOW);
    expect(drawnWagons(busy.wagons).drawn.some(w => w.id === 'd1')).toBe(true);
  });

  test("a cancelled train's cars are not placed — they are back on the dock if anywhere", () => {
    const s = scene(
      yardOf({ cancelled: [trainRow('tx', 'ARRIVED', { outcome: 'cancelled', cars: [car('cx', 'fix/x')] })] }),
      statusOf(),
      NOW,
    );
    expect(s.wagons).toHaveLength(0);
  });
});

describe('the departure board', () => {
  test('in flight first, along the line; landed below', () => {
    const s = scene(
      yardOf({
        cars: [car('gc', 'feat/gating')],
        approach: [
          approachRow('p1', 'feat/pub', 'publishing'),
          approachRow('l1', 'feat/lost', 'gated-red', { verdict: 'lost' }),
          approachRow('r1', 'feat/red', 'gated-red', { verdict: 'failed' }),
        ],
        dock: [car('d1', 'feat/parked')],
        inFlight: [trainRow('t1', 'BOARDED', { cars: [car('a1', 'feat/aboard')] })],
        arrivals: [
          trainRow('t0', 'ARRIVED', {
            live: false,
            outcome: 'arrived',
            mergeRef: '1234567',
            arrivedAt: { ms: NOW - 1000, at: '2026-09-07T23:19:59Z', basis: 'completed_at' },
            cars: [car('z1', 'feat/landed')],
          }),
        ],
      }),
      statusOf({ gates: { capacity: 3, active: [gate('feat/gating', 'g1')] } }),
      NOW,
    );
    expect(s.boardRows.map(r => [r.id, r.where])).toEqual([
      ['p1', 'Approach'],
      ['gc', 'Gate bay 1'],
      ['l1', 'Gate exit'],
      ['d1', 'Dock · slot 1'],
      ['r1', 'Garage'],
      ['a1', 'Track · #259 at CI'],
      ['z1', 'Arrivals'],
    ]);
    expect(s.boardRows.map(r => r.landed)).toEqual([false, false, false, false, false, false, true]);
  });
});

describe('the machines', () => {
  test('the dock says why it is not boarding, in the server\'s words', () => {
    const held = scene(
      yardOf({ dock: [car('d1', 'x')] }),
      statusOf({
        boarding: { ...statusOf().boarding, held_because: 'track occupied (1 open train)', next_board: 'boards on the next tick once the track clears', cooldown_remaining_minutes: 29 },
      }),
      NOW,
    );
    expect(held.machines.dock).toMatchObject({ parked: 1, held: 'track occupied (1 open train)', next: 'boards on the next tick once the track clears', cooldownMinutes: 29 });
    expect(held.machines.dock.label).toBe('1 parked · held: track occupied (1 open train)');
    // An empty dock is not held — nothing is there to hold. It says
    // what the next car would meet.
    const cooling = scene(yardOf(), statusOf({ boarding: { ...statusOf().boarding, cooldown_remaining_minutes: 12, held_because: 'cooldown — 12 min left' } }), NOW);
    expect(cooling.machines.dock.label).toBe('empty · cooldown 12 min');
    const free = scene(yardOf(), statusOf({ boarding: { ...statusOf().boarding, next_board: 'boards on the next tick' } }), NOW);
    expect(free.machines.dock.label).toBe('empty · boards on the next tick');
    const below = scene(yardOf(), statusOf({ boarding: { ...statusOf().boarding, held_because: 'below threshold (0/1)', next_board: 'boards on the next tick once the dock reaches 1' } }), NOW);
    expect(below.machines.dock.label).toBe('empty');
    // Cars parked with nothing keeping them depart on the next tick.
    const ready = scene(yardOf({ dock: [car('d1', 'x')] }), statusOf({ boarding: { ...statusOf().boarding, next_board: 'boards on the next tick' } }), NOW);
    expect(ready.machines.dock.label).toBe('1 parked · departs next tick');
    // An older server that sends no hold: the count, and no guess.
    expect(scene(yardOf(), statusOf(), NOW).machines.dock.label).toBe('empty');
    expect(scene(yardOf({ dock: [car('d1', 'x')] }), statusOf(), NOW).machines.dock.label).toBe('1 parked');
  });

  test("the conductor's clock: last seen, the next tick from its own heartbeat, silence as an alarm", () => {
    const live = scene(
      yardOf(),
      statusOf({
        conductor: { last_seen: '2026-09-07T23:10:00Z', silent_for_minutes: 10, expected_every_minutes: 10, silent: false, last_verb: 'reconcile', last_rc: 0 },
      }),
      NOW,
    );
    expect(live.machines.conductor).toMatchObject({ lamp: 'ok', silent: false, lastSeen: '2026-09-07T23:10:00Z', nextTick: '2026-09-07T23:20:00.000Z' });
    expect(live.machines.conductor.label).toBe('next tick ≤ 23:20 UTC');
    const silent = scene(
      yardOf(),
      statusOf({
        conductor: { last_seen: '2026-09-07T22:00:00Z', silent_for_minutes: 80, expected_every_minutes: 10, silent: true, last_verb: 'reconcile', last_rc: 0 },
      }),
      NOW,
    );
    expect(silent.machines.conductor).toMatchObject({ lamp: 'err', silent: true });
    expect(silent.machines.conductor.label).toBe('SILENT · 80m since it last fired');
    const none = scene(yardOf(), statusOf({ conductor: null }), NOW);
    expect(none.machines.conductor).toMatchObject({ lamp: 'off', nextTick: null });
    expect(none.machines.conductor.label).toBe('no reading');
  });

  test('the runner shed and the cluster tower have no reading until the page feeds one — that is what they say', () => {
    const s = scene(yardOf(), statusOf(), NOW);
    expect(s.machines.runner).toEqual({ kind: 'unknown' });
    expect(s.machines.cluster).toEqual({ kind: 'unknown' });
    const fed = scene(yardOf(), statusOf(), NOW, {
      runner: { kind: 'idle', last: null },
      cluster: { kind: 'ready', commit: '0d8c37d', since: NOW_ISO },
    });
    expect(fed.machines.runner).toEqual({ kind: 'idle', last: null });
    expect(fed.machines.cluster).toEqual({ kind: 'ready', commit: '0d8c37d', since: NOW_ISO });
  });

  test('the scene carries the server clock, or the local one when the status is absent', () => {
    expect(scene(yardOf(), statusOf(), NOW).now).toBe(NOW_ISO);
    expect(scene(yardOf(), null, NOW).now).toBe(NOW_ISO);
    // No status at all: the bays are unknown, not zero — an outage is
    // not an idle yard.
    expect(scene(yardOf(), null, NOW).bays).toEqual([]);
  });
});

describe('parseSelection — the one string the map, the board and the alerts all speak', () => {
  test('round-trips every entity kind', () => {
    expect(parseSelection('car:abc')).toEqual({ kind: 'car', id: 'abc' });
    expect(parseSelection('train:t1')).toEqual({ kind: 'train', id: 't1' });
    expect(parseSelection('bay:2')).toEqual({ kind: 'bay', index: 2 });
    expect(parseSelection('track')).toEqual({ kind: 'track' });
    expect(parseSelection('dock')).toEqual({ kind: 'dock' });
    expect(parseSelection('garage')).toEqual({ kind: 'garage' });
    expect(parseSelection('approach')).toEqual({ kind: 'approach' });
    expect(parseSelection('arrivals')).toEqual({ kind: 'arrivals' });
    expect(parseSelection('conductor')).toEqual({ kind: 'conductor' });
    expect(parseSelection('runner')).toEqual({ kind: 'runner' });
    expect(parseSelection('cluster')).toEqual({ kind: 'cluster' });
    // A key from a future map falls back to the track, never throws.
    expect(parseSelection('from-the-future')).toEqual({ kind: 'track' });
    expect(parseSelection('bay:x')).toEqual({ kind: 'track' });
  });
});

describe("journeyStops — a packet's completed steps as a journey", () => {
  test('every completed step with a stamp is a stop; the gate step carries its receipt', () => {
    const job = {
      steps: [
        { spec_slug: 'opened', title: 'Opened', status: 'completed', metadata: {} },
        { spec_slug: 'gate', title: 'Gate', status: 'completed', metadata: { completed_at: '2026-09-07T22:10:00Z', receipt: JSON.stringify({ verdict: 'green', head: 'b4f38151234', fails: [] }) } },
        { spec_slug: 'review', title: 'Open for review', status: 'ready', metadata: {} },
        { spec_slug: 'merged', title: 'Merged', status: 'pending', metadata: {} },
      ],
    };
    expect(journeyStops(job)).toEqual([
      { lamp: 'ok', what: 'Opened', when: null, note: null },
      { lamp: 'ok', what: 'Gate', when: '2026-09-07T22:10:00Z', note: 'green · b4f3815' },
      { lamp: 'working', what: 'Open for review', when: null, note: null },
    ]);
  });

  test('a red receipt names what failed and reads red; no job reads as no stops', () => {
    const job = {
      steps: [
        { spec_slug: 'gate', title: 'Gate', status: 'completed', completed_at: '2026-09-07T22:10:00Z', metadata: { receipt: JSON.stringify({ verdict: 'failed', head: 'abcdef0', fails: ['clippy', 'test'] }) } },
      ],
    };
    expect(journeyStops(job)).toEqual([
      { lamp: 'err', what: 'Gate', when: '2026-09-07T22:10:00Z', note: 'failed · abcdef0 · clippy, test' },
    ]);
    expect(journeyStops(null)).toEqual([]);
  });
});
