import { describe, expect, test } from 'bun:test';
import {
  inspectionShed,
  probeRun,
  shedCounts,
  shedLabel,
  shedLamp,
  shedPlace,
  shedStatus,
  shedTone,
} from './yard-shed';
import { readCarProof, type CarProof, type CarRow, type JobLite } from './yard';

// The inspection shed is the floor's last stage. Every state below is a
// real one read off live packets on 2026-09-11: one car with a probe and
// a failed attempt, one carrying only `proof_event`, one carrying
// nothing — one in each of the three places, all at once.

const proofOf = (over: Partial<CarProof> = {}): CarProof => ({
  probe: null,
  expect: null,
  event: null,
  attempt: null,
  stamped: null,
  ...over,
});

const car = (id: string, proof: CarProof | null): CarRow => ({
  id,
  kind: 'ship-a-change',
  branch: `feat/${id}`,
  title: `Car ${id}`,
  tags: [],
  sim: false,
  skipReason: null,
  head: 'abc1234',
  proof,
});

/** A `run-car-probe` ops-request as the dispatcher's arrival rule files
 *  it (`run-car-probes-on-train-arrived`). */
const request = (id: string, carId: string, status: 'open' | 'closed'): JobLite => ({
  id,
  kind: 'ops-request',
  title: `run the recorded probe for Car ${carId}`,
  status,
  opened_on: '2026-09-11',
  metadata: {
    verb: 'run-car-probe',
    car: carId,
    branch: `feat/${carId}`,
    host: 'forge',
    spawned_by_rule: 'run-car-probes-on-train-arrived',
    opened_at: '2026-09-11T04:01:03Z',
    ...(status === 'closed' ? { closed_at: '2026-09-11T04:01:38Z' } : {}),
  },
});

describe('readCarProof', () => {
  test('reads the four proof fields the gate and the forge write', () => {
    const j: JobLite = {
      id: 'c1',
      kind: 'ship-a-change',
      title: 'A maintenance protocol states its category',
      status: 'open',
      opened_on: '2026-09-10',
      metadata: {
        branch: 'feat/a-maintenance-protocol-states-its-category',
        proof_probe: 'bash infra/lint/x.sh --self-test',
        proof_expect: 'MAINTENANCE-PROTOCOLS-STATE-THEIR-CATEGORY',
        proof_attempt: {
          at: '2026-09-10T21:20:54Z',
          exit: 1,
          host: 'david-asus-minipc',
          output: 'jq: error — category=null',
          missing_tools: ['jq'],
          probe: 'bash infra/lint/x.sh --self-test',
          expect: 'MAINTENANCE-PROTOCOLS-STATE-THEIR-CATEGORY',
        },
      },
    };
    expect(readCarProof(j)).toEqual({
      probe: 'bash infra/lint/x.sh --self-test',
      expect: 'MAINTENANCE-PROTOCOLS-STATE-THEIR-CATEGORY',
      event: null,
      attempt: {
        at: '2026-09-10T21:20:54Z',
        exit: 1,
        host: 'david-asus-minipc',
        output: 'jq: error — category=null',
        missingTools: ['jq'],
      },
      stamped: null,
    });
  });

  test('the stamp is the proven step completing — what leaving the shed looks like', () => {
    const j: JobLite = {
      id: 'c2',
      kind: 'ship-a-change',
      title: 'A live protocol matches its file',
      status: 'closed',
      opened_on: '2026-09-11',
      metadata: { proof_probe: 'bash x.sh', proof_expect: 'self-test ok' },
      steps: [
        {
          spec_slug: 'proven',
          title: 'Proven in production',
          status: 'completed',
          metadata: { completed_at: '2026-09-11T04:01:38Z', proven_by: 'run-car-probe', proof: '{"exit":0}' },
        },
      ],
    };
    expect(readCarProof(j)?.stamped).toEqual({ at: '2026-09-11T04:01:38Z', by: 'run-car-probe' });
  });

  test('a proven step still pending is not a stamp', () => {
    const j: JobLite = {
      id: 'c3',
      kind: 'ship-a-change',
      title: 'x',
      status: 'open',
      opened_on: '2026-09-11',
      metadata: { proof_event: 'The next train in flight.' },
      steps: [{ spec_slug: 'proven', title: 'Proven in production', status: 'pending' }],
    };
    expect(readCarProof(j)).toEqual({
      probe: null,
      expect: null,
      event: 'The next train in flight.',
      attempt: null,
      stamped: null,
    });
  });

  test('a packet recording nothing about proof reads null, not a row of nulls', () => {
    expect(
      readCarProof({
        id: 'c4',
        kind: 'ship-a-change',
        title: 'x',
        status: 'open',
        opened_on: '2026-09-11',
        metadata: { branch: 'feat/x' },
      }),
    ).toBeNull();
    expect(readCarProof(null)).toBeNull();
  });
});

describe('where an unproven car stands', () => {
  test('a probe puts it in the shed', () => {
    expect(shedPlace(proofOf({ probe: 'bash x.sh', expect: 'ok' }))).toBe('inspection-shed');
  });

  test('only an event puts it on the event siding', () => {
    expect(shedPlace(proofOf({ event: 'the next train in flight' }))).toBe('siding-event');
  });

  test('neither puts it on the no-probe siding', () => {
    expect(shedPlace(proofOf())).toBe('siding-no-probe');
    expect(shedPlace(null)).toBe('siding-no-probe');
    expect(shedPlace(undefined)).toBe('siding-no-probe');
  });

  // THE DECISION, PINNED. A car can record both a probe and an event,
  // and the floor keeps one wagon per branch, so the two cannot both
  // hold it. The probe wins: it is the half a machine can settle, and a
  // car whose probe can run is not waiting on anybody.
  test('a car carrying both stands in the shed, never on both', () => {
    const both = proofOf({ probe: 'bash x.sh', expect: 'ok', event: 'and also a train' });
    expect(shedPlace(both)).toBe('inspection-shed');
    const shed = inspectionShed([car('a', both)], null);
    expect(shed).toHaveLength(1);
    expect(shed[0]?.place).toBe('inspection-shed');
  });

  test('an empty probe string is no probe — the field exists, it says nothing', () => {
    expect(shedPlace(proofOf({ probe: '' }))).toBe('siding-no-probe');
  });
});

describe('the run the forge drains', () => {
  const proof = proofOf({ probe: 'bash x.sh', expect: 'ok' });

  test('an open run-car-probe for this car is queued, with the packet and its instant', () => {
    expect(probeRun('a', proof, [request('r1', 'a', 'open')])).toEqual({
      kind: 'queued',
      id: 'r1',
      at: '2026-09-11T04:01:03Z',
    });
  });

  test("a request for another car is not this car's run", () => {
    expect(probeRun('a', proof, [request('r1', 'b', 'open')])).toEqual({ kind: 'waiting' });
  });

  test('a converge ops-request is not a probe run', () => {
    const converge: JobLite = {
      id: 'r9',
      kind: 'ops-request',
      title: 'converge on forge',
      status: 'open',
      opened_on: '2026-09-11',
      metadata: { verb: 'converge', host: 'forge' },
    };
    expect(probeRun('a', proof, [converge])).toEqual({ kind: 'waiting' });
  });

  test('a drained request leaves the attempt the forge wrote back', () => {
    const ran = proofOf({
      probe: 'bash x.sh',
      expect: 'ok',
      attempt: { at: '2026-09-10T21:20:54Z', exit: 1, host: 'david-asus-minipc', output: 'boom', missingTools: [] },
    });
    expect(probeRun('a', ran, [request('r1', 'a', 'closed')])).toEqual({
      kind: 'ran',
      at: '2026-09-10T21:20:54Z',
      exit: 1,
      host: 'david-asus-minipc',
      output: 'boom',
      missingTools: [],
    });
  });

  test('an open request outranks an attempt — the live fact is the retry', () => {
    const ran = proofOf({
      probe: 'bash x.sh',
      attempt: { at: '2026-09-10T21:20:54Z', exit: 1, host: null, output: null, missingTools: [] },
    });
    expect(probeRun('a', ran, [request('r1', 'a', 'open')]).kind).toBe('queued');
  });

  // A LIMIT IS NOT A FILTER. The page reads a window of ops-requests;
  // "no request in the rows read" is not "no request exists", and the
  // wagon's line must not claim the stronger of the two.
  test('no rows read at all is waiting, not "no request filed"', () => {
    expect(probeRun('a', proof, null)).toEqual({ kind: 'waiting' });
    expect(
      shedStatus({
        car: car('a', proof),
        place: 'inspection-shed',
        probe: 'bash x.sh',
        expect: 'ok',
        event: null,
        run: { kind: 'waiting' },
      }),
    ).toBe('inspection shed · no probe run in the packets read');
  });
});

describe('the shed as a whole', () => {
  const probed = car('a', proofOf({ probe: 'bash a.sh', expect: 'A-OK' }));
  const failed = car(
    'b',
    proofOf({
      probe: 'bash b.sh',
      expect: 'B-OK',
      attempt: { at: '2026-09-10T21:20:54Z', exit: 1, host: 'forge', output: 'boom', missingTools: [] },
    }),
  );
  const evented = car('c', proofOf({ event: 'the next train in flight' }));
  const bare = car('d', null);

  test('every awaiting-proof car lands in exactly one place, input order kept', () => {
    const shed = inspectionShed([probed, failed, evented, bare], [request('r1', 'a', 'open')]);
    expect(shed.map(s => [s.car.id, s.place])).toEqual([
      ['a', 'inspection-shed'],
      ['b', 'inspection-shed'],
      ['c', 'siding-event'],
      ['d', 'siding-no-probe'],
    ]);
    expect(shed[0]?.run).toEqual({ kind: 'queued', id: 'r1', at: '2026-09-11T04:01:03Z' });
    expect(shed[1]?.run.kind).toBe('ran');
  });

  test('a wagon in the shed carries the probe command and the string it must print', () => {
    const shed = inspectionShed([probed], null);
    expect(shed[0]).toMatchObject({ probe: 'bash a.sh', expect: 'A-OK' });
  });

  test('a wagon on the event siding carries the event it waits for', () => {
    expect(inspectionShed([evented], null)[0]).toMatchObject({
      probe: null,
      expect: null,
      event: 'the next train in flight',
    });
  });

  test('the counts and the label are the three places, nothing invented', () => {
    const shed = inspectionShed([probed, failed, evented, bare], null);
    expect(shedCounts(shed)).toEqual({ inspecting: 2, failed: 1, onEvent: 1, noProbe: 1 });
    expect(shedLabel(shedCounts(shed))).toBe('2 inspecting · 1 probe failed · 1 on an event · 1 with no probe');
  });

  test('an empty shed says clear', () => {
    expect(shedLabel(shedCounts([]))).toBe('clear');
    expect(shedCounts([])).toEqual({ inspecting: 0, failed: 0, onEvent: 0, noProbe: 0 });
  });

  test('each place states itself in one line', () => {
    const lines = inspectionShed([probed, failed, evented, bare], [request('r1', 'a', 'open')]).map(shedStatus);
    expect(lines).toEqual([
      'inspection shed · probe queued on the forge',
      'inspection shed · probe failed, exit 1',
      'siding · waiting on an event, no probe can settle it',
      'siding · no probe recorded',
    ]);
  });

  test('a probe that exited zero without a stamp says exactly that', () => {
    const odd = car(
      'e',
      proofOf({
        probe: 'bash e.sh',
        attempt: { at: '2026-09-10T21:20:54Z', exit: 0, host: null, output: 'A-OK', missingTools: [] },
      }),
    );
    const shed = inspectionShed([odd], null);
    const only = shed[0];
    expect(only && shedStatus(only)).toBe('inspection shed · probe exit 0, not stamped');
    expect(shedCounts(shed).failed).toBe(0);
  });

  test('a failed probe draws red; a siding is a holding track, not an alarm', () => {
    const shed = inspectionShed([probed, failed, evented, bare], null);
    expect(shed.map(shedTone)).toEqual(['ok', 'red', 'static', 'static']);
    expect(shed.map(shedLamp)).toEqual(['working', 'err', 'off', 'off']);
  });
});
