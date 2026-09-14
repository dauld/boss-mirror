import { afterEach, describe, expect, test } from 'bun:test';
import {
  assignmentPacket,
  fetchMyDay,
  filterByProtocol,
  protocolCounts,
  needsAPerson,
  splitQueues,
  type AssignmentRow,
} from './assignments';

type RowOverrides = Omit<Partial<AssignmentRow>, 'step'> & {
  step?: Partial<AssignmentRow['step']>;
};

function row(over: RowOverrides): AssignmentRow {
  return {
    job_id: 'j1',
    job_title: 'Fix the kettle',
    due_on: null,
    workflow: 'field-service',
    subject_kind: 'asset',
    subject_id: 'SYS-1',
    priority: 'standard',
    ...over,
    step: {
      id: Math.random().toString(36).slice(2),
      job_id: 'j1',
      kind: 'task',
      title: 'Do it',
      status: 'ready',
      assignee_id: null,
      ...(over.step ?? {}),
    },
  } as AssignmentRow;
}

describe('splitQueues', () => {
  test('partitions mine / up-for-grabs / in-flight-elsewhere', () => {
    const rows = [
      row({ step: { assignee_id: 'me' } }),
      row({ step: { assignee_id: null } }),
      row({ step: { assignee_id: 'them', status: 'active' } }),
    ];
    const q = splitQueues(rows, 'me');
    expect(q.mine.length).toBe(1);
    expect(q.upForGrabs.length).toBe(1);
    expect(q.inFlightElsewhere.length).toBe(1);
  });

  test('urgent sorts above standard within a queue', () => {
    const q = splitQueues(
      [
        row({ priority: 'standard', step: { assignee_id: 'me' } }),
        row({ priority: 'urgent', step: { assignee_id: 'me' } }),
      ],
      'me',
    );
    expect(q.mine[0]?.priority).toBe('urgent');
  });

  test('a due date outranks no due date at equal priority', () => {
    const q = splitQueues(
      [
        row({ step: { assignee_id: 'me' } }),
        row({ due_on: '2026-08-13', step: { assignee_id: 'me' } }),
      ],
      'me',
    );
    expect(q.mine[0]?.due_on).toBe('2026-08-13');
  });
});

// David, 2026-08-16: "a special separation between jobs that are in a
// queue with a human-only policy with jobs that agents are also
// eligible for as a practical consideration."
describe('the human/agent separation', () => {
  test('an agent-completion step is kept out of the claimable queue', () => {
    const q = splitQueues(
      [
        row({ step: { assignee_id: null, completion: 'human' } }),
        row({ step: { assignee_id: null, kind: 'demand-gate', completion: 'agent' } }),
      ],
      'me',
    );
    // The point of the split: a person scanning "up for grabs" must
    // not be offered a step the dispatcher is supposed to execute.
    // Claiming one is how a protocol silently becomes manual.
    expect(q.upForGrabs.length).toBe(1);
    expect(q.upForGrabs[0]?.step.completion).toBe('human');
    expect(q.notMineToDo.length).toBe(1);
    expect(q.notMineToDo[0]?.step.kind).toBe('demand-gate');
  });

  test('nothing is dropped — the two buckets partition the unclaimed', () => {
    const rows = [
      row({ step: { assignee_id: null, completion: 'human' } }),
      row({ step: { assignee_id: null, completion: 'agent' } }),
      row({ step: { assignee_id: null, completion: 'child-job' } }),
      row({ step: { assignee_id: null, completion: 'external' } }),
      row({ step: { assignee_id: null, completion: 'auto-on-materialize' } }),
    ];
    const q = splitQueues(rows, 'me');
    expect(q.upForGrabs.length + q.notMineToDo.length).toBe(rows.length);
    // Only `human` is a person's job; every other contract completes
    // by some mechanism that is not somebody reading a queue.
    expect(q.upForGrabs.length).toBe(1);
  });

  test('an unknown contract reads as human, not as agent-eligible', () => {
    // A tenant protocol can name a step kind this deployment has not
    // registered, and the server sends null. Erring toward "a person
    // should look at this" costs a glance; erring the other way files
    // real work under "an agent will get to it" and it stalls.
    for (const completion of [undefined, null]) {
      expect(needsAPerson(row({ step: { assignee_id: null, completion } }))).toBe(true);
    }
    const q = splitQueues([row({ step: { assignee_id: null } })], 'me');
    expect(q.upForGrabs.length).toBe(1);
    expect(q.notMineToDo.length).toBe(0);
  });

  test('a claimed agent step stays with its claimant, not in the automation list', () => {
    // The split only ever partitions UNCLAIMED rows. Somebody already
    // holding an agent-completion step is mid-flight on it, and moving
    // it out from under them would lose the assignment in the UI.
    const q = splitQueues(
      [row({ step: { assignee_id: 'me', completion: 'agent' } })],
      'me',
    );
    expect(q.mine.length).toBe(1);
    expect(q.notMineToDo.length).toBe(0);
  });
});

describe('assignmentPacket', () => {
  test('maps a row onto the packet-card grammar', () => {
    const p = assignmentPacket(row({}));
    expect(p.id).toBe('j1');
    expect(p.kind).toBe('field-service');
    expect(p.title).toBe('Fix the kettle');
    expect(p.branch).toBe('Do it');
    expect(p.tags).toEqual([]);
    expect(p.sim).toBe(false);
    expect(p.skipReason).toBeNull();
  });

  test('non-standard priority and due date ride as tag chips', () => {
    const p = assignmentPacket(row({ priority: 'urgent', due_on: '2026-08-13' }));
    expect(p.tags).toEqual(['urgent', 'due 2026-08-13']);
  });

  test('a not-yet-actionable step is tagged blocked', () => {
    const p = assignmentPacket(row({ step: { status: 'pending' } }));
    expect(p.tags).toEqual(['blocked']);
    expect(assignmentPacket(row({ step: { status: 'active' } })).tags).toEqual([]);
  });

  // fb3b5ce1 (2026-09-14): built through the yard's one constructor
  // now, not a literal with a fixed field list. The assignment row is a
  // projection without the packet's metadata (boss-jobs port.rs
  // AssignmentRow), so the packet-record facts it does not carry read
  // as ABSENT here — no head, no proof — the same answer the
  // constructor gives a car outside the yard's window, never
  // `undefined`. The strike count is the one packet fact the row DOES
  // carry since d6e53a35: a builder's own struck car read clean on My
  // Day while the yard drew it struck, and the builder is the one who
  // can act on a strike before the next red holds the car out.
  test('the card carries every field the yard\'s constructor sets', () => {
    const p = assignmentPacket(row({}));
    expect(p.redTrains).toBe(0);
    expect(p.head).toBeNull();
    expect(p.proof).toBeNull();
  });

  test('a struck car\'s row paints the same strike count the yard shows', () => {
    expect(assignmentPacket(row({ red_trains: 2 })).redTrains).toBe(2);
    expect(assignmentPacket(row({ red_trains: 1 })).redTrains).toBe(1);
    // A stated 0 and an older server that states nothing both read clean.
    expect(assignmentPacket(row({ red_trains: 0 })).redTrains).toBe(0);
    expect(assignmentPacket(row({})).redTrains).toBe(0);
  });

  test('a simulated packet is marked SIM in the personal queue too', () => {
    expect(assignmentPacket(row({ simulated: true })).sim).toBe(true);
    // Pre-column packets carry the tag instead — the same fallback the
    // yard uses, so one packet cannot read sim in one lens and real in
    // the other.
    expect(assignmentPacket(row({ tags: ['sim'] })).sim).toBe(true);
    expect(assignmentPacket(row({ simulated: false, tags: ['hotfix'] })).sim).toBe(false);
  });
});

describe('protocol filtering', () => {
  const queues = {
    mine: [row({ workflow: 'approval' }), row({ workflow: 'ship-a-change' })],
    upForGrabs: [row({ workflow: 'approval' })],
    notMineToDo: [row({ workflow: 'demand-forecast' })],
    inFlightElsewhere: [row({ workflow: 'user-feedback' })],
    // A protocol living ONLY in verdicts — the queue the tally forgot
    // when d598681f added it (audited 2026-08-20).
    verdicts: [
      row({ workflow: 'correction-cycle', step: { assignee_id: 'me', kind: 'sign-off' } }),
    ],
  };

  test('counts every protocol across all five queues, busiest first', () => {
    expect(protocolCounts(queues)).toEqual([
      { workflow: 'approval', count: 2 },
      { workflow: 'correction-cycle', count: 1 },
      // `demand-forecast` is only in notMineToDo — a chip that skipped
      // that queue would send the reader to an empty-looking filter
      // that then renders a row.
      { workflow: 'demand-forecast', count: 1 },
      { workflow: 'ship-a-change', count: 1 },
      { workflow: 'user-feedback', count: 1 },
    ]);
  });

  test('a verdicts-only protocol gets a chip, and All agrees with the page', () => {
    // Before the fix a sign-off docket had no chip at all, and All (N)
    // disagreed with the cards by exactly the verdict rows.
    const counts = protocolCounts(queues);
    expect(counts.find((c) => c.workflow === 'correction-cycle')?.count).toBe(1);
    const allChip = counts.reduce((n, c) => n + c.count, 0);
    const rendered =
      queues.verdicts.length +
      queues.mine.length +
      queues.upForGrabs.length +
      queues.notMineToDo.length +
      queues.inFlightElsewhere.length;
    expect(allChip).toBe(rendered);
  });

  test('no queues means no chips, rather than a crash', () => {
    expect(protocolCounts(null)).toEqual([]);
  });

  test('null filter is every row; a protocol keeps only its own', () => {
    expect(filterByProtocol(queues.mine, null)).toHaveLength(2);
    expect(filterByProtocol(queues.mine, 'approval')).toHaveLength(1);
  });

  test('a stale chip selection empties the list instead of widening it', () => {
    // The chip for a protocol that has since drained must not read as
    // "no filter" — that would silently show everything at the moment
    // the operator believes they are looking at one protocol.
    expect(filterByProtocol(queues.mine, 'retired-protocol')).toHaveLength(0);
  });
});

// The 2026-08-20 audit: a server failure must never render as "you
// have no work". The fetch edge says which one happened; the page
// decides what stays on screen.
describe('fetchMyDay is explicit about failure', () => {
  const realFetch = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = realFetch;
  });

  test('a 2xx answer carries the split queues', async () => {
    globalThis.fetch = (async (_input: RequestInfo | URL) =>
      new Response(
        JSON.stringify({ data: [row({ step: { assignee_id: 'me' } })] }),
        { status: 200 },
      )) as typeof fetch;
    const res = await fetchMyDay('me', 'brewer');
    expect(res.kind).toBe('ready');
    if (res.kind === 'ready') expect(res.queues.mine).toHaveLength(1);
  });

  test('a non-2xx is an error carrying its status, not an empty day', async () => {
    globalThis.fetch = (async (_input: RequestInfo | URL) =>
      new Response('down', { status: 502 })) as typeof fetch;
    expect(await fetchMyDay('me', 'brewer')).toEqual({
      kind: 'error',
      status: 502,
    });
  });

  test('the network failing outright is an error too, status 0', async () => {
    globalThis.fetch = (async (_input: RequestInfo | URL): Promise<Response> => {
      throw new TypeError('fetch failed');
    }) as typeof fetch;
    expect(await fetchMyDay('me', 'brewer')).toEqual({ kind: 'error', status: 0 });
  });
});

// d598681f, accepted 2026-08-19: verdicts split from owned work.
import { isVerdict } from './assignments';

describe('verdicts split from owned work', () => {
  const row = (kind: string, completion?: 'human' | 'agent') =>
    ({
      job_id: 'j1', job_title: 't', workflow: 'w', subject_kind: 'custom',
      subject_id: 's', priority: 'standard',
      step: { id: kind, job_id: 'j1', kind, title: 't', status: 'ready',
              assignee_id: 'me', completion: completion ?? 'human' },
    }) as never;

  test('sign-offs and reviews are verdicts; tasks are owned work', () => {
    expect(isVerdict(row('sign-off'))).toBe(true);
    expect(isVerdict(row('review-design'))).toBe(true);
    expect(isVerdict(row('correction-verdict'))).toBe(true);
    expect(isVerdict(row('task'))).toBe(false);
    expect(isVerdict(row('checklist'))).toBe(false);
  });

  test('an agent-completed kind is never a verdict for a person', () => {
    expect(isVerdict(row('sign-off', 'agent'))).toBe(false);
  });

  test('the registry flag outranks the client roster both ways', () => {
    // A new decision kind the roster has never heard of: the server
    // says decides, the client believes it.
    const flagged = row('novel-verdict') as { step: { decision_shaped?: boolean } };
    flagged.step.decision_shaped = true;
    expect(isVerdict(flagged as never)).toBe(true);
    // And the server saying "not a decision" beats a roster kind.
    const unflagged = row('sign-off') as { step: { decision_shaped?: boolean } };
    unflagged.step.decision_shaped = false;
    expect(isVerdict(unflagged as never)).toBe(false);
  });

  test('splitQueues partitions assigned rows into verdicts and mine', () => {
    const q = splitQueues([row('sign-off'), row('task')], 'me');
    expect(q.verdicts.map((r) => r.step.kind)).toEqual(['sign-off']);
    expect(q.mine.map((r) => r.step.kind)).toEqual(['task']);
  });
});
