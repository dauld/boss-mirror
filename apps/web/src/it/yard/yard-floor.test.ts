import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  ARRIVALS_DRAWN,
  NO_FEEDS,
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
import { carRow, type ApproachRow, type CarProof, type CarRow, type JobLite, type TrainRow, type YardState } from './yard';
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
  // The dock as the station served it — the only way the dock lane
  // is read now (the client-side re-derivation is gone).
  dockStation: {
    source: 'station',
    discipline: ['priority', 'age'],
    wipLimit: null,
    overLimit: false,
    total: 0,
    upstream: null,
  },
  arrivals: [],
  cancelled: [],
  withdrawn: [],
  delivery: [],
  awaitingProof: [],
  publishing: [],
  cars: [],
  packets: { trains: [], gateRuns: [] },
  day: null,
  ...over,
});

const statusOf = (over: Partial<YardStatus> = {}): YardStatus => ({
  trains: [],
  dock: [],
  boarding: {
    dock_threshold: 1,
    cooldown_minutes: 45,
    at_times: [],
    cadence_reading: 'read',
    dock_depth: 0,
    threshold_met: false,
    summary: '',
    held_because: null,
    cooldown_remaining_minutes: null,
    last_board_at: null,
    last_board_reading: 'read',
    next_board: null,
  },
  recent: [],
  stranded: [],
  held: [],
  held_cars: [],
  gates: { capacity: 3, active: [], queued: [], typical_seconds: null },
  garage: [],
  limbo: [],
  policy: { stall_hours: 2, max_red_trains: 2 },
  conductor: null,
  gate_runs: null,
  sidings: [],
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
  deliveryChannel: 'software',
  ...over,
});

/** What a car's packet records about proving it — the four `proof_*`
 *  fields plus the `proven` step's stamp, as [`readCarProof`] reads them. */
const proofOf = (over: Partial<CarProof> = {}): CarProof => ({
  probe: null,
  expect: null,
  event: null,
  attempt: null,
  stamped: null,
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

/** A train that arrived at `at`, carrying one car. */
const arrivedWith = (trainId: string, at: string, c: CarRow): TrainRow =>
  trainRow(trainId, 'ARRIVED', {
    live: false,
    outcome: 'arrived',
    mergeRef: 'b641f3a',
    arrivedAt: { ms: Date.parse(at), at, basis: 'completed_at' },
    cars: [c],
  });

/** A publish-request row — the one approach lane the client still
 *  supplies (the open publish-request packets, mapped 1:1). The verdict
 *  lanes come from the status payload's `garage` / `limbo` / `stranded`
 *  / `held` below. */
const publishRow = (id: string, branch: string, over: Partial<ApproachRow> = {}): ApproachRow => ({
  id,
  branch,
  sha: null,
  state: 'publishing',
  opened_on: '2026-09-07',
  note: null,
  hold: null,
  verdict: null,
  ...over,
});

const SINCE = '2026-09-07';
const garaged = (branch: string, packet_id: string, failed_check: string | null = null, failed_line: string | null = null) => ({
  branch, failed_check, failed_line, since: SINCE, packet_id, sha: null,
});
const unjudged = (branch: string, packet_id: string, verdict = 'lost') => ({
  branch, verdict, since: SINCE, packet_id, sha: null,
});
const strandedGreen = (branch: string, packet_id: string) => ({
  branch, packet_id, sha: null, since: SINCE,
});
const heldGreen = (branch: string, packet_id: string, reason: string) => ({
  branch, reason, since: SINCE, packet_id, sha: null,
});
/** A car standing ON the dock that cannot board — a dock row plus its
 *  reason, which is exactly the wire shape (the Rust `HeldCar` flattens
 *  a `DockCar`). */
const heldCar = (id: string, branch: string, reason: string) => ({
  id, title: `Car ${id}`, branch, parked_since: SINCE, reason,
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
      statusOf({ gates: { capacity: 3, active: [gate(honours, 'g1')], queued: [], typical_seconds: null } }),
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
      statusOf({ gates: { capacity: 3, active: [gate('feat/x', 'g1')], queued: [], typical_seconds: null } }),
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
    const s = scene(yardOf(), statusOf({ gates: { capacity: 3, active: [gate('feat/y', 'g9')], queued: [], typical_seconds: null } }), NOW);
    expect(wagon(s, 'g9').station).toBe('gate');
    expect(wagon(s, 'g9').tag).toBe('y');
  });

  // A TRAIN's gate-run in a bay (128b5496) is the train being tested. It
  // used to stand there as a `train/…` wagon indistinguishable from a
  // PR car, and the question "why is a PR car in the gates" came twice in
  // one afternoon (2026-09-14). The server names the train on the row;
  // the floor draws the bay as the train under test.
  test('a train gate in a bay reads as the train under test, not a car', () => {
    const s = scene(
      yardOf({ inFlight: [trainRow('t1', 'DEPARTED')] }),
      statusOf({
        gates: {
          capacity: 3,
          active: [{ ...gate('train/20260914-1727', 'g7'), train: 't1' }],
          queued: [{ branch: 'train/20260914-1800', packet_id: 'g8', queued_at: '2026-09-07T23:10:00Z', position: 1, waiting_seconds: 600, estimated_wait_seconds: null, train: 't2' }],
          typical_seconds: null,
        },
      }),
      NOW,
    );
    const w = wagon(s, 'g7');
    expect(w.station).toBe('gate');
    expect(w.kind).toBe('train-gate');
    expect(w.tag).toBe('train gate');
    expect(w.trainId).toBe('t1');
    expect(w.title).toContain('train t1');
    expect(w.status).toContain('testing the train');
    expect(s.bays[0]?.tag).toBe('train gate');
    const q = wagon(s, 'g8');
    expect(q.kind).toBe('train-gate');
    expect(q.trainId).toBe('t2');
    expect(q.tag).toBe('train gate');
    // A car's gate is untouched by this.
    const c = scene(yardOf(), statusOf({ gates: { capacity: 3, active: [gate('feat/y', 'g9')], queued: [], typical_seconds: null } }), NOW);
    expect(wagon(c, 'g9').kind).toBe('gate-run');
    expect(wagon(c, 'g9').trainId).toBeNull();
  });

  test("a gate's since as a bare date draws no elapsed and no progress; as an instant it draws both", () => {
    const dated = scene(yardOf(), statusOf({ gates: { capacity: 3, active: [{ ...gate('feat/y', 'g9'), since: '2026-09-07' }], queued: [], typical_seconds: null } }), NOW);
    expect(dated.bays[0]).toMatchObject({ elapsed: 'Sep 7, 2026', progress: 0 });
    expect(wagon(dated, 'g9').status).toBe('gating · Sep 7, 2026');
    const timed = scene(yardOf(), statusOf({ gates: { capacity: 3, active: [{ ...gate('feat/y', 'g9'), since: '2026-09-07T23:14:00Z' }], queued: [], typical_seconds: null } }), NOW);
    expect(timed.bays[0]).toMatchObject({ elapsed: '6m' });
    expect(timed.bays[0]?.progress).toBeCloseTo(6 / 12, 5);
    // Past the usual it is full, not over: the bar is a drawing scale.
    expect(scene(yardOf(), statusOf({ gates: { capacity: 3, active: [gate('feat/y', 'g9')], queued: [], typical_seconds: null } }), NOW).bays[0]?.progress).toBe(1);
  });

  test('a stale gate warns — a dead Job looks like a slow one from here, and the bay says so', () => {
    const s = scene(yardOf(), statusOf({ gates: { capacity: 3, active: [gate('feat/y', 'g9', true)], queued: [], typical_seconds: null } }), NOW);
    expect(wagon(s, 'g9').lamp).toBe('warn');
    expect(wagon(s, 'g9').tone).toBe('warn');
    expect(s.bays[0]?.stale).toBe(true);
  });

  test('more active gates than capacity widen the bays rather than hide a run', () => {
    const s = scene(
      yardOf(),
      statusOf({ gates: { capacity: 1, active: [gate('a', 'g1'), gate('b', 'g2')], queued: [], typical_seconds: null } }),
      NOW,
    );
    expect(s.bays).toHaveLength(2);
    expect(wagon(s, 'g2').slot).toBe(1);
  });
});

describe('the gate QUEUE lane — runs waiting for a bay', () => {
  // The queue is normal, not an alarm: neutral tone, no warn lamp. Three
  // busy bays with two waiting must read as a queue, never as absence.
  const queued = (branch: string, packet_id: string, position: number, over = {}) => ({
    branch,
    packet_id,
    queued_at: '2026-09-07T23:05:00Z',
    position,
    waiting_seconds: 600,
    estimated_wait_seconds: 300,
    ...over,
  });

  test('each queued run stands in the lane, in its place in line', () => {
    const s = scene(
      yardOf(),
      statusOf({
        gates: {
          capacity: 1,
          active: [gate('feat/gating', 'g1')],
          queued: [queued('feat/first', 'q1', 1), queued('feat/second', 'q2', 2, { estimated_wait_seconds: 1200 })],
          typical_seconds: 900,
        },
      }),
      NOW,
    );
    expect(wagon(s, 'q1')).toMatchObject({ station: 'gate-queue', slot: 0, since: '2026-09-07T23:05:00Z' });
    expect(wagon(s, 'q2').slot).toBe(1);
    expect(wagon(s, 'q1').status).toBe('queued · #1 in line · waiting 10m · est. ~5m');
    expect(wagon(s, 'q2').status).toContain('#2 in line');
    // The bay is still busy with its own run — a queued run takes none.
    expect(s.bays[0]?.branch).toBe('feat/gating');
  });

  test('a queue is neutral — never the warn tone a stranded green wears', () => {
    const s = scene(
      yardOf(),
      statusOf({ gates: { capacity: 3, active: [], queued: [queued('feat/x', 'q1', 1)], typical_seconds: 900 } }),
      NOW,
    );
    expect(wagon(s, 'q1').tone).toBe('static');
    expect(wagon(s, 'q1').lamp).toBe('off');
  });

  test('the queue machine says how many wait and what the front of the line expects', () => {
    const two = statusOf({
      gates: {
        capacity: 1,
        active: [gate('feat/gating', 'g1')],
        queued: [queued('feat/first', 'q1', 1), queued('feat/second', 'q2', 2)],
        typical_seconds: 900,
      },
    });
    expect(scene(yardOf(), two, NOW).machines.queue).toMatchObject({ count: 2 });
    expect(scene(yardOf(), two, NOW).machines.queue.label).toBe('2 waiting · next ~5m');
    expect(scene(yardOf(), statusOf(), NOW).machines.queue.label).toBe('clear');
  });

  test('the departure board places a queued car before the bays', () => {
    const s = scene(
      yardOf(),
      statusOf({ gates: { capacity: 1, active: [gate('feat/gating', 'g1')], queued: [queued('feat/x', 'q1', 1)], typical_seconds: null } }),
      NOW,
    );
    const rows = s.boardRows.map(r => r.where);
    expect(rows).toContain('Gate queue · #1');
    expect(rows.indexOf('Gate queue · #1')).toBeLessThan(rows.indexOf('Gate bay 1'));
  });

  test('a queued car the page can name keeps its car id, so it slides into the bay', () => {
    const c = car('c1', 'feat/x');
    const s = scene(
      yardOf({ cars: [c] }),
      statusOf({ gates: { capacity: 1, active: [], queued: [queued('feat/x', 'q1', 1)], typical_seconds: null } }),
      NOW,
    );
    expect(s.wagons.filter(w => w.branch === 'feat/x')).toHaveLength(1);
    expect(wagon(s, 'c1').station).toBe('gate-queue');
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

  // A STRUCK CAR LOOKS STRUCK (2bb0d014, 2026-09-14). Read after train
  // #361: a car a red train released carried `metadata.red_trains: 1`
  // and stood on the dock drawn exactly like a clean one. One strike is
  // the state in which the NEXT red holds the car out, so it is the
  // moment an operator can still look before it costs a second consist
  // — and the floor said nothing. The count is READ off the record the
  // conductor stamps, never inferred from a train's outcome.
  test('a dock car with one red train behind it takes the warn tone and says so', () => {
    const s = scene(
      yardOf({ dock: [car('c1', 'fix/a', { redTrains: 1 })] }),
      statusOf({ dock: [{ id: 'c1', title: 'Car c1', branch: 'fix/a', parked_since: '2026-09-07T22:00:00Z' }] }),
      NOW,
    );
    expect(wagon(s, 'c1').tone).toBe('warn');
    expect(wagon(s, 'c1').status).toBe('parked · gated green, waiting to board · 1 red train behind it');
  });

  test('two red trains pluralise; a skip reason keeps its place ahead of the count', () => {
    const s = scene(
      yardOf({ dock: [car('c1', 'fix/a', { redTrains: 2, skipReason: 'track occupied' })] }),
      statusOf(),
      NOW,
    );
    expect(wagon(s, 'c1').tone).toBe('warn');
    expect(wagon(s, 'c1').status).toBe('parked · held: track occupied · 2 red trains behind it');
  });

  test('a clean car — red_trains absent or zero — reads exactly as before', () => {
    const s = scene(
      yardOf({ dock: [car('c1', 'fix/a'), car('c2', 'fix/b', { redTrains: 0 })] }),
      statusOf(),
      NOW,
    );
    for (const id of ['c1', 'c2']) {
      expect(wagon(s, id).tone).toBe('ok');
      expect(wagon(s, id).status).toBe('parked · gated green, waiting to board');
    }
  });

  // A held car keeps its held reason and its neutral tone — a brake
  // deliberately on is still not an alarm — and gains the count, so the
  // operator reading the siding sees WHY it is held as well as THAT.
  test('a held car keeps its reason and tone and gains the red-train count', () => {
    const s = scene(
      yardOf({ dock: [car('c9', 'fix/held', { redTrains: 2 })] }),
      statusOf({ held_cars: [heldCar('c9', 'fix/held', 'held: 2 red trains')] }),
      NOW,
    );
    expect(wagon(s, 'c9')).toMatchObject({ tone: 'static', lamp: 'off' });
    expect(wagon(s, 'c9').status).toBe('held — 2 red trains · 2 red trains behind it');
  });

  // THE CASE THAT MATTERS. The dock station stops listing a held car
  // (36c3d4ca), so the client dock has NO row for it and the count above
  // had nowhere to come from — a car held out for two reds read its
  // reason sentence alone (ac80357b). The server's HeldCar now carries
  // `red_trains`, and the held wagon reads the count off that row.
  test('a held car the client dock no longer lists shows its strikes from the server row', () => {
    const s = scene(
      yardOf({ dock: [] }),
      statusOf({
        held_cars: [{ ...heldCar('c9', 'fix/held', 'needs a look before it boards again'), red_trains: 2 }],
      }),
      NOW,
    );
    expect(wagon(s, 'c9')).toMatchObject({ tone: 'static', lamp: 'off' });
    expect(wagon(s, 'c9').status).toBe(
      'held — needs a look before it boards again · 2 red trains behind it',
    );
  });

  // The server row's count is the authority when it states one — a
  // stated 0 is "clean", not "unknown", and does not fall back to the
  // client's row. Only a row WITHOUT the field (an older server) falls
  // back to the client's CarRow, and a clean car reads as before.
  test('a stated server count wins; an older server row falls back to the client row', () => {
    const stated = scene(
      yardOf({ dock: [car('c9', 'fix/held', { redTrains: 2 })] }),
      statusOf({ held_cars: [{ ...heldCar('c9', 'fix/held', 'why'), red_trains: 0 }] }),
      NOW,
    );
    expect(wagon(stated, 'c9').status).toBe('held — why');
    const older = scene(
      yardOf({ dock: [car('c9', 'fix/held', { redTrains: 1 })] }),
      statusOf({ held_cars: [heldCar('c9', 'fix/held', 'why')] }),
      NOW,
    );
    expect(wagon(older, 'c9').status).toBe('held — why · 1 red train behind it');
    const clean = scene(
      yardOf({ dock: [] }),
      statusOf({ held_cars: [heldCar('c9', 'fix/held', 'why')] }),
      NOW,
    );
    expect(wagon(clean, 'c9').status).toBe('held — why');
  });

  // THE HELD SIDING. A car an operator held cannot board, so the
  // loading-dock station row stops listing it (36c3d4ca) and the client
  // dock goes quiet about it — while the floor's `held` lane counts held
  // GATE-RUNS, a different thing entirely. It is drawn from the server's
  // own `held_cars`, on the dock where it physically stands, in the
  // neutral tone a deliberate brake earns.
  test('a held car stands on the dock in the neutral tone, with its reason', () => {
    const s = scene(
      // The client dock holds the free car only — the state after the
      // station row stops listing a held one.
      yardOf({ dock: [car('c1', 'fix/a')] }),
      statusOf({
        dock: [{ id: 'c1', title: 'Car c1', branch: 'fix/a', parked_since: '2026-09-07T22:00:00Z' }],
        held_cars: [heldCar('c9', 'fix/held', 'waiting on an operator action')],
      }),
      NOW,
    );
    expect(wagon(s, 'c9')).toMatchObject({ station: 'dock', tone: 'static', lamp: 'off' });
    expect(wagon(s, 'c9').status).toBe('held — waiting on an operator action');
    expect(wagon(s, 'c9').since).toBe(SINCE);
    // The dock machine counts what can board and names what cannot.
    expect(s.machines.dock.parked).toBe(1);
    expect(s.machines.dock.heldCars).toBe(1);
    expect(s.machines.dock.label).toContain('1 held');
  });

  // BEFORE *AND* AFTER the station row learns about holds. While the row
  // still admits a held car, the client dock lists it too — and the floor
  // must draw ONE wagon for it, on the held siding, never a parked one
  // beside a held twin.
  test('a held car the client dock still lists is one wagon, held', () => {
    const s = scene(
      yardOf({ dock: [car('c9', 'fix/held')] }),
      statusOf({ held_cars: [heldCar('c9', 'fix/held', 'waiting on an operator action')] }),
      NOW,
    );
    expect(s.wagons.filter(w => w.branch === 'fix/held')).toHaveLength(1);
    expect(wagon(s, 'c9').tone).toBe('static');
    expect(s.machines.dock.parked).toBe(0);
  });

  // An operator's marker often opens with its own "held:" — the wagon
  // already says held, so the reason is what follows it. Same reading the
  // held-GREEN lane uses; one function, not a second.
  test('a held car does not say held twice', () => {
    const s = scene(
      yardOf(),
      statusOf({ held_cars: [heldCar('c9', 'fix/held', 'held: rolls the dev pod')] }),
      NOW,
    );
    expect(wagon(s, 'c9').status).toBe('held — rolls the dev pod');
  });

  test('a parked car being re-gated is ONE wagon, in the bay, keeping its car id', () => {
    const c = car('c1', 'fix/a');
    const s = scene(
      yardOf({ dock: [c], cars: [c] }),
      statusOf({ gates: { capacity: 3, active: [gate('fix/a', 'g1')], queued: [], typical_seconds: null } }),
      NOW,
    );
    expect(s.wagons.filter(w => w.branch === 'fix/a')).toHaveLength(1);
    expect(wagon(s, 'c1').station).toBe('gate');
  });

  test('a red gate puts the car in the garage with the check that failed', () => {
    const s = scene(
      yardOf({ cars: [car('c1', 'fix/red')] }),
      statusOf({ garage: [garaged('fix/red', 'g1', 'clippy, test')] }),
      NOW,
    );
    const w = wagon(s, 'c1');
    expect(w.station).toBe('garage');
    expect(w.tone).toBe('red');
    expect(w.lamp).toBe('err');
    expect(w.status).toContain('clippy, test');
    // No excerpt on the receipt: the status ends at the check, and the
    // wagon makes no `why` claim — an older receipt reads as before.
    expect(w.status).toBe('garaged · red gate (clippy, test) — rework');
    expect(w.why ?? null).toBeNull();
  });

  test('a red gate with an excerpt says what the assertion said, on the wagon and in its tooltip', () => {
    // 6730dccb: "a troubled packet must look troubled, and the reason
    // is one field away" — the server's `failed_line` rides the status
    // line after the check, and the tooltip carries it whole.
    const line = "thread 'refuses_while_legacy' panicked at crates/core/boss-jobs/src/yard.rs:9:5:";
    const s = scene(
      yardOf({ cars: [car('c1', 'fix/red')] }),
      statusOf({ garage: [garaged('fix/red', 'g1', 'test', line)] }),
      NOW,
    );
    const w = wagon(s, 'c1');
    expect(w.station).toBe('garage');
    expect(w.status).toBe(`garaged · red gate (test) — ${line} — rework`);
    expect(w.why).toBe(line);
  });

  test('a garaged branch with no car stands in the garage under its gate-run packet', () => {
    // The garage lane has no age bound and needs no stand-in pass: the
    // approach IS this lane now, so a branch whose last verdict is weeks
    // old draws from the same row as a fresh one — under the packet id
    // the lane names, which is a packet an operator can open.
    const s = scene(
      yardOf(),
      statusOf({ garage: [garaged('fix/old-red', 'g-old')] }),
      NOW,
    );
    const w = wagon(s, 'g-old');
    expect(w.station).toBe('garage');
    expect(w.status).toContain('run died outside a check');
    expect(s.machines.garage.count).toBe(1);
  });

  test('a lost verdict stands at the gate exit, not in the garage — the change was never judged', () => {
    const s = scene(
      yardOf(),
      statusOf({ limbo: [unjudged('fix/lost', 'g1')] }),
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
      yardOf({ publishing: [publishRow('p1', 'feat/pub', { note: 'emp-david' })] }),
      statusOf({
        stranded: [strandedGreen('feat/stranded', 'g1')],
        held: [heldGreen('feat/held', 'g2', 'waiting on #240')],
      }),
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
      yardOf(),
      statusOf({ held: [heldGreen('feat/h', 'g3', 'held: rolls the dev pod')] }),
      NOW,
    );
    expect(wagon(twice, 'g3').status).toBe('held — rolls the dev pod');
    expect(s.wagons.filter(w => w.station === 'approach').map(w => w.slot)).toEqual([0, 1, 2]);
    expect(s.machines.approach.label).toBe('1 stranded · 1 publishing · 1 held');
  });

  test('a green gate-run the server does not strand draws NO wagon, however fresh the packet', () => {
    // THE PHANTOM CAR, 2026-09-10: a green stamped `rerailed_to` stood
    // here as a gated-green wagon all day, because the floor's approach
    // was derived from the page's own gate-run window with a weaker
    // notion of "spent" than boss-jobs' (CLAUDE.md §9a). The floor now
    // asks the server and nothing else — the packets ride along for the
    // signals panel, and the empty lane is the empty siding.
    const rerailed = {
      id: 'f802558d', kind: 'gate-run', title: 'Gate: feat/x', status: 'closed',
      opened_on: '2026-09-07',
      metadata: { branch: 'feat/x', sha: 'a'.repeat(40), rerailed_to: 'feat/x-rerail' },
      steps: [{ title: 'Record the receipt', status: 'completed', metadata: { verdict: 'green' } }],
    };
    const s = scene(yardOf({ packets: { trains: [], gateRuns: [rerailed] } }), statusOf(), NOW);
    expect(s.wagons).toEqual([]);
    expect(s.machines.approach.label).toBe('clear');
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

  test('each stage of the line is a stage index over PR · CI · merge · deploy · converge · arrived · proven', () => {
    expect(STAGES).toEqual(['PR', 'CI', 'merge', 'deploy', 'converge', 'arrived', 'proven']);
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
          { id: 't1', title: 'PR train 2026-09-07 23:37', phase: 'awaiting-ci', at_step: 'CI', block: { kind: 'ci-red', checks: 'clippy' }, ci_result: 'failing', pr_url: null, car_count: 1, channel: null, boarded_at: null, eta: { kind: 'unknown', reason: 'not under test' } },
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

  // The conductor stamps the train's channel — the heaviest of its
  // cars' — at board, and the server row carries it (cffef553,
  // 2026-09-15). The locomotive names it ('data train') so a reader can
  // tell a config-only train from a software one without opening every
  // car. An old train, or no server row, is null: drawn as nothing.
  test("a locomotive carries the server row's channel; an unstamped train carries none", () => {
    const row = (channel: 'data' | null) =>
      ({ id: 't1', title: 'PR train 2026-09-07 23:37', phase: 'awaiting-ci', at_step: 'CI', block: null, ci_result: null, pr_url: null, car_count: 2, channel, boarded_at: null, eta: { kind: 'unknown', reason: 'not under test' } }) as const;
    const yard = yardOf({ inFlight: [trainRow('t1', 'BOARDED', { cars: [car('c1', 'fix/a'), car('c2', 'fix/b')] })] });
    expect(scene(yard, statusOf({ trains: [row('data')] }), NOW).locos[0]?.channel).toBe('data');
    expect(scene(yard, statusOf({ trains: [row(null)] }), NOW).locos[0]?.channel).toBeNull();
    expect(scene(yard, statusOf(), NOW).locos[0]?.channel).toBeNull();
  });

  // The two render sites, pinned at source in the yard-page-*.test.ts
  // idiom: the map's locomotive names its channel off the loco, the
  // page's train card off the server row — both as '<channel> train',
  // both guarded so an unstamped train draws nothing.
  test("the locomotive and the train card name the channel as '<channel> train', guarded", () => {
    const strip = (s: string) => s.replace(/<!--[\s\S]*?-->/g, '');
    const map = strip(readFileSync(join(import.meta.dir, 'YardMap.svelte'), 'utf8'));
    expect(map).toMatch(/\{#if l\.channel\}\s*<text[^>]*class="plate">\{l\.channel\} train<\/text>/);
    const page = strip(readFileSync(join(import.meta.dir, 'YardPage.svelte'), 'utf8'));
    expect(page).toMatch(/\{@const channel = serverTrainById\.get\(t\.id\)\?\.channel \?\? null\}/);
    expect(page).toMatch(/\{#if channel\}\s*<span class="yard-chip"[^>]*>\{channel\} train<\/span>/);
  });

  test("a car aboard is 'since' the server's boarded_at, else the conductor's boarding minute read off the train title", () => {
    expect(wagon(aboard('BOARDED'), 'c1').since).toBe('2026-09-07T23:37:00.000Z');
    const served = scene(
      yardOf({ inFlight: [trainRow('t1', 'BOARDED', { cars: [car('c1', 'fix/a')] })] }),
      statusOf({
        trains: [{ id: 't1', title: 'PR train 2026-09-07 23:37', phase: 'awaiting-ci', at_step: 'ci', block: null, ci_result: null, pr_url: null, car_count: 1, channel: null, boarded_at: '2026-09-07T23:37:12Z', eta: { kind: 'unknown', reason: 'not under test' } }],
      }),
      NOW,
    );
    expect(wagon(served, 'c1').since).toBe('2026-09-07T23:37:12Z');
  });
});

describe('the signals along the track', () => {
  test('no train: every lamp off', () => {
    expect(scene(yardOf(), statusOf(), NOW).signals).toEqual(['off', 'off', 'off', 'off', 'off', 'off', 'off']);
  });

  test('the lead train lights the signals: passed green, current pulsing, blocked red', () => {
    const s = scene(yardOf({ inFlight: [trainRow('t1', 'DEPARTED')] }), statusOf(), NOW);
    expect(s.signals).toEqual(['ok', 'ok', 'ok', 'now', 'off', 'off', 'off']);
    const b = scene(
      yardOf({ inFlight: [trainRow('t1', 'BOARDED', { lamp: 'failing', trouble: { kind: 'ci-red' } })] }),
      statusOf(),
      NOW,
    );
    expect(b.signals).toEqual(['ok', 'err', 'off', 'off', 'off', 'off', 'off']);
  });

  test('with two trains open, the one furthest along leads', () => {
    const s = scene(
      yardOf({ inFlight: [trainRow('t1', 'BOARDED'), trainRow('t2', 'CONVERGING')] }),
      statusOf(),
      NOW,
    );
    expect(s.signals).toEqual(['ok', 'ok', 'ok', 'ok', 'now', 'off', 'off']);
  });

  // THE PROVEN SIGNAL IS NOT A TRAIN'S. A locomotive's furthest stage is
  // `arrived`; proving is per CAR, downstream of every train, so the last
  // lamp is lit by the inspection shed instead of by the lead loco.
  test('the last lamp is the shed: pulsing while a car is inspected, red when a probe failed', () => {
    const probed = car('p1', 'feat/probed', { proof: proofOf({ probe: 'bash x.sh', expect: 'ok' }) });
    const s = scene(yardOf({ awaitingProof: [probed] }), statusOf(), NOW);
    expect(s.signals[6]).toBe('now');
    const failed = car('p2', 'feat/failed', {
      proof: proofOf({
        probe: 'bash y.sh',
        attempt: {
          at: '2026-09-07T22:00:00Z',
          exit: 1,
          host: 'forge',
          stdout: null,
          stderr: 'boom',
          why: 'it exited 1',
          missingTools: [],
          notYet: false,
        },
      }),
    });
    expect(scene(yardOf({ awaitingProof: [failed] }), statusOf(), NOW).signals[6]).toBe('err');
    // A siding is not the shed working: nothing is running there.
    const evented = car('p3', 'feat/evented', { proof: proofOf({ event: 'the next train' }) });
    expect(scene(yardOf({ awaitingProof: [evented] }), statusOf(), NOW).signals[6]).toBe('off');
  });
});

// THE INSPECTION SHED AND ITS TWO SIDINGS — the floor's last stage.
// Before this, `arrived` was the end of the map while the page's own
// subtitle promised PROVEN, and the only surface for a probe was one
// counter tile. Every value below is a field the packet already carries.
describe('the inspection shed', () => {
  const probe = (over: Partial<CarProof> = {}) =>
    proofOf({ probe: 'bash infra/lint/x.sh --self-test', expect: 'X-OK', ...over });

  test('an arrived car with a probe stands in the shed, showing the command and the string', () => {
    const c = car('c1', 'feat/probed', { proof: probe() });
    const s = scene(yardOf({ awaitingProof: [c] }), statusOf(), NOW);
    expect(wagon(s, 'c1')).toMatchObject({
      station: 'inspection-shed',
      slot: 0,
      tone: 'ok',
      lamp: 'working',
      probe: { command: 'bash infra/lint/x.sh --self-test', expect: 'X-OK' },
    });
    expect(wagon(s, 'c1').status).toBe('inspection shed · no probe run in the packets read');
  });

  test('the queued run-car-probe request is the wagon\'s line, read off the ops-requests the page fetched', () => {
    const c = car('c1', 'feat/probed', { proof: probe() });
    const req = {
      id: 'r1',
      kind: 'ops-request',
      title: 'run the recorded probe',
      status: 'open',
      opened_on: '2026-09-07',
      metadata: { verb: 'run-car-probe', car: 'c1', opened_at: '2026-09-07T23:10:00Z' },
    } as const;
    const s = scene(yardOf({ awaitingProof: [c] }), statusOf(), NOW, { ...NO_FEEDS, probes: [req] });
    expect(wagon(s, 'c1').status).toBe('inspection shed · probe queued on the forge');
  });

  test('a car waiting on an event stands on the first siding, one carrying no probe on the second', () => {
    const ev = car('c2', 'feat/evented', { proof: proofOf({ event: 'the next train in flight' }) });
    const bare = car('c3', 'feat/bare', { proof: null });
    const s = scene(yardOf({ awaitingProof: [ev, bare] }), statusOf(), NOW);
    expect(wagon(s, 'c2')).toMatchObject({ station: 'siding-event', slot: 0, tone: 'static', lamp: 'off', probe: null });
    expect(wagon(s, 'c2').status).toBe('siding · waiting on an event, no probe can settle it');
    expect(wagon(s, 'c3')).toMatchObject({ station: 'siding-no-probe', slot: 0, tone: 'static', lamp: 'off' });
    expect(wagon(s, 'c3').status).toBe('siding · no probe recorded');
  });

  // ONE BRANCH, ONE WAGON. A car in the shed has landed, so its train is
  // in the arrivals window and the arrivals stack would claim it too.
  // The shed is downstream of arrivals, so the shed wins — and the
  // wagon keeps the train link and the train's own arrival instant,
  // which is when it entered the shed.
  test('a car in the shed is not also in the arrivals stack', () => {
    const c = car('c1', 'feat/probed', { proof: probe() });
    const s = scene(
      yardOf({ awaitingProof: [c], arrivals: [arrivedWith('t9', '2026-09-07T23:00:00Z', c)] }),
      statusOf(),
      NOW,
    );
    expect(s.wagons.filter(w => w.id === 'c1')).toHaveLength(1);
    expect(wagon(s, 'c1')).toMatchObject({
      station: 'inspection-shed',
      trainId: 't9',
      since: '2026-09-07T23:00:00Z',
    });
    // It left the arrivals yard, so it is no longer one of the day's
    // landed wagons there.
    expect(s.machines.arrivals.landed).toBe(0);
  });

  test('a car still aboard a moving train is on the track, never in the shed', () => {
    const c = car('c1', 'feat/probed', { proof: probe() });
    const s = scene(
      yardOf({ awaitingProof: [c], inFlight: [trainRow('t1', 'CONVERGING', { cars: [c] })] }),
      statusOf(),
      NOW,
    );
    expect(s.wagons.filter(w => w.id === 'c1')).toHaveLength(1);
    expect(wagon(s, 'c1').station).toBe('train');
  });

  test('the shed machine counts the three places and says clear when empty', () => {
    const s = scene(
      yardOf({
        awaitingProof: [
          car('c1', 'feat/probed', { proof: probe() }),
          car('c2', 'feat/evented', { proof: proofOf({ event: 'a train' }) }),
          car('c3', 'feat/bare', { proof: null }),
        ],
      }),
      statusOf(),
      NOW,
    );
    expect(s.machines.inspection).toEqual({
      label: '1 inspecting · 1 on an event · 1 with no probe',
      inspecting: 1,
      failed: 0,
      notYet: 0,
      onEvent: 1,
      noProbe: 1,
      flakes: [],
      flakeLabel: 'no flakes in the runs read',
    });
    expect(scene(yardOf(), statusOf(), NOW).machines.inspection).toEqual({
      label: 'clear',
      inspecting: 0,
      failed: 0,
      notYet: 0,
      onEvent: 0,
      noProbe: 0,
      flakes: [],
      flakeLabel: 'no flakes in the runs read',
    });
  });

  // Backlog 36cc4913: the shed lists reds-that-were-flakes by check,
  // read off the gate-run packets the page holds — the same stamps
  // `boss orient`'s FLAKES line counts.
  test('the shed machine carries the flake tally off the gate-runs read', () => {
    const flaked: JobLite = {
      id: 'g1',
      kind: 'gate-run',
      title: 'Gate: fix/x',
      status: 'closed',
      opened_on: '2026-09-18',
      metadata: { branch: 'fix/x', sha: 'abc', flake_of: 'p1', flaky_checks: ['test'] },
    };
    const s = scene(yardOf({ packets: { trains: [], gateRuns: [flaked] } }), statusOf(), NOW);
    expect(s.machines.inspection.flakes).toEqual([{ check: 'test', count: 1 }]);
    expect(s.machines.inspection.flakeLabel).toBe('flakes · test: 1');
  });

  test('the board names each place and does not call an unproven car landed', () => {
    const s = scene(
      yardOf({
        awaitingProof: [
          car('c1', 'feat/probed', { proof: probe() }),
          car('c2', 'feat/evented', { proof: proofOf({ event: 'a train' }) }),
          car('c3', 'feat/bare', { proof: null }),
        ],
      }),
      statusOf(),
      NOW,
    );
    const rows = s.boardRows.filter(r => ['c1', 'c2', 'c3'].includes(r.id));
    expect(rows.map(r => [r.where, r.landed])).toEqual([
      ['Inspection shed', false],
      ['Siding · on an event', false],
      ['Siding · no probe', false],
    ]);
  });

  test('selecting the shed is a plain selection the map and the panel share', () => {
    expect(parseSelection('inspection-shed')).toEqual({ kind: 'inspection-shed' });
  });

  // LEAVE STAMPED. The `proven` step completing is the transition: the
  // car drops out of `awaitingProof`, so the wagon leaves the shed, and
  // the arrivals stack says it is proven rather than merely arrived.
  test('a stamped car stands in the arrivals yard and reads proven', () => {
    const c = car('c9', 'feat/done', {
      proof: proofOf({ probe: 'bash x.sh', stamped: { at: '2026-09-07T23:05:00Z', by: 'run-car-probe' } }),
    });
    const s = scene(yardOf({ arrivals: [arrivedWith('t9', '2026-09-07T23:00:00Z', c)] }), statusOf(), NOW);
    expect(wagon(s, 'c9').station).toBe('arrivals');
    expect(wagon(s, 'c9').status).toBe('landed in b641f3a · proven');
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
    expect(rows[0]?.where).toBe('Arrivals · software');
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

  // FOUR SIDINGS BY DELIVERY CHANNEL (design c6bd173e, car 1 — backlog
  // 953aaf30). A landed wagon stands on the siding of its car's
  // `delivery_channel`, in the order data · config · software · infra,
  // and its slot counts along THAT siding. Landing itself is still the
  // converge for every channel (car 3 gives each its own evidence).
  test('a landed car stands on the siding of its channel, slotted along that siding', () => {
    const on = (id: string, at: string, ch: CarRow['deliveryChannel']) =>
      trainRow(id, 'ARRIVED', {
        live: false,
        outcome: 'arrived',
        mergeRef: 'b641f3a',
        arrivedAt: { ms: Date.parse(at), at, basis: 'completed_at' },
        cars: [car(`${id}-c`, `fix/${id}`, { deliveryChannel: ch })],
      });
    const s = scene(
      yardOf({
        arrivals: [
          on('t9', '2026-09-07T23:00:00Z', 'software'),
          on('t8', '2026-09-07T22:00:00Z', 'data'),
          on('t7', '2026-09-07T21:00:00Z', 'infra'),
          on('t6', '2026-09-07T20:00:00Z', 'config'),
          on('t5', '2026-09-07T19:00:00Z', 'software'),
        ],
      }),
      statusOf(),
      NOW,
    );
    expect(wagon(s, 't8-c')).toMatchObject({ station: 'arrivals', siding: 'data', slot: 0 });
    expect(wagon(s, 't6-c')).toMatchObject({ station: 'arrivals', siding: 'config', slot: 0 });
    expect(wagon(s, 't9-c')).toMatchObject({ station: 'arrivals', siding: 'software', slot: 0 });
    expect(wagon(s, 't5-c')).toMatchObject({ station: 'arrivals', siding: 'software', slot: 1 });
    expect(wagon(s, 't7-c')).toMatchObject({ station: 'arrivals', siding: 'infra', slot: 0 });
    // The board names the siding, newest first across all four.
    expect(s.boardRows.filter(r => r.landed).map(r => [r.id, r.where])).toEqual([
      ['t9-c', 'Arrivals · software'],
      ['t8-c', 'Arrivals · data'],
      ['t7-c', 'Arrivals · infra'],
      ['t6-c', 'Arrivals · config'],
      ['t5-c', 'Arrivals · software'],
    ]);
  });

  test('an old car — parked before the stamp existed — lands on the software siding', () => {
    // Built through the one constructor from a packet carrying no
    // `delivery_channel`, the way every car before the stamp reads.
    const old = carRow({ id: 'old', kind: 'ship-a-change', title: 'Old car', metadata: { branch: 'fix/old' } });
    const s = scene(yardOf({ arrivals: [arrivedWith('t1', '2026-09-07T23:00:00Z', old)] }), statusOf(), NOW);
    expect(wagon(s, 'old')).toMatchObject({ station: 'arrivals', siding: 'software', slot: 0 });
  });

  test('the drawn cap is per siding — a full software siding hides no data wagon', () => {
    const at = (i: number) => new Date(Date.parse('2026-09-07T23:00:00Z') - i * 60_000).toISOString();
    const arrivals = [
      ...Array.from({ length: ARRIVALS_DRAWN + 2 }, (_, i) => landed(`s${i}`, at(i))),
      trainRow('d', 'ARRIVED', {
        live: false,
        outcome: 'arrived',
        arrivedAt: { ms: Date.parse(at(20)), at: at(20), basis: 'completed_at' },
        cars: [car('d-c', 'fix/d', { deliveryChannel: 'data' })],
      }),
    ];
    const { drawn, hidden } = drawnWagons(scene(yardOf({ arrivals }), statusOf(), NOW).wagons);
    expect(drawn.some(w => w.id === 'd-c')).toBe(true);
    expect(drawn.filter(w => w.siding === 'software')).toHaveLength(ARRIVALS_DRAWN);
    expect(hidden).toBe(2);
  });

  // EACH SIDING LANDS ON ITS OWN EVIDENCE (design c6bd173e, car 3 —
  // edae6e8b). The server judges a car on ITS channel's evidence and
  // the floor draws that judgement: a config car whose manifests have
  // not applied stands on its siding CONVERGING — its train arrived,
  // but its change is not live — with the evidence it waits for named;
  // a landed one carries its evidence on the status line. Measured
  // 2026-09-15: boss-gcp converged 14 minutes after the image roll, so
  // an infra car really does stand converging after its train arrives.
  test('a landed car is judged on its siding row — converging until its own evidence exists', () => {
    const at = '2026-09-07T23:00:00Z';
    const arrivals = [
      trainRow('t1', 'ARRIVED', {
        live: false,
        outcome: 'arrived',
        mergeRef: 'b641f3a',
        arrivedAt: { ms: Date.parse(at), at, basis: 'completed_at' },
        cars: [
          car('cfg', 'fix/manifest', { deliveryChannel: 'config' }),
          car('inf', 'fix/talos', { deliveryChannel: 'infra' }),
          car('sw', 'fix/crate', { deliveryChannel: 'software', proof: proofOf({ stamped: { at: at, by: 'x' } }) }),
          car('old', 'fix/old', { deliveryChannel: 'data' }),
        ],
      }),
    ];
    const status = statusOf({
      sidings: [
        {
          id: 'cfg',
          branch: 'fix/manifest',
          train: 't1',
          channel: 'config',
          landing: { kind: 'landed', evidence: 'manifests applied and verified at b641f3a', at: '2026-09-07T22:55:00Z' },
        },
        {
          id: 'inf',
          branch: 'fix/talos',
          train: 't1',
          channel: 'infra',
          landing: { kind: 'converging', awaiting: 'host converge on b641f3a: boss-gcp — last reported 1111111' },
        },
        {
          id: 'sw',
          branch: 'fix/crate',
          train: 't1',
          channel: 'software',
          landing: { kind: 'landed', evidence: 'the cluster jobs API self-reports b641f3a', at },
        },
        { id: 'old', branch: 'fix/old', train: 't1', channel: 'data', landing: { kind: 'unread', why: 'the window begins after this merge' } },
      ],
    });
    const s = scene(yardOf({ arrivals }), status, NOW);
    // Landed on its own evidence: the evidence rides the status line,
    // the wagon reads settled.
    expect(wagon(s, 'cfg')).toMatchObject({ station: 'arrivals', siding: 'config', tone: 'ok', lamp: 'ok' });
    expect(wagon(s, 'cfg').status).toBe('landed in b641f3a · manifests applied and verified at b641f3a · arrived');
    expect(wagon(s, 'cfg').since).toBe('2026-09-07T22:55:00Z');
    // Not yet: on its siding, converging, the awaited evidence named.
    expect(wagon(s, 'inf')).toMatchObject({ station: 'arrivals', siding: 'infra', tone: 'warn', lamp: 'working' });
    expect(wagon(s, 'inf').status).toBe('converging · awaiting host converge on b641f3a: boss-gcp — last reported 1111111');
    // The proof stamp still reads after the evidence.
    expect(wagon(s, 'sw').status).toBe('landed in b641f3a · the cluster jobs API self-reports b641f3a · proven');
    // Unread is not converging: the wagon reads landed by its train, as
    // before the lane, and says the evidence was not read.
    expect(wagon(s, 'old')).toMatchObject({ station: 'arrivals', siding: 'data', tone: 'ok', lamp: 'ok' });
    expect(wagon(s, 'old').status).toBe('landed in b641f3a · arrived · data evidence unread');
    // Converging wagons are still on the board's landed rows: the
    // siding is where they stand — but the day's LANDED count excludes
    // them (three of the four cars landed).
    expect(s.boardRows.find(r => r.id === 'inf')).toMatchObject({ landed: true, where: 'Arrivals · infra' });
    expect(s.machines.arrivals.landed).toBe(3);
  });

  test('without a siding row — an older server — a landed car reads as it did before the lane', () => {
    const s = scene(
      yardOf({ arrivals: [arrivedWith('t1', '2026-09-07T23:00:00Z', car('c', 'fix/c', { deliveryChannel: 'config' }))] }),
      statusOf(),
      NOW,
    );
    expect(wagon(s, 'c').status).toBe('landed in b641f3a · arrived');
    expect(wagon(s, 'c')).toMatchObject({ tone: 'ok', lamp: 'ok', siding: 'config' });
  });

  // The EARLIER half of the claim: a config car's manifests apply in the
  // converge run that closes minutes before the conductor stamps the
  // image roll, so a car ABOARD a converging train can already be live.
  // It stays coupled (the consist is the train's) and its status line
  // says so, with the evidence.
  test('a car aboard a converging train that has landed on its own evidence says so', () => {
    const c = car('cfg', 'fix/manifest', { deliveryChannel: 'config' });
    const status = statusOf({
      sidings: [
        {
          id: 'cfg',
          branch: 'fix/manifest',
          train: 't1',
          channel: 'config',
          landing: { kind: 'landed', evidence: 'manifests applied and verified at b641f3a', at: '2026-09-07T22:55:00Z' },
        },
      ],
    });
    const s = scene(yardOf({ inFlight: [trainRow('t1', 'CONVERGING', { cars: [c] })] }), status, NOW);
    expect(wagon(s, 'cfg')).toMatchObject({ station: 'train', lamp: 'ok', tone: 'ok' });
    expect(wagon(s, 'cfg').status).toBe('aboard #259 · converge · landed on config: manifests applied and verified at b641f3a');
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

// THE CANCELLED SIDING (design c6bd173e, outcomes): a withdrawn car —
// its `abandoned` terminal completed — is one of the three terminal
// tracks, beside arrivals and the inspection shed. It is drawn, not
// listed on the departure board: a withdrawn car is neither in flight
// nor landed, and the board's two counts are exactly those. Struck and
// left-behind are dock badges, not a track.
describe('the cancelled siding', () => {
  const withdrawn = (id: string, at: string | null, over: Partial<CarRow> = {}) => ({
    car: car(id, `fix/${id}`, over),
    at,
  });

  test('a withdrawn car stands on the cancelled siding, since the instant it was abandoned', () => {
    const s = scene(
      yardOf({ withdrawn: [withdrawn('w1', '2026-09-07T22:00:00Z'), withdrawn('w2', '2026-09-07T21:00:00Z')] }),
      statusOf(),
      NOW,
    );
    expect(wagon(s, 'w1')).toMatchObject({ station: 'cancelled', slot: 0, tone: 'static', lamp: 'off', since: '2026-09-07T22:00:00Z' });
    expect(wagon(s, 'w1').status).toBe('withdrawn · abandoned');
    expect(wagon(s, 'w2').slot).toBe(1);
    expect(wagon(s, 'w1').siding).toBeUndefined();
    expect(s.boardRows.map(r => r.id)).toEqual([]);
    expect(s.machines.cancelled).toEqual({ label: '2 withdrawn', count: 2 });
  });

  test('a withdrawn car whose packet names no branch is still a wagon, named by its id', () => {
    // The six abandoned cars in the record on 2026-09-15 all read
    // `branch: null` — filed as cars, withdrawn before they had one.
    const s = scene(yardOf({ withdrawn: [withdrawn('0123456789abcdef', null, { branch: '' })] }), statusOf(), NOW);
    expect(wagon(s, '0123456789abcdef')).toMatchObject({ station: 'cancelled', tag: '01234567', since: null });
  });

  test('an empty siding says so, and the selection is one the map and the panel share', () => {
    expect(scene(yardOf(), statusOf(), NOW).machines.cancelled).toEqual({ label: 'empty', count: 0 });
    expect(parseSelection('cancelled')).toEqual({ kind: 'cancelled' });
  });

  test('a car both landed and withdrawn keeps its arrival — one branch, one wagon', () => {
    const c = car('c1', 'fix/c1');
    const s = scene(
      yardOf({ arrivals: [arrivedWith('t1', '2026-09-07T23:00:00Z', c)], withdrawn: [withdrawn('c1', '2026-09-07T23:30:00Z')] }),
      statusOf(),
      NOW,
    );
    expect(s.wagons.filter(w => w.id === 'c1')).toHaveLength(1);
    expect(wagon(s, 'c1').station).toBe('arrivals');
  });
});

describe('the departure board', () => {
  test('in flight first, along the line; landed below', () => {
    const s = scene(
      yardOf({
        cars: [car('gc', 'feat/gating')],
        publishing: [publishRow('p1', 'feat/pub')],
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
      statusOf({
        gates: { capacity: 3, active: [gate('feat/gating', 'g1')], queued: [], typical_seconds: null },
        limbo: [unjudged('feat/lost', 'l1')],
        garage: [garaged('feat/red', 'r1')],
      }),
      NOW,
    );
    expect(s.boardRows.map(r => [r.id, r.where])).toEqual([
      ['p1', 'Approach'],
      ['gc', 'Gate bay 1'],
      ['l1', 'Gate exit'],
      ['d1', 'Dock · slot 1'],
      ['r1', 'Garage'],
      ['a1', 'Track · #259 at CI'],
      ['z1', 'Arrivals · software'],
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

  test('a dock the station queue could not serve reads as NO READING, never as empty', () => {
    // `empty` is a claim, and an unavailable station queue is no
    // evidence for it — the same posture the protocols lint takes when
    // the registry is unreachable: skip loudly rather than pass. A
    // rollback to an image that cannot deserialize the loading-dock row
    // lands here, so the one thing the lane must not do is look calm.
    const unread = scene(
      yardOf({ dockStation: { source: 'unavailable' } }),
      statusOf({ boarding: { ...statusOf().boarding, next_board: 'boards on the next tick' } }),
      NOW,
    );
    expect(unread.machines.dock.label).toBe('no reading · the station queue did not serve');
    expect(unread.machines.dock.parked).toBe(0);
    expect(unread.machines.dock.lamp).toBe('off');
    // The held lane is a SEPARATE read (the server's status), so what it
    // holds is still stated beside the missing reading.
    const withHeld = scene(
      yardOf({ dockStation: { source: 'unavailable' } }),
      statusOf({ held_cars: [{ id: 'h1', branch: 'feat/h', title: 'Held car', parked_since: '2026-09-07T22:00:00Z', reason: 'waiting on a person' }] }),
      NOW,
    );
    expect(withHeld.machines.dock.label).toBe('no reading · the station queue did not serve · 1 held');
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
      probes: null,
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
      // The step it stands at names its status beside its title: a
      // perfect-tense title alone reads as done (648a68a9).
      { lamp: 'working', what: 'Open for review', when: null, note: 'ready, not yet done' },
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

  // A REFUSAL IS NOT A RED STOP (backlog ff5b9634). gate.sh exits 2
  // and writes `verdict: refused` when it declined to judge at all,
  // with `refused_because` in the refusing component's own words. The
  // journey has to draw that the way it draws a `lost` run — a warning
  // that this leg produced no evidence — rather than the red that says
  // the author has something to fix.
  test('a refused receipt warns; it judged nothing about the branch', () => {
    const receipt = JSON.stringify({
      verdict: 'refused', head: 'abcdef0', mode: 'full',
      refused_because: 'pre-flight lint a-car-stays-under-the-edit-level could not answer',
      checks: [{ name: 'a-car-stays-under-the-edit-level', result: 'refused', seconds: 0 }],
    });
    const job = { steps: [{ spec_slug: 'gate', title: 'Gate', status: 'completed', completed_at: '2026-09-22T09:00:00Z', metadata: { receipt } }] };
    expect(journeyStops(job)).toEqual([
      { lamp: 'warn', what: 'Gate', when: '2026-09-22T09:00:00Z', note: 'refused · abcdef0' },
    ]);
  });

  // THE WIDE RECEIPT. The gate runner used to reduce gate.sh's account
  // of a run to {verdict, head, mode, fails} before reporting it; it now
  // reports the whole receipt, whose `checks` array carries every check
  // with its result and duration and has no derived `fails` beside it
  // (a summary living twice in one document is a fact that can drift —
  // CLAUDE.md §9a). Both shapes are on real cars right now: old receipts
  // sit on every landed car and must keep reading.
  test('a wide receipt names what failed from `checks`', () => {
    const receipt = JSON.stringify({
      verdict: 'failed', head: 'abcdef0', mode: 'full', dirty: false, free_gb: 91,
      checks: [
        { name: 'fmt', result: 'pass', seconds: 3 },
        { name: 'clippy', result: 'fail', seconds: 44 },
        { name: 'test', result: 'fail', seconds: 812 },
      ],
    });
    const job = { steps: [{ spec_slug: 'gate', title: 'Gate', status: 'completed', completed_at: '2026-09-09T10:00:00Z', metadata: { receipt } }] };
    expect(journeyStops(job)).toEqual([
      { lamp: 'err', what: 'Gate', when: '2026-09-09T10:00:00Z', note: 'failed · abcdef0 · clippy, test' },
    ]);
  });

  test('an old four-field receipt still reads — landed cars carry them', () => {
    const receipt = JSON.stringify({ verdict: 'failed', head: 'abcdef0', mode: 'full', fails: ['clippy'] });
    const job = { steps: [{ spec_slug: 'gate', title: 'Gate', status: 'completed', completed_at: '2026-09-09T10:00:00Z', metadata: { receipt } }] };
    expect(journeyStops(job)).toEqual([
      { lamp: 'err', what: 'Gate', when: '2026-09-09T10:00:00Z', note: 'failed · abcdef0 · clippy' },
    ]);
  });

  test('a green wide receipt names no failure', () => {
    const receipt = JSON.stringify({
      verdict: 'green', head: 'b4f38151234',
      checks: [{ name: 'fmt', result: 'pass', seconds: 3 }],
    });
    const job = { steps: [{ spec_slug: 'gate', title: 'Gate', status: 'completed', completed_at: '2026-09-09T10:00:00Z', metadata: { receipt } }] };
    expect(journeyStops(job)).toEqual([
      { lamp: 'ok', what: 'Gate', when: '2026-09-09T10:00:00Z', note: 'green · b4f3815' },
    ]);
  });
});
