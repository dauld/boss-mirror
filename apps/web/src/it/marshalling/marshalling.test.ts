import { describe, expect, test } from 'bun:test';
import {
  constraintOf,
  drainHours,
  joinSidings,
  longestWaits,
  overlapLine,
  parseQueueAge,
  parseStationFlow,
  parseStationLoad,
  parseStationLoadEnvelope,
  waitText,
  waitsCountLine,
  whyNotMoving,
  type Siding,
} from './marshalling';

const LOAD = {
  data: [
    {
      station: 'q.platform-admin.task',
      kind: 'constraint',
      depth: 41,
      wip_limit: null,
      over_limit: false,
      oldest_opened_on: '2026-08-22',
      oldest_age_days: 17,
      capability_roles: ['platform-admin'],
    },
    {
      station: 'q.bookkeeper.bill-approval',
      kind: 'constraint',
      depth: 6,
      wip_limit: null,
      over_limit: false,
      oldest_opened_on: '2026-09-07',
      oldest_age_days: 2,
      capability_roles: ['bookkeeper'],
    },
    {
      station: 'loading-dock',
      kind: 'batch',
      depth: 3,
      wip_limit: 24,
      over_limit: false,
      oldest_opened_on: '2026-09-08',
      oldest_age_days: 1,
      capability_roles: null,
    },
    {
      station: 'q.cto.sign-off',
      kind: 'constraint',
      depth: 0,
      wip_limit: null,
      over_limit: false,
      oldest_opened_on: null,
      oldest_age_days: null,
      capability_roles: ['cto'],
    },
  ],
  total: 4,
};

const FLOW = {
  data: [
    {
      station: 'q.platform-admin.task',
      kind: 'constraint',
      basis: 'step-events',
      arrived: 12,
      served: 0,
      net: 12,
      unavailable_reason: null,
    },
    {
      station: 'q.bookkeeper.bill-approval',
      kind: 'constraint',
      basis: 'step-events',
      arrived: 8,
      served: 6,
      net: 2,
      unavailable_reason: null,
    },
    {
      station: 'loading-dock',
      kind: 'batch',
      basis: 'unavailable',
      arrived: null,
      served: null,
      net: null,
      unavailable_reason: 'membership turns on Job metadata, which the flow cube does not carry',
    },
    {
      station: 'q.cto.sign-off',
      kind: 'constraint',
      basis: 'step-events',
      arrived: 0,
      served: 0,
      net: 0,
      unavailable_reason: null,
    },
  ],
  total: 4,
  window_hours: 24,
  since: '2026-09-08T04:00:00Z',
  as_of: '2026-09-09T04:00:00Z',
};

function sidings(): ReadonlyArray<Siding> {
  return joinSidings(parseStationLoad(LOAD), parseStationFlow(FLOW));
}

describe('parseStationLoad / parseStationFlow', () => {
  // Until 67825067 these parsed to NO ROWS, and no rows is what the
  // board paints as "Every watched station is clear." A body that is not
  // the envelope is a failed read, so the parse throws and fetchRemote
  // hands the page its failure line.
  test('a payload that is not the expected shape is refused, never read as no rows', () => {
    expect(() => parseStationLoad({ data: 'nope' })).toThrow(
      '/api/stations/load: HTTP 200, but the body is an object with no data list',
    );
    expect(() => parseStationLoad(null)).toThrow('/api/stations/load: HTTP 200, but the body is null');
    expect(() => parseStationLoadEnvelope([])).toThrow('/api/stations/load: HTTP 200, but the body is a list');
    expect(() => parseStationFlow({ nope: true })).toThrow(
      '/api/stations/flow: HTTP 200, but the body is an object with no data list',
    );
  });

  test('a well-formed envelope with no rows is the only empty', () => {
    expect(parseStationLoad({ data: [], total: 0 })).toEqual([]);
    const flow = parseStationFlow({ data: [] });
    expect(flow.rows.length).toBe(0);
    expect(flow.windowHours).toBeNull();
  });

  test('the window comes off the envelope, never assumed by the caller', () => {
    expect(parseStationFlow(FLOW).windowHours).toBe(24);
    expect(parseStationFlow(FLOW).asOf).toBe('2026-09-09T04:00:00Z');
  });
});

// Stations overlap: a packet stands at every station whose predicate
// it matches. On 2026-09-23 the sidings summed to 517 over 303
// distinct packets — all 213 agent-station packets also stood in
// q.platform-admin.task — and nothing on the board said so, so a
// reader adding the depth column overstated the work by 214 (backlog
// 140a2222). The load now carries `distinct_packets` and, per row,
// `also_elsewhere`; the board states the overlap from those.
const OVERLAPPING = {
  data: [
    { station: 'q.platform-admin.task', kind: 'constraint', depth: 297, also_elsewhere: 213 },
    { station: 'a.platform-admin.opus-5-1m', kind: 'constraint', depth: 213, also_elsewhere: 213 },
    { station: 'q.platform-admin.sign-off', kind: 'constraint', depth: 6, also_elsewhere: 0 },
    { station: 'loading-dock', kind: 'batch', depth: 1, also_elsewhere: 0 },
    { station: 'q.cto.sign-off', kind: 'constraint', depth: 0, also_elsewhere: 0 },
  ],
  total: 5,
  distinct_packets: 303,
};

describe('parseStationLoadEnvelope / overlapLine', () => {
  test('the distinct count and each row share come off the envelope', () => {
    const env = parseStationLoadEnvelope(OVERLAPPING);
    expect(env.distinctPackets).toBe(303);
    expect(env.rows.map((r) => r.alsoElsewhere)).toEqual([213, 213, 0, 0, 0]);
    // The rows are the same rows the plain parser reads.
    expect(env.rows).toEqual(parseStationLoad(OVERLAPPING));
  });

  test('an older server that sends neither field reads as unknown, never as zero', () => {
    const env = parseStationLoadEnvelope(LOAD);
    expect(env.distinctPackets).toBeNull();
    expect(env.rows.every((r) => r.alsoElsewhere === null)).toBe(true);
  });

  test('overlapping sidings state the sum, the distinct count, and where they overlap', () => {
    const line = overlapLine(parseStationLoadEnvelope(OVERLAPPING));
    expect(line).toContain('sum to 517');
    expect(line).toContain('303 distinct packets');
    expect(line).toContain('214');
    expect(line).toContain('q.platform-admin.task 213 of 297');
    expect(line).toContain('a.platform-admin.opus-5-1m 213 of 213');
    // A station that shares nothing is not named as overlapping.
    expect(line).not.toContain('sign-off');
  });

  test('sidings that share no packet say the sum IS the packet count', () => {
    const line = overlapLine(
      parseStationLoadEnvelope({
        data: [
          { station: 'a', depth: 4, also_elsewhere: 0 },
          { station: 'b', depth: 2, also_elsewhere: 0 },
        ],
        distinct_packets: 6,
      }),
    );
    expect(line).toContain('sum to 6');
    expect(line).toContain('no packet stands at two stations');
  });

  test('without a distinct count the sum is refused as a count of the work, not passed off as one', () => {
    const line = overlapLine(parseStationLoadEnvelope(LOAD));
    expect(line).toContain('sum to 50');
    expect(line).toContain('did not say how many distinct packets');
  });

  test('an empty network needs no overlap line', () => {
    expect(overlapLine(parseStationLoadEnvelope({ data: [], distinct_packets: 0 }))).toBeNull();
  });
});

describe('joinSidings', () => {
  test('carries depth from load and flow from flow, keyed by station', () => {
    const rows = sidings();
    const admin = rows.find((s) => s.station === 'q.platform-admin.task');
    expect(admin?.depth).toBe(41);
    expect(admin?.oldestAgeDays).toBe(17);
    expect(admin?.flow).toEqual({ kind: 'counted', arrived: 12, served: 0, net: 12 });
  });

  test('a station whose flow the log cannot be attributed to keeps its reason', () => {
    const dock = sidings().find((s) => s.station === 'loading-dock');
    expect(dock?.depth).toBe(3);
    expect(dock?.flow.kind).toBe('unavailable');
    expect(dock?.flow.kind === 'unavailable' && dock.flow.reason).toContain('metadata');
  });

  test('a station missing from the flow read is unavailable, not zero', () => {
    const rows = joinSidings(parseStationLoad(LOAD), { rows: [], windowHours: 24, asOf: null });
    expect(rows.every((s) => s.flow.kind === 'unavailable')).toBe(true);
  });
});

describe('drainHours', () => {
  test('depth divided by the observed service rate', () => {
    // 6 served in 24h = 0.25/h; 6 waiting clears in 24h.
    expect(drainHours(6, 6, 24)).toBe(24);
    expect(drainHours(12, 6, 24)).toBe(48);
  });

  test('a queue nothing left is not draining, and that is null rather than a big number', () => {
    expect(drainHours(41, 0, 24)).toBeNull();
  });

  test('an empty queue clears in no time at all', () => {
    expect(drainHours(0, 0, 24)).toBe(0);
    expect(drainHours(0, 5, 24)).toBe(0);
  });
});

describe('constraintOf', () => {
  test('names the queue that is not draining at all over one that merely is slow', () => {
    const c = constraintOf(sidings(), 24);
    expect(c.kind).toBe('station');
    expect(c.kind === 'station' && c.station).toBe('q.platform-admin.task');
    expect(c.kind === 'station' && c.drainHours).toBeNull();
    expect(c.because).toContain('41');
  });

  test('with every queue draining, the slowest to clear is the constraint', () => {
    const draining = sidings().map((s) =>
      s.station === 'q.platform-admin.task'
        ? { ...s, flow: { kind: 'counted' as const, arrived: 12, served: 20, net: -8 } }
        : s,
    );
    const c = constraintOf(draining, 24);
    // admin: 41 deep, 20 served -> 49.2h. bookkeeper: 6 deep, 6 served -> 24h.
    expect(c.kind === 'station' && c.station).toBe('q.platform-admin.task');
    expect(c.kind === 'station' && c.drainHours).toBeCloseTo(49.2, 1);
  });

  test('an empty network has no constraint and says so', () => {
    const empty = sidings().map((s) => ({ ...s, depth: 0 }));
    const c = constraintOf(empty, 24);
    expect(c.kind).toBe('none');
    expect(c.because).toContain('Nothing');
  });

  test('a queue whose flow is uncountable is never named the constraint', () => {
    // The dock is the only thing holding work, and its rate is unknown.
    const onlyDock = sidings().filter((s) => s.station === 'loading-dock');
    const c = constraintOf(onlyDock, 24);
    expect(c.kind).toBe('none');
    expect(c.because).toContain('loading-dock');
  });
});

describe('whyNotMoving', () => {
  test('a queue nothing left in the window says its head has not moved', () => {
    const admin = sidings().find((s) => s.station === 'q.platform-admin.task')!;
    const why = whyNotMoving(admin, 24);
    expect(why).toContain('Nothing left this queue');
    expect(why).toContain('12 arrived');
  });

  test('a draining queue says how fast, and whether it is keeping up', () => {
    const book = sidings().find((s) => s.station === 'q.bookkeeper.bill-approval')!;
    expect(whyNotMoving(book, 24)).toContain('6 served');
    expect(whyNotMoving(book, 24)).toContain('growing');
  });

  test('an uncountable queue states the clause instead of a rate', () => {
    const dock = sidings().find((s) => s.station === 'loading-dock')!;
    expect(whyNotMoving(dock, 24)).toContain('metadata');
  });

  test('an empty queue says it is clear', () => {
    const cto = sidings().find((s) => s.station === 'q.cto.sign-off')!;
    expect(whyNotMoving(cto, 24)).toContain('Clear');
  });
});

const QUEUE_AGE = {
  data: [
    {
      job_id: 'e654e223-e48d-47e6-abef-382bc897ed83',
      job_kind: 'publish-to-github',
      job_title: 'Publish to the public GitHub mirror',
      step_id: 's1',
      spec_slug: 'open-pr',
      step_title: 'Open a pull request on the public mirror',
      status: 'ready',
      assignee_id: 'claude@algedonic.dev',
      simulated: false,
      since: '2026-08-27T19:34:47Z',
      exact: false,
      waiting_seconds: 1066888,
      waiting_days: 12.348,
    },
    {
      job_id: 'cbf04be9-0000-0000-0000-000000000000',
      job_kind: 'design-doc',
      job_title: 'The gap between mostly sure and absolutely sure',
      step_id: 's2',
      spec_slug: 'fold',
      step_title: 'Fold it into current truth',
      status: 'ready',
      assignee_id: null,
      simulated: false,
      since: '2026-09-04T00:00:00Z',
      exact: true,
      waiting_seconds: 432000,
      waiting_days: 5,
    },
    {
      job_id: 'sim-0000',
      job_kind: 'wholesale-keg-order',
      job_title: 'A simulated brewery packet',
      step_id: 's3',
      spec_slug: 'pick',
      step_title: 'Pick the kegs',
      status: 'ready',
      assignee_id: null,
      simulated: true,
      since: '2025-07-24T00:00:00Z',
      exact: true,
      waiting_seconds: 99999999,
      waiting_days: 400,
    },
  ],
  total: 3,
  now: '2026-09-09T04:00:00Z',
};

describe('parseQueueAge / longestWaits', () => {
  test('parses the lens rows and reads `now` off the envelope', () => {
    const lens = parseQueueAge(QUEUE_AGE);
    expect(lens.waits.length).toBe(3);
    expect(lens.now).toBe('2026-09-09T04:00:00Z');
    expect(lens.waits[0]?.exact).toBe(false);
  });

  test('a payload that is not the expected shape is refused (67825067), not read as nothing outstanding', () => {
    expect(() => parseQueueAge({ data: 7 })).toThrow(
      '/api/jobs/queue-age: HTTP 200, but the body is an object with no data list',
    );
    expect(() => parseQueueAge(null)).toThrow('/api/jobs/queue-age: HTTP 200, but the body is null');
    expect(parseQueueAge({ data: [] }).now).toBeNull();
  });

  test('simulated obligations are not this board’s queue', () => {
    const out = longestWaits(parseQueueAge(QUEUE_AGE).waits, 10);
    expect(out.some((w) => w.simulated)).toBe(false);
  });

  test('longest first, capped', () => {
    const out = longestWaits(parseQueueAge(QUEUE_AGE).waits, 1);
    expect(out.length).toBe(1);
    expect(out[0]?.jobKind).toBe('publish-to-github');
  });
});

// The table showed 12 of 320 real obligations on 2026-09-23 and said
// neither number (backlog 18683a0a, page-audit 7c228914 gap 8): a limit
// that does not count what it leaves out reads as a filter.
describe('waitsCountLine', () => {
  const waits = parseQueueAge(QUEUE_AGE).waits;

  test('a cut table names the total and how many it leaves out', () => {
    expect(waitsCountLine(waits, 1)).toBe(
      '1 of 2 outstanding obligations, longest first · +1 more not shown · 1 on simulated or shadow packets not ranked',
    );
  });

  test('an uncut table still states its total', () => {
    expect(waitsCountLine(waits, 12)).toBe(
      '2 of 2 outstanding obligations, longest first · 1 on simulated or shadow packets not ranked',
    );
  });

  test('the simulated clause is absent when nothing simulated was left out', () => {
    const real = waits.filter((w) => !w.simulated);
    expect(waitsCountLine(real, 12)).toBe('2 of 2 outstanding obligations, longest first');
    expect(waitsCountLine(real.slice(0, 1), 12)).toBe('1 of 1 outstanding obligation, longest first');
  });

  test('the count agrees with what longestWaits shows', () => {
    const shown = longestWaits(waits, 1).length;
    expect(waitsCountLine(waits, 1).startsWith(`${shown} of `)).toBe(true);
  });
});

describe('waitText', () => {
  test('days for a long wait, hours for a short one', () => {
    const [first, second] = parseQueueAge(QUEUE_AGE).waits;
    expect(waitText(first!)).toContain('12 d');
    expect(waitText(second!)).toBe('5 d');
  });

  test('an inexact stamp is labelled a lower bound, never printed as fact', () => {
    const [first] = parseQueueAge(QUEUE_AGE).waits;
    expect(waitText(first!)).toContain('at least');
  });

  test('under a day reads in hours', () => {
    const w = { ...parseQueueAge(QUEUE_AGE).waits[1]!, waitingDays: 0.25 };
    expect(waitText(w)).toBe('6 h');
  });
});
