import { describe, expect, test } from 'bun:test';
import {
  ageBand,
  ageDays,
  arrivalsByDay,
  channelOf,
  daysEndingOn,
  failedStep,
  holderOf,
  inboundKinds,
  loadEveryPage,
  loadKind,
  parseJobsPage,
  readings,
  takenIn,
  waiting,
  type InboundRow,
} from './receiving';

const WORKFLOWS = [
  { kind: 'backlog-item', category: 'platform', status: 'active' },
  { kind: 'user-feedback', category: 'platform', status: 'active' },
  { kind: 'design-doc', category: 'platform', status: 'active' },
  { kind: 'maintenance-backup', category: 'platform', status: 'active' },
  { kind: 'pr-train', category: 'platform', status: 'active' },
  { kind: 'gate-run', category: 'platform', status: 'active' },
  { kind: 'ship-a-change', category: 'platform', status: 'active' },
  { kind: 'ops-request', category: 'platform', status: 'active' },
  { kind: 'payroll-run', category: 'finance', status: 'active' },
  { kind: 'learning', category: 'platform', status: 'retired' },
];

function job(over: Record<string, unknown>): Record<string, unknown> {
  return {
    id: '0123456789abcdef',
    kind: 'backlog-item',
    title: 'A thing',
    status: 'open',
    opened_on: '2026-09-11',
    closed_on: null,
    priority: 'standard',
    metadata: {},
    steps: [
      { kind: 'trigger', status: 'completed', assignee_id: null },
      { kind: 'task', status: 'ready', assignee_id: 'claude@algedonic.dev' },
    ],
    ...over,
  };
}

describe('which kinds are inbound', () => {
  test('platform kinds that are neither chores nor the delivery pipeline, active only', () => {
    expect(inboundKinds(WORKFLOWS)).toEqual(['backlog-item', 'design-doc', 'user-feedback']);
  });
});

describe('channel — a recorded fact first, a derived reading otherwise', () => {
  test('metadata.channel is the fact when it names a channel', () => {
    expect(channelOf(job({ metadata: { channel: 'monitoring' } }))).toEqual({
      channel: 'monitoring',
      basis: 'recorded',
    });
  });
  test('user-feedback is the feedback channel by kind', () => {
    expect(channelOf(job({ kind: 'user-feedback' }))).toEqual({
      channel: 'feedback',
      basis: 'derived',
    });
  });
  test('a machine reporter is monitoring; a session actor is a session', () => {
    expect(channelOf(job({ metadata: { reporter: 'cadence.silence.sweep' } })).channel).toBe(
      'monitoring',
    );
    expect(channelOf(job({ metadata: { reporter: 'conductor' } })).channel).toBe('monitoring');
    expect(channelOf(job({ metadata: { reporter: 'automation:cluster-watchdog' } })).channel).toBe(
      'monitoring',
    );
    expect(channelOf(job({ metadata: { reporter: 'claude@algedonic.dev' } })).channel).toBe(
      'session',
    );
    expect(channelOf(job({ metadata: { filed_by: 'the builder of fix/x' } })).channel).toBe(
      'session',
    );
  });
  test('design and protocol kinds are their own channels; an incident is monitoring', () => {
    expect(channelOf(job({ kind: 'design-doc' })).channel).toBe('design');
    expect(channelOf(job({ kind: 'protocol-retro' })).channel).toBe('design');
    expect(channelOf(job({ kind: 'incident' })).channel).toBe('monitoring');
    expect(channelOf(job({ kind: 'rotate-a-credential' })).channel).toBe('protocol');
    expect(channelOf(job({ kind: 'publish-to-github' })).channel).toBe('protocol');
  });
  test('a backlog item naming no source is unrecorded, never guessed', () => {
    expect(channelOf(job({}))).toEqual({ channel: 'unrecorded', basis: 'derived' });
  });
  test('an unknown metadata.channel value does not pass as a fact', () => {
    expect(channelOf(job({ metadata: { channel: 'carrier-pigeon' } }))).toEqual({
      channel: 'unrecorded',
      basis: 'derived',
    });
  });
});

describe('the page is parsed once', () => {
  test('rows carry what the tracks and the manifest need, and total is read off the envelope', () => {
    const page = parseJobsPage({ data: [job({})], total: 41 });
    expect(page.total).toBe(41);
    expect(page.rows).toHaveLength(1);
    const r = page.rows[0];
    expect(r?.id).toBe('0123456789abcdef');
    expect(r?.openedOn).toBe('2026-09-11');
    expect(r?.ready).toEqual([{ kind: 'task', who: 'claude@algedonic.dev', failed: null }]);
    expect(r?.channel).toBe('unrecorded');
  });
  // A failed verb answer (backlog 074e1287): jobs.complete_linked_step
  // leaves the step OPEN and writes the verb's last FAILED line on it
  // as `failed` with the alert it filed. Publish 254177e2's open-pr sat
  // ready for five hours on 2026-09-19 and this yard drew it like any
  // ready step — the packet was troubled and did not look it.
  test('a ready step a failed verb annotated carries the FAILED line and its alert', () => {
    const line = 'publish-github-pr: FAILED — pushing publish/2026-09-18: dubious ownership';
    const page = parseJobsPage({
      data: [
        job({
          kind: 'publish-to-github',
          steps: [
            { kind: 'approval', status: 'completed', assignee_id: 'emp-david' },
            {
              kind: 'task',
              status: 'ready',
              assignee_id: null,
              metadata: {
                ops_verb: 'publish-github-pr',
                failed: line,
                failed_exit: '1',
                failed_source: 'c98a782f-0000-4000-8000-000000000000',
                alert: 'a1e57000-0000-4000-8000-000000000000',
              },
            },
          ],
        }),
      ],
      total: 1,
    });
    const r = page.rows[0];
    expect(r?.ready).toEqual([
      {
        kind: 'task',
        who: null,
        failed: {
          line,
          exit: '1',
          source: 'c98a782f-0000-4000-8000-000000000000',
          alert: 'a1e57000-0000-4000-8000-000000000000',
        },
      },
    ]);
    if (!r) throw new Error('fixture parsed to nothing');
    expect(failedStep(r)?.failed?.line).toBe(line);
    // A healthy packet has no failed step — the yard says nothing.
    const healthy = parseJobsPage({ data: [job({})], total: 1 }).rows[0];
    if (!healthy) throw new Error('fixture parsed to nothing');
    expect(failedStep(healthy)).toBeNull();
  });
  test('a malformed envelope is an empty page with total 0, not a throw', () => {
    expect(parseJobsPage(null)).toEqual({ rows: [], total: 0 });
  });
});

// THE PARTITION (design 62de32ae decision 4): a packet stands in
// receiving until its intake step completes, and is marshalling's after
// — the rule `regions.rs::taken_in` draws the server's line with.
describe('receiving holds a packet until its intake step completes', () => {
  const parsed = (steps: ReadonlyArray<Record<string, unknown>>): InboundRow => {
    const r = parseJobsPage({ data: [job({ steps })], total: 1 }).rows[0];
    if (!r) throw new Error('fixture parsed to nothing');
    return r;
  };
  const untriaged = parsed([
    { kind: 'trigger', status: 'completed' },
    { kind: 'task', status: 'ready' },
  ]);
  const triaged = parsed([
    { kind: 'trigger', status: 'completed' },
    { kind: 'task', status: 'completed' },
    { kind: 'task', status: 'ready' },
  ]);

  test('a trigger takes nothing in; any other completed step does', () => {
    expect(untriaged.takenIn).toBe(false);
    expect(triaged.takenIn).toBe(true);
    expect(takenIn([{ kind: 'task', status: 'skipped' }])).toBe(false);
    expect(takenIn([])).toBe(false);
  });

  test('only what nothing has taken in is standing', () => {
    const standing = waiting([{ ...untriaged, id: 'u' }, { ...triaged, id: 't' }], '2026-09-20');
    expect(standing.map((r) => r.id)).toEqual(['u']);
  });
});

// A LIMIT IS NOT A FILTER (design 62de32ae decision 4): the board read
// one page of 500 and, on 2026-09-24, backlog-item had 822 in the window.
describe('every page of a kind is read, to its total', () => {
  const rows = (from: number, n: number): ReadonlyArray<InboundRow> =>
    parseJobsPage({
      data: Array.from({ length: n }, (_, i) => job({ id: `row-${from + i}` })),
      total: n,
    }).rows;
  const TOTAL = 1201;

  test('past the page, every row — and a row a shifting page repeats is kept once', async () => {
    const asked: number[] = [];
    const load: typeof loadKind = async (_kind, _days, limit, offset = 0) => {
      asked.push(offset);
      // The table moved under the read: the second page starts one row
      // early, repeating the first page's last row.
      const start = offset === 0 ? 0 : offset - 1;
      return { kind: 'ready', data: { rows: rows(start, Math.min(limit, TOTAL - start)), total: TOTAL } };
    };
    const got = await loadEveryPage('backlog-item', 8, 500, load);
    if (got.kind !== 'ready') throw new Error(`the read failed: ${JSON.stringify(got)}`);
    expect(got.data.total).toBe(TOTAL);
    expect(new Set(got.data.rows.map((r) => r.id)).size).toBe(got.data.rows.length);
    expect(got.data.rows.length).toBe(TOTAL);
    expect(asked.slice(0, 2)).toEqual([0, 500]);
  });

  test('a page that fails fails the read — half a kind is not a kind', async () => {
    const load: typeof loadKind = async (_kind, _days, limit, offset = 0) =>
      offset === 0
        ? { kind: 'ready', data: { rows: rows(0, limit), total: TOTAL } }
        : { kind: 'failed', error: 'HTTP 500' };
    const got = await loadEveryPage('backlog-item', 8, 500, load);
    expect(got).toEqual({ kind: 'failed', error: 'HTTP 500' });
  });
});

describe('who holds a standing packet', () => {
  const base = parseJobsPage({ data: [job({})], total: 1 }).rows[0];
  const row = (ready: InboundRow['ready']): InboundRow => {
    if (!base) throw new Error('fixture parsed to nothing');
    return { ...base, ready };
  };
  test('an agent, a human, nobody', () => {
    expect(holderOf(row([{ kind: 'task', who: 'claude@algedonic.dev', failed: null }]))).toEqual({
      who: 'agent',
      label: 'the agent',
    });
    expect(holderOf(row([{ kind: 'answer-question', who: 'emp-david', failed: null }]))).toEqual({
      who: 'human',
      label: 'emp-david',
    });
    expect(holderOf(row([{ kind: 'task', who: null, failed: null }]))).toEqual({
      who: 'nobody',
      label: 'unassigned',
    });
    expect(holderOf(row([]))).toEqual({ who: 'nobody', label: 'no ready step' });
  });
  test('a human on any ready step outranks the agent — the packet waits on the person', () => {
    expect(
      holderOf(
        row([
          { kind: 'task', who: 'claude@algedonic.dev', failed: null },
          { kind: 'sign-off', who: 'emp-david', failed: null },
        ]),
      ).who,
    ).toBe('human');
  });
});

describe('age', () => {
  test('days from opened_on to today, and the band it falls in', () => {
    expect(ageDays('2026-09-05', '2026-09-12')).toBe(7);
    expect(ageBand(0)).toBe('fresh');
    expect(ageBand(3)).toBe('fresh');
    expect(ageBand(4)).toBe('aging');
    expect(ageBand(14)).toBe('aging');
    expect(ageBand(15)).toBe('stale');
  });
  test('the window is the last n days ending today, oldest first', () => {
    expect(daysEndingOn('2026-09-12', 3)).toEqual(['2026-09-10', '2026-09-11', '2026-09-12']);
  });
});

describe('arrivals and departures per day', () => {
  const rows = parseJobsPage(
    {
      data: [
        job({ id: 'a', opened_on: '2026-09-11', metadata: { channel: 'monitoring' } }),
        job({ id: 'b', opened_on: '2026-09-11', kind: 'user-feedback' }),
        job({ id: 'c', opened_on: '2026-09-12', status: 'closed', closed_on: '2026-09-12' }),
        job({ id: 'd', opened_on: '2026-09-01', status: 'closed', closed_on: '2026-09-11' }),
      ],
      total: 4,
    },
  ).rows;
  test('a bar per day by channel, and the departures beside it', () => {
    const days = arrivalsByDay(rows, ['2026-09-11', '2026-09-12']);
    expect(days[0]).toEqual({
      day: '2026-09-11',
      arrived: 2,
      byChannel: { monitoring: 1, feedback: 1 },
      left: 1,
    });
    expect(days[1]).toEqual({
      day: '2026-09-12',
      arrived: 1,
      byChannel: { unrecorded: 1 },
      left: 1,
    });
  });
  test('the standing packets, oldest first, with age, band and holder', () => {
    const w = waiting(rows, '2026-09-12');
    expect(w.map((r) => r.id)).toEqual(['a', 'b']);
    expect(w[0]?.age).toBe(1);
    expect(w[0]?.band).toBe('fresh');
    expect(w[0]?.holder.who).toBe('agent');
  });
});

describe('what the snapshot says', () => {
  test('the readings name the feedback that stands, the one-actor share and the unrecorded share', () => {
    const rows = parseJobsPage(
      {
        data: [
          job({ id: 'f1', kind: 'user-feedback', opened_on: '2026-08-22' }),
          job({ id: 'f2', kind: 'user-feedback', opened_on: '2026-09-10' }),
          job({ id: 'b1', opened_on: '2026-09-11' }),
          job({
            id: 'b2',
            opened_on: '2026-09-11',
            steps: [{ kind: 'task', status: 'ready', assignee_id: 'emp-david' }],
          }),
          job({ id: 'b3', opened_on: '2026-09-01', status: 'closed', closed_on: '2026-09-11' }),
        ],
        total: 5,
      },
    ).rows;
    const r = readings(rows, '2026-09-12', ['2026-09-11', '2026-09-12']);
    expect(r.feedbackStanding).toEqual({ count: 2, oldestDays: 21 });
    expect(r.onOneActor).toEqual({ count: 3, of: 4, actor: 'the agent' });
    expect(r.unrecorded).toEqual({ count: 2, of: 2 });
  });
});
