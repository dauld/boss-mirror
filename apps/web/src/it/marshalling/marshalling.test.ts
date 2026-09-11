import { describe, expect, test } from 'bun:test';
import {
  constraintOf,
  drainHours,
  joinSidings,
  longestWaits,
  parseQueueAge,
  parseStationFlow,
  parseStationLoad,
  waitText,
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
  test('a payload that is not the expected shape parses to nothing, never to a guess', () => {
    expect(parseStationLoad({ data: 'nope' })).toEqual([]);
    expect(parseStationLoad(null).length).toBe(0);
    const flow = parseStationFlow({ nope: true });
    expect(flow.rows.length).toBe(0);
    expect(flow.windowHours).toBeNull();
  });

  test('the window comes off the envelope, never assumed by the caller', () => {
    expect(parseStationFlow(FLOW).windowHours).toBe(24);
    expect(parseStationFlow(FLOW).asOf).toBe('2026-09-09T04:00:00Z');
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

  test('a payload that is not the expected shape parses to nothing', () => {
    expect(parseQueueAge({ data: 7 }).waits).toEqual([]);
    expect(parseQueueAge(null).now).toBeNull();
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
