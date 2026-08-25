import { afterEach, describe, expect, test } from 'bun:test';
import {
  arrivalMedians,
  arrivalReport,
  arrivalStamp,
  assembleYard,
  disciplineLabel,
  etaPhase,
  fetchYard,
  outcomeText,
  trainEta,
  trainOutcome,
  trainStatus,
  ciLamp,
  isSim,
  protocolHue,
  wipAdvisory,
  dockUpstream,
  NO_MEDIANS,
  PROTOCOL_PALETTE,
  type JobLite,
  type StationQueueEnvelope,
} from './yard';

function train(over: Partial<JobLite>): JobLite {
  return {
    id: 't1', kind: 'pr-train', title: 'PR train', status: 'open',
    opened_on: '2026-08-12', metadata: {}, steps: [], ...over,
  };
}

const s = (slug: string, status: string, metadata: Record<string, unknown> = {}) =>
  ({ spec_slug: slug, title: slug, status, metadata });

describe('trainStatus', () => {
  test('walks BOARDING → BOARDED → DEPARTED → ARRIVED', () => {
    expect(trainStatus(train({ steps: [s('pr', 'ready')] }))).toBe('BOARDING');
    expect(trainStatus(train({ steps: [s('pr', 'completed')] }))).toBe('BOARDED');
    expect(
      trainStatus(train({ steps: [s('pr', 'completed'), s('merged', 'completed')] })),
    ).toBe('DEPARTED');
    expect(
      trainStatus(
        train({ steps: [s('merged', 'completed'), s('deployed', 'completed')] }),
      ),
    ).toBe('ARRIVED');
    expect(trainStatus(train({ status: 'closed' }))).toBe('ARRIVED');
  });
});

describe('ciLamp', () => {
  test('reads the ci step result; pending until a verdict exists', () => {
    expect(ciLamp(train({ steps: [s('ci', 'ready')] }))).toBe('pending');
    expect(ciLamp(train({ steps: [s('ci', 'completed', { result: 'green' })] }))).toBe('green');
    expect(ciLamp(train({ steps: [s('ci', 'completed', { result: 'failing' })] }))).toBe('failing');
  });
});

describe('assembleYard', () => {
  const ships: JobLite[] = [
    { id: 'c1', kind: 'ship-a-change', title: 'A car', status: 'open',
      opened_on: '2026-08-12', metadata: { branch: 'feat/a' },
      steps: [s('review', 'ready')] },
    { id: 'c2', kind: 'ship-a-change', title: 'Boarded car', status: 'open',
      opened_on: '2026-08-12', metadata: { branch: 'feat/b', train: 't1' },
      steps: [s('review', 'completed')] },
  ];
  test('dock holds only parked, unboarded cars; consists join by id', () => {
    const y = assembleYard(
      [train({ metadata: { boarded_jobs: ['c2'] }, steps: [s('pr', 'completed')] })],
      ships,
    );
    expect(y.dock.map(c => c.id)).toEqual(['c1']);
    expect(y.inFlight[0]?.cars[0]?.branch).toBe('feat/b');
    expect(y.inFlight[0]?.live).toBe(true);
  });
  test('closed trains that arrived are arrivals, never live', () => {
    const y = assembleYard(
      [train({ id: 't9', status: 'closed', metadata: { outcome: 'arrived' } })],
      [],
    );
    expect(y.arrivals.length).toBe(1);
    expect(y.arrivals[0]?.live).toBe(false);
  });
  test('packet cards carry protocol, tags, and sim through both queues', () => {
    const y = assembleYard(
      [train({ metadata: { boarded_jobs: ['c2'] }, steps: [s('pr', 'completed')] })],
      ships,
    );
    expect(y.dock[0]?.kind).toBe('ship-a-change');
    expect(y.dock[0]?.sim).toBe(false);
    expect(y.inFlight[0]?.cars[0]?.kind).toBe('ship-a-change');
  });
});

// ---------------------------------------------------------------------------
// The dock as a registry-backed lens (stations.md): when the station
// endpoint serves, the envelope is authoritative — membership AND
// order come from the server, and the header shows the station's own
// facts (discipline, advisory WIP verdict).
// ---------------------------------------------------------------------------

function envelope(over: Partial<StationQueueEnvelope> = {}): StationQueueEnvelope {
  return {
    station: 'loading-dock',
    kind: 'batch',
    discipline: ['priority', 'age'],
    wip_limit: null,
    over_limit: false,
    total: 0,
    data: [],
    ...over,
  };
}

const dockJob = (id: string, over: Partial<JobLite> = {}): JobLite => ({
  id, kind: 'ship-a-change', title: `car ${id}`, status: 'open',
  opened_on: '2026-08-10', metadata: { branch: `feat/${id}` }, ...over,
});

describe('the dock from the station envelope', () => {
  test('envelope rows map to the same packet-card grammar as dockRows', () => {
    const env = envelope({
      total: 2,
      data: [
        dockJob('s1', {
          tags: ['hotfix'],
          metadata: { branch: 'feat/s1', skip_reason: 'CI red' },
          simulated: true,
        }),
        dockJob('s2'),
      ],
    });
    const y = assembleYard([], [], env);
    expect(y.dock.map(c => c.id)).toEqual(['s1', 's2']);
    expect(y.dock[0]).toEqual({
      id: 's1', kind: 'ship-a-change', branch: 'feat/s1', title: 'car s1',
      tags: ['hotfix'], sim: true, skipReason: 'CI red',
    });
    expect(y.dock[1]?.sim).toBe(false);
    expect(y.dock[1]?.skipReason).toBeNull();
  });

  test('the envelope is authoritative: membership does not re-derive from ships', () => {
    // A ship that dockRows would park, but the station did not serve.
    const parked: JobLite = {
      id: 'c1', kind: 'ship-a-change', title: 'A car', status: 'open',
      opened_on: '2026-08-12', metadata: { branch: 'feat/a' },
      steps: [s('review', 'ready')],
    };
    const y = assembleYard([], [parked], envelope({ total: 1, data: [dockJob('s9')] }));
    expect(y.dock.map(c => c.id)).toEqual(['s9']);
  });

  test('server order is preserved — no client re-sort by age or anything else', () => {
    // Deliberately NOT in age order: any client-side re-sort would flip it.
    const env = envelope({
      total: 3,
      data: [
        dockJob('newer', { opened_on: '2026-08-12' }),
        dockJob('oldest', { opened_on: '2026-08-01' }),
        dockJob('middle', { opened_on: '2026-08-07' }),
      ],
    });
    const y = assembleYard([], [], env);
    expect(y.dock.map(c => c.id)).toEqual(['newer', 'oldest', 'middle']);
  });

  test('the header facts come off the envelope', () => {
    const y = assembleYard([], [], envelope({ wip_limit: 5, over_limit: true, total: 7 }));
    expect(y.dockStation).toEqual({
      source: 'station',
      discipline: ['priority', 'age'],
      wipLimit: 5,
      overLimit: true,
      total: 7,
      upstream: null,
    });
  });

  test('without an envelope the dock falls back to the derived rows', () => {
    const parked: JobLite = {
      id: 'c1', kind: 'ship-a-change', title: 'A car', status: 'open',
      opened_on: '2026-08-12', metadata: { branch: 'feat/a' },
      steps: [s('review', 'ready')],
    };
    const y = assembleYard([], [parked], null);
    expect(y.dock.map(c => c.id)).toEqual(['c1']);
    expect(y.dockStation).toEqual({ source: 'derived' });
    // The 2-arg call sites mean the same thing.
    expect(assembleYard([], [parked]).dockStation).toEqual({ source: 'derived' });
  });
});

describe('the station header idiom', () => {
  test('discipline renders in the mono-caps idiom', () => {
    expect(disciplineLabel(['priority', 'age'])).toBe('PRIORITY → AGE');
    expect(disciplineLabel(['due'])).toBe('DUE');
    // A key published tomorrow renders with zero code change.
    expect(disciplineLabel(['shortest-job-first'])).toBe('SHORTEST-JOB-FIRST');
  });

  test('the WIP chip appears only on an over-limit station', () => {
    const over = assembleYard([], [], envelope({ wip_limit: 5, over_limit: true, total: 7 }));
    expect(wipAdvisory(over.dockStation)).toBe('WIP 7/5');
    const under = assembleYard([], [], envelope({ wip_limit: 5, over_limit: false, total: 3 }));
    expect(wipAdvisory(under.dockStation)).toBeNull();
    // No declared limit -> never a chip, whatever the flag says.
    const limitless = assembleYard([], [], envelope({ over_limit: true, total: 9 }));
    expect(wipAdvisory(limitless.dockStation)).toBeNull();
    // The derived dock has no station facts to advertise.
    expect(wipAdvisory({ source: 'derived' })).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// The walk upstream (David, feedback 3ccb79f5): when a queue is not
// filling as expected, the diagnosis is upstream. The station row
// declares where that is; the lens renders a button for whatever the
// row says and nothing at all when it says nothing.
// ---------------------------------------------------------------------------

describe('the upstream button', () => {
  test('a declared upstream becomes a button labelled for the walk', () => {
    const y = assembleYard(
      [],
      [],
      envelope({ upstream: { label: 'FEEDBACK', href: '/system/feedback' } }),
    );
    const b = dockUpstream(y.dockStation);
    expect(b).not.toBeNull();
    expect(b!.label).toBe('↑ UPSTREAM: FEEDBACK');
    expect(b!.href).toBe('/system/feedback');
    // The tooltip says what the button does, not what it is.
    expect(b!.title).toContain('feeds this station');
  });

  test('the label is the registry vocabulary, upper-cased — a station published tomorrow needs no code', () => {
    const y = assembleYard(
      [],
      [],
      envelope({ upstream: { label: 'design docs', href: '/system/design' } }),
    );
    expect(dockUpstream(y.dockStation)?.label).toBe('↑ UPSTREAM: DESIGN DOCS');
  });

  test('a station declaring no upstream renders nothing', () => {
    expect(dockUpstream(assembleYard([], [], envelope()).dockStation)).toBeNull();
  });

  test('the derived dock has no station row, so no upstream', () => {
    expect(dockUpstream({ source: 'derived' })).toBeNull();
  });

  test('a half-declared pointer is not a button — a dead link is worse than none', () => {
    const noHref = assembleYard([], [], envelope({ upstream: { label: 'FEEDBACK', href: '' } }));
    expect(dockUpstream(noHref.dockStation)).toBeNull();
    const noLabel = assembleYard(
      [],
      [],
      envelope({ upstream: { label: '', href: '/system/feedback' } }),
    );
    expect(dockUpstream(noLabel.dockStation)).toBeNull();
  });

  test('an older cluster whose envelope predates the field is simply upstream-less', () => {
    // The key is absent, not null — the shape a station registry
    // deployed before 119-station-upstream.sql serves.
    const legacy = { ...envelope() } as Record<string, unknown>;
    delete legacy.upstream;
    const y = assembleYard([], [], legacy as unknown as StationQueueEnvelope);
    expect(y.dockStation).toEqual({
      source: 'station',
      discipline: ['priority', 'age'],
      wipLimit: null,
      overLimit: false,
      total: 0,
      upstream: null,
    });
    expect(dockUpstream(y.dockStation)).toBeNull();
  });
});

describe('fetchYard against the station endpoint', () => {
  const realFetch = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = realFetch;
  });

  const json = (body: unknown, status = 200) =>
    new Response(JSON.stringify(body), { status });

  function stub(station: () => Response | Promise<Response>) {
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes('/api/stations/loading-dock/queue')) return station();
      if (url.includes('kind=pr-train')) return json({ data: [] });
      if (url.includes('kind=ship-a-change'))
        return json({
          data: [
            {
              id: 'c1', kind: 'ship-a-change', title: 'A car', status: 'open',
              opened_on: '2026-08-12', metadata: { branch: 'feat/a' },
              steps: [s('review', 'ready')],
            },
          ],
        });
      throw new Error(`unexpected fetch: ${url}`);
    }) as typeof fetch;
  }

  test('when the endpoint serves, the dock reads its own station row', async () => {
    stub(() => json(envelope({ total: 1, data: [dockJob('s1')] })));
    const y = await fetchYard();
    expect(y?.dock.map(c => c.id)).toEqual(['s1']);
    expect(y?.dockStation.source).toBe('station');
  });

  test('a cluster that predates the registry still renders the yard whole', async () => {
    // 404 (no station row), 503 (registry not configured), and a
    // thrown network error all mean the same thing: derive locally.
    for (const station of [
      () => json('no such station', 404),
      () => json('station registry not configured', 503),
      () => Promise.reject(new Error('connection refused')),
    ]) {
      stub(station as () => Response | Promise<Response>);
      const y = await fetchYard();
      expect(y?.dock.map(c => c.id)).toEqual(['c1']);
      expect(y?.dockStation).toEqual({ source: 'derived' });
    }
  });

  test('a 200 that is not a queue envelope falls back too', async () => {
    stub(() => json({ hello: 'not an envelope' }));
    const y = await fetchYard();
    expect(y?.dock.map(c => c.id)).toEqual(['c1']);
    expect(y?.dockStation).toEqual({ source: 'derived' });
  });
});

describe('the packet-card grammar', () => {
  test('a simulated packet is named by its data, not a code path', () => {
    const base = { id: 'x', kind: 'ship-a-change', title: 't', status: 'open',
      opened_on: '2026-08-12' } as const;
    // The Job's admission-fixed field is the source of truth …
    expect(isSim({ ...base, simulated: true })).toBe(true);
    expect(isSim({ ...base, simulated: true, tags: [], metadata: {} })).toBe(true);
    // … and the tag / metadata conventions survive as fallback for
    // packets that predate it.
    expect(isSim({ ...base, tags: ['sim'] })).toBe(true);
    expect(isSim({ ...base, tags: ['Simulated'] })).toBe(true);
    expect(isSim({ ...base, metadata: { simulated: true } })).toBe(true);
    expect(isSim({ ...base, simulated: false, tags: ['sim'] })).toBe(true);
    expect(isSim({ ...base, tags: ['fix'], metadata: {} })).toBe(false);
    expect(isSim({ ...base, simulated: false })).toBe(false);
  });
  test('protocol hue is stable, palette-bound, and distinguishes the yard kinds', () => {
    expect(protocolHue('ship-a-change')).toBe(protocolHue('ship-a-change'));
    expect(PROTOCOL_PALETTE).toContain(protocolHue('ship-a-change'));
    expect(PROTOCOL_PALETTE).toContain(protocolHue('some-future-kind'));
    expect(protocolHue('ship-a-change')).not.toBe(protocolHue('pr-train'));
    expect(new Set(PROTOCOL_PALETTE).size).toBe(PROTOCOL_PALETTE.length);
  });
});

// ---------------------------------------------------------------------------
// Arrivals are ARRIVALS (David, 2026-08-13: an EMPTY train sat at the
// top of the board). Two separate defects: a cancelled train is not an
// arrival, and `opened_on` is day-granular so a ported train tied with
// today's real arrival and won the tie-break.
// ---------------------------------------------------------------------------

/** A completed step carrying the conductor's RFC3339 stamp. */
const at = (slug: string, completedAt: string, metadata: Record<string, unknown> = {}) => ({
  spec_slug: slug,
  title: slug,
  status: 'completed',
  metadata: { ...metadata, completed_at: completedAt },
});

/** A completed step with only the day-granular column. */
const on = (slug: string, completedOn: string) => ({
  spec_slug: slug,
  title: slug,
  status: 'completed',
  metadata: {},
  completed_on: completedOn,
});

describe('trainOutcome', () => {
  test('reads the terminal outcome the close stamps on the Job', () => {
    expect(trainOutcome(train({ status: 'closed', metadata: { outcome: 'arrived' } }))).toBe(
      'arrived',
    );
    expect(trainOutcome(train({ status: 'closed', metadata: { outcome: 'cancelled' } }))).toBe(
      'cancelled',
    );
  });

  test('falls back to the completed terminal step for trains closed before the stamp', () => {
    expect(
      trainOutcome(train({ status: 'closed', steps: [s('arrived', 'completed')] })),
    ).toBe('arrived');
    expect(
      trainOutcome(train({ status: 'closed', steps: [s('cancelled', 'completed')] })),
    ).toBe('cancelled');
  });

  test('a SKIPPED terminal is not a terminal — the cancelled-train trap', () => {
    // close_job_on_terminal skips every non-terminal step, so a
    // cancelled train carries a *skipped* `arrived` step. Treating
    // skipped as done is exactly how the empty train reached the top.
    const cancelled = train({
      status: 'closed',
      steps: [
        s('collect', 'completed'),
        s('deployed', 'skipped'),
        s('arrived', 'skipped'),
        s('cancelled', 'completed'),
      ],
    });
    expect(trainOutcome(cancelled)).toBe('cancelled');
  });

  test('a deploy that happened is arrival evidence for trains predating the terminals', () => {
    expect(trainOutcome(train({ status: 'closed', steps: [s('deployed', 'completed')] }))).toBe(
      'arrived',
    );
    // Closed with nothing to show for it: unknown, never an arrival.
    expect(trainOutcome(train({ status: 'closed' }))).toBe('unknown');
  });

  test('an idle window and a refused consist are not cancellations', () => {
    // Of the 8 trains that closed in the 14 hours to 2026-08-20, 4
    // "cancelled" with zero cars aboard — windows that opened with
    // nothing waiting. The protocol now closes those `idle`, and a
    // consist the check refused `refused`; the board must not go on
    // calling either one a train that failed.
    expect(trainOutcome(train({ status: 'closed', metadata: { outcome: 'idle' } }))).toBe('idle');
    expect(trainOutcome(train({ status: 'closed', metadata: { outcome: 'refused' } }))).toBe(
      'refused',
    );
    expect(
      trainOutcome(train({ status: 'closed', steps: [s('arrived', 'skipped'), s('idle', 'completed')] })),
    ).toBe('idle');
    expect(outcomeText('idle')).toContain('idle window');
    expect(outcomeText('refused')).toContain('boardable');
    expect(outcomeText('cancelled')).toBe('cancelled, the train did not arrive');
  });
});

describe('arrivalStamp', () => {
  test('prefers the arrived step completed_at, then deployed, then the date, then opened_on', () => {
    const both = train({
      opened_on: '2026-08-01',
      steps: [at('deployed', '2026-08-13T09:20:00Z'), at('arrived', '2026-08-13T09:24:00Z')],
    });
    expect(arrivalStamp(both)).toEqual({
      ms: Date.parse('2026-08-13T09:24:00Z'),
      at: '2026-08-13T09:24:00Z',
      basis: 'completed_at',
    });
    // Only the deploy carries an instant.
    expect(arrivalStamp(train({ steps: [at('deployed', '2026-08-13T09:20:00Z')] })).at).toBe(
      '2026-08-13T09:20:00Z',
    );
    // The conductor may stamp the column rather than the metadata.
    const column = train({
      steps: [
        { spec_slug: 'arrived', title: 'arrived', status: 'completed',
          completed_at: '2026-08-13T09:24:00Z', metadata: {} },
      ],
    });
    expect(arrivalStamp(column).basis).toBe('completed_at');
    // Day-granular fallback …
    expect(arrivalStamp(train({ steps: [on('deployed', '2026-08-11')] }))).toEqual({
      ms: Date.parse('2026-08-11'),
      at: '2026-08-11',
      basis: 'completed_on',
    });
    // … and the last resort.
    expect(arrivalStamp(train({ opened_on: '2026-08-05' }))).toEqual({
      ms: Date.parse('2026-08-05'),
      at: '2026-08-05',
      basis: 'opened_on',
    });
  });

  test('an unparseable stamp falls through instead of poisoning the order', () => {
    const junk = train({
      opened_on: '2026-08-05',
      steps: [at('arrived', 'whenever'), on('deployed', '2026-08-11')],
    });
    expect(arrivalStamp(junk).basis).toBe('completed_on');
  });
});

describe('the arrivals board', () => {
  const arrivedTrain = (id: string, over: Partial<JobLite> = {}): JobLite =>
    train({ id, status: 'closed', metadata: { outcome: 'arrived' }, ...over });

  test('a cancelled train never arrived — it is not on the arrivals board', () => {
    const y = assembleYard(
      [
        train({
          id: 'empty',
          title: 'train: 2026-08-13 AM',
          status: 'closed',
          // The empty window: no cars, cancelled by the marker.
          metadata: { outcome: 'cancelled', empty: 'true' },
          steps: [s('collect', 'completed'), s('cancelled', 'completed')],
        }),
        arrivedTrain('real', { steps: [at('arrived', '2026-08-13T09:24:00Z')] }),
      ],
      [],
    );
    expect(y.arrivals.map(t => t.id)).toEqual(['real']);
    // Not silently dropped from the world, either.
    expect(y.cancelled.map(t => t.id)).toEqual(['empty']);
    expect(y.cancelled[0]?.outcome).toBe('cancelled');
  });

  test('an ancient ported train does not outrank today’s arrival', () => {
    // The live failure: the port stamped every train with the port
    // date, so `opened_on` ties and the tie-break is arbitrary —
    // input order won, and the ancient train was first.
    const ancient = arrivedTrain('ancient', {
      opened_on: '2026-08-13',
      steps: [at('deployed', '2026-03-02T11:04:00Z'), at('arrived', '2026-03-02T11:05:00Z')],
    });
    const today = arrivedTrain('today', {
      opened_on: '2026-08-13',
      steps: [at('deployed', '2026-08-13T09:20:00Z'), at('arrived', '2026-08-13T09:24:00Z')],
    });
    const dateOnly = arrivedTrain('date-only', {
      opened_on: '2026-08-13',
      steps: [on('deployed', '2026-08-11')],
    });
    const openedOnly = arrivedTrain('opened-only', { opened_on: '2026-08-12' });

    const y = assembleYard([ancient, dateOnly, openedOnly, today], []);
    expect(y.arrivals.map(t => t.id)).toEqual(['today', 'opened-only', 'date-only', 'ancient']);
    expect(y.arrivals[0]?.arrivedAt.basis).toBe('completed_at');
    expect(y.arrivals[3]?.arrivedAt.at).toBe('2026-03-02T11:05:00Z');
  });

  test('the board holds the five most recent arrivals', () => {
    const trains = Array.from({ length: 7 }, (_, i) =>
      arrivedTrain(`t${i}`, {
        steps: [at('arrived', `2026-08-0${i + 1}T09:00:00Z`)],
      }),
    );
    const y = assembleYard(trains, []);
    expect(y.arrivals.map(t => t.id)).toEqual(['t6', 't5', 't4', 't3', 't2']);
  });
});

// ---------------------------------------------------------------------------
// The landing report the conductor writes into the arrived step.
// Every field may be null; nothing here invents one.
// ---------------------------------------------------------------------------

const REPORT = {
  consist: [
    { car_id_short: 'a1b2c3d4', title: 'Fix the lamp', branch: 'feat/lamp' },
    { car_id_short: 'e5f6a7b8', title: 'Yard ETAs', branch: 'feat/eta' },
  ],
  left_behind: [{ car_id_short: 'c9d0e1f2', reason: 'conflict in yard.ts' }],
  generation: 'g41',
  merged_sha: '3c0c63e',
  timings: {
    boarded_at: '2026-08-13T08:50:00Z',
    merged_at: '2026-08-13T09:15:00Z',
    deployed_at: '2026-08-13T09:22:00Z',
    arrived_at: '2026-08-13T09:24:00Z',
    board_to_merge_s: 1500,
    merge_to_deploy_s: 420,
    total_s: 2040,
  },
};

describe('arrivalReport', () => {
  test('reads the object out of the arrived step', () => {
    const r = arrivalReport(train({ steps: [s('arrived', 'completed', { arrival_report: REPORT })] }));
    expect(r).toEqual(REPORT);
    expect(r?.consist.map(c => c.branch)).toEqual(['feat/lamp', 'feat/eta']);
    expect(r?.left_behind[0]?.reason).toBe('conflict in yard.ts');
    expect(r?.timings?.total_s).toBe(2040);
  });

  test('an older train has no report and renders nothing extra', () => {
    expect(arrivalReport(train({ steps: [s('arrived', 'completed')] }))).toBeNull();
    expect(arrivalReport(train({}))).toBeNull();
    // Not an object -> not a report.
    expect(
      arrivalReport(train({ steps: [s('arrived', 'completed', { arrival_report: 'soon' })] })),
    ).toBeNull();
  });

  test('missing fields stay null / empty — never invented', () => {
    const r = arrivalReport(
      train({ steps: [s('arrived', 'completed', { arrival_report: { generation: 7 } })] }),
    );
    expect(r).toEqual({
      consist: [],
      left_behind: [],
      generation: '7',
      merged_sha: null,
      timings: null,
    });
  });

  test('found wherever the conductor stamped it', () => {
    const r = arrivalReport(
      train({ steps: [s('deployed', 'completed', { arrival_report: REPORT })] }),
    );
    expect(r?.merged_sha).toBe('3c0c63e');
  });
});

// ---------------------------------------------------------------------------
// ETAs (David: "we have this instant point-to-point network but
// batching requires a lot of coordination — can we put ETAs on
// trains?"). Honest or absent: the estimate comes from medians of
// what recent trains actually did, and a train with no started-at
// evidence gets its phase and no time.
// ---------------------------------------------------------------------------

describe('arrivalMedians', () => {
  const leg = (id: string, pr: string, merged: string, deployed: string): JobLite =>
    train({
      id,
      status: 'closed',
      metadata: { outcome: 'arrived' },
      steps: [at('pr', pr), at('merged', merged), at('deployed', deployed)],
    });

  test('fewer than two usable samples is a no-estimate state', () => {
    expect(arrivalMedians([])).toEqual({
      boardToMergeS: null,
      mergeToDeployS: null,
      samples: 0,
    });
    const one = leg('a', '2026-08-13T09:00:00Z', '2026-08-13T09:30:00Z', '2026-08-13T09:40:00Z');
    expect(arrivalMedians([one])).toEqual({
      boardToMergeS: null,
      mergeToDeployS: null,
      samples: 1,
    });
    // Timestamps the conductor never stamped are not samples.
    const blind = train({ id: 'b', status: 'closed', metadata: { outcome: 'arrived' },
      steps: [s('pr', 'completed'), s('merged', 'completed'), s('deployed', 'completed')] });
    expect(arrivalMedians([one, blind]).boardToMergeS).toBeNull();
  });

  test('medians of the legs recent trains actually ran', () => {
    const m = arrivalMedians([
      leg('a', '2026-08-13T09:00:00Z', '2026-08-13T09:30:00Z', '2026-08-13T09:35:00Z'), // 1800 / 300
      leg('b', '2026-08-12T09:00:00Z', '2026-08-12T09:10:00Z', '2026-08-12T09:20:00Z'), // 600 / 600
      leg('c', '2026-08-11T09:00:00Z', '2026-08-11T09:20:00Z', '2026-08-11T09:35:00Z'), // 1200 / 900
    ]);
    expect(m).toEqual({ boardToMergeS: 1200, mergeToDeployS: 600, samples: 3 });
    // Even counts average the two middles.
    expect(
      arrivalMedians([
        leg('a', '2026-08-13T09:00:00Z', '2026-08-13T09:10:00Z', '2026-08-13T09:15:00Z'), // 600 / 300
        leg('b', '2026-08-12T09:00:00Z', '2026-08-12T09:20:00Z', '2026-08-12T09:35:00Z'), // 1200 / 900
      ]).boardToMergeS,
    ).toBe(900);
  });

  test('the conductor’s own timings beat re-derived deltas', () => {
    const reported = (id: string, bm: number, md: number): JobLite =>
      train({
        id,
        status: 'closed',
        metadata: { outcome: 'arrived' },
        steps: [
          s('arrived', 'completed', {
            arrival_report: { timings: { board_to_merge_s: bm, merge_to_deploy_s: md } },
          }),
        ],
      });
    expect(arrivalMedians([reported('a', 900, 200), reported('b', 1100, 400)])).toEqual({
      boardToMergeS: 1000,
      mergeToDeployS: 300,
      samples: 2,
    });
  });

  test('only the last N arrivals count', () => {
    const recent = Array.from({ length: 5 }, (_, i) =>
      leg(`r${i}`, '2026-08-13T09:00:00Z', '2026-08-13T09:10:00Z', '2026-08-13T09:15:00Z'),
    );
    const ancient = leg('old', '2026-01-01T09:00:00Z', '2026-01-01T19:00:00Z', '2026-01-02T09:00:00Z');
    const m = arrivalMedians([...recent, ancient], 5);
    expect(m).toEqual({ boardToMergeS: 600, mergeToDeployS: 300, samples: 5 });
  });
});

describe('etaPhase', () => {
  test('the phase is the train status crossed with the CI lamp', () => {
    expect(etaPhase('BOARDING', 'pending')).toBe('boarding');
    expect(etaPhase('BOARDED', 'pending')).toBe('ci');
    expect(etaPhase('BOARDED', 'green')).toBe('merging');
    expect(etaPhase('BOARDED', 'failing')).toBe('blocked');
    expect(etaPhase('DEPARTED', 'green')).toBe('deploying');
    expect(etaPhase('DEPARTED', 'failing')).toBe('deploying');
    expect(etaPhase('ARRIVED', 'green')).toBe('arrived');
  });
});

describe('trainEta', () => {
  const M = { boardToMergeS: 1800, mergeToDeployS: 600, samples: 4 } as const;
  const now = Date.parse('2026-08-13T09:00:00Z');

  test('a boarded train: the rest of the board→merge leg plus the deploy leg', () => {
    const t = train({ steps: [at('pr', '2026-08-13T08:50:00Z'), s('ci', 'completed', { result: 'green' })] });
    const eta = trainEta(t, M, now);
    expect(eta).toEqual({
      kind: 'eta',
      phase: 'merging',
      // 1800 - 600 elapsed = 1200 left on the leg, + 600 to deploy.
      atMs: now + 1_800_000,
      basis: 'median of last 4 arrivals',
    });
  });

  test('a departed train rides the merge→deploy median alone, clamped at now', () => {
    const t = train({
      steps: [
        at('pr', '2026-08-13T08:00:00Z'),
        at('merged', '2026-08-13T08:55:00Z'),
        s('ci', 'completed', { result: 'green' }),
      ],
    });
    expect(trainEta(t, M, now)).toEqual({
      kind: 'eta',
      phase: 'deploying',
      atMs: now + 300_000,
      basis: 'median of last 4 arrivals',
    });
    // Overdue never runs backwards: the estimate is "any moment now".
    const late = train({
      steps: [at('pr', '2026-08-13T07:00:00Z'), at('merged', '2026-08-13T08:00:00Z')],
    });
    expect(trainEta(late, M, now)).toEqual({
      kind: 'eta',
      phase: 'deploying',
      atMs: now,
      basis: 'median of last 4 arrivals',
    });
  });

  test('no medians, no promise — the phase renders without a time', () => {
    const t = train({ steps: [at('pr', '2026-08-13T08:50:00Z')] });
    expect(trainEta(t, NO_MEDIANS, now)).toEqual({ kind: 'phase', phase: 'ci' });
  });

  test('no started-at evidence, no invented duration', () => {
    // Boarded, but nothing says when — phase only.
    expect(trainEta(train({ steps: [s('pr', 'completed')] }), M, now)).toEqual({
      kind: 'phase',
      phase: 'ci',
    });
    // Still boarding: there is no leg under way to estimate.
    expect(trainEta(train({ steps: [s('pr', 'ready')] }), M, now)).toEqual({
      kind: 'phase',
      phase: 'boarding',
    });
    // Red CI: the fix has no median. Say so instead of guessing.
    const red = train({
      steps: [at('pr', '2026-08-13T08:50:00Z'), s('ci', 'completed', { result: 'failing' })],
    });
    expect(trainEta(red, M, now)).toEqual({ kind: 'phase', phase: 'blocked' });
  });
});

describe('the yard wires ETAs onto trains in flight', () => {
  test('in-flight rows carry an estimate built from the arrivals below them', () => {
    const arrivals: JobLite[] = ['2026-08-13', '2026-08-12'].map((d, i) =>
      train({
        id: `a${i}`,
        status: 'closed',
        metadata: { outcome: 'arrived' },
        steps: [
          at('pr', `${d}T08:00:00Z`),
          at('merged', `${d}T08:30:00Z`),
          at('deployed', `${d}T08:40:00Z`),
          at('arrived', `${d}T08:41:00Z`),
        ],
      }),
    );
    const flying = train({
      id: 'flying',
      steps: [at('pr', '2026-08-13T09:00:00Z'), s('ci', 'completed', { result: 'green' })],
    });
    const now = Date.parse('2026-08-13T09:10:00Z');
    const y = assembleYard([flying, ...arrivals], [], null, now);
    // medians: board→merge 1800, merge→deploy 600; 600s elapsed.
    expect(y.inFlight[0]?.eta).toEqual({
      kind: 'eta',
      phase: 'merging',
      atMs: now + 1_800_000,
      basis: 'median of last 2 arrivals',
    });
    // An arrived train is not in flight and gets no estimate.
    expect(y.arrivals[0]?.eta).toEqual({ kind: 'phase', phase: 'arrived' });
  });
});
