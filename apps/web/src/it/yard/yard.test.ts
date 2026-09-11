import { afterEach, describe, expect, test } from 'bun:test';
import {
  awaitingProof,
  splitAtDeparture,
  comparableVersions,
  deliveryStats,
  arrivalMedians,
  arrivalReport,
  arrivalStamp,
  assembleYard,
  disciplineLabel,
  etaPhase,
  fetchYard,
  trainEta,
  trainOutcome,
  trainStatus,
  ciLamp,
  isSim,
  type TerminalReport,
  type TerminalVersion,
  protocolHue,
  wipAdvisory,
  dockUpstream,
  NO_MEDIANS,
  PROTOCOL_PALETTE,
  type JobLite,
  type StationQueueEnvelope,
  trainTrouble,
  troubleLabel,
  toTrainRow,
  cancelRequestBody,
  canOfferCancel,
  CANCEL_ROLE,
  type TrainRow,
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
  test('walks BOARDING → BOARDED → DEPARTED → CONVERGING → ARRIVED', () => {
    expect(trainStatus(train({ steps: [s('pr', 'ready')] }))).toBe('BOARDING');
    expect(trainStatus(train({ steps: [s('pr', 'completed')] }))).toBe('BOARDED');
    expect(
      trainStatus(train({ steps: [s('pr', 'completed'), s('merged', 'completed')] })),
    ).toBe('DEPARTED');
    // Deployed, but the cluster has not converged on the merge yet — the
    // real ~10-minute wait. Still live, not arrived.
    expect(
      trainStatus(
        train({
          steps: [
            s('merged', 'completed'),
            s('deployed', 'completed'),
            s('converged', 'ready'),
          ],
        }),
      ),
    ).toBe('CONVERGING');
    // Both deployed AND converged done → arrived.
    expect(
      trainStatus(
        train({
          steps: [
            s('merged', 'completed'),
            s('deployed', 'completed'),
            s('converged', 'completed'),
          ],
        }),
      ),
    ).toBe('ARRIVED');
    // A pre-converged train has no `converged` step; its finish line is
    // `deployed`, so its absence is arrival, not a stuck train.
    expect(
      trainStatus(train({ steps: [s('merged', 'completed'), s('deployed', 'completed')] })),
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
  test('the dock is the station\'s rows; consists join by id', () => {
    const y = assembleYard(
      [train({ metadata: { boarded_jobs: ['c2'] }, steps: [s('pr', 'completed')] })],
      ships,
      envelope({ total: 1, data: [ships[0] as JobLite] }),
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
      envelope({ total: 1, data: [ships[0] as JobLite] }),
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
  test('envelope rows map to the packet-card grammar', () => {
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
      tags: ['hotfix'], sim: true, skipReason: 'CI red', head: null,
    });
    expect(y.dock[1]?.sim).toBe(false);
    expect(y.dock[1]?.skipReason).toBeNull();
  });

  test('the envelope is authoritative: membership does not re-derive from ships', () => {
    // A ship the old client predicate would have parked, which the
    // station did not serve.
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

  test('without an envelope the dock cannot be read — never a derived list', () => {
    // The membership rule is the station ROW's (predicate + the hold
    // clause 36c3d4ca taught it). A client copy of it was a fourth
    // definition that could not follow the row, so there is none: the
    // lane says it cannot see rather than listing a car the row would
    // no longer admit.
    const parked: JobLite = {
      id: 'c1', kind: 'ship-a-change', title: 'A car', status: 'open',
      opened_on: '2026-08-12', metadata: { branch: 'feat/a' },
      steps: [s('review', 'ready')],
    };
    const y = assembleYard([], [parked], null);
    expect(y.dock).toEqual([]);
    expect(y.dockStation).toEqual({ source: 'unavailable' });
    // The 2-arg call sites mean the same thing.
    expect(assembleYard([], [parked]).dockStation).toEqual({ source: 'unavailable' });
    // The rest of the yard still renders: the car is still a car.
    expect(y.cars.map(c => c.id)).toEqual(['c1']);
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
    // A dock with no reading has no station facts to advertise.
    expect(wipAdvisory({ source: 'unavailable' })).toBeNull();
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
      envelope({ upstream: { label: 'FEEDBACK', href: '/it/design/feedback' } }),
    );
    const b = dockUpstream(y.dockStation);
    expect(b).not.toBeNull();
    expect(b!.label).toBe('↑ UPSTREAM: FEEDBACK');
    expect(b!.href).toBe('/it/design/feedback');
    // The tooltip says what the button does, not what it is.
    expect(b!.title).toContain('feeds this station');
  });

  test('the label is the registry vocabulary, upper-cased — a station published tomorrow needs no code', () => {
    const y = assembleYard(
      [],
      [],
      envelope({ upstream: { label: 'design docs', href: '/it/design' } }),
    );
    expect(dockUpstream(y.dockStation)?.label).toBe('↑ UPSTREAM: DESIGN DOCS');
  });

  test('a station declaring no upstream renders nothing', () => {
    expect(dockUpstream(assembleYard([], [], envelope()).dockStation)).toBeNull();
  });

  test('a dock with no reading has no station row, so no upstream', () => {
    expect(dockUpstream({ source: 'unavailable' })).toBeNull();
  });

  test('a half-declared pointer is not a button — a dead link is worse than none', () => {
    const noHref = assembleYard([], [], envelope({ upstream: { label: 'FEEDBACK', href: '' } }));
    expect(dockUpstream(noHref.dockStation)).toBeNull();
    const noLabel = assembleYard(
      [],
      [],
      envelope({ upstream: { label: '', href: '/it/design/feedback' } }),
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

  test('an endpoint that will not serve leaves the dock unreadable, not empty', async () => {
    // 404 (no station row), 503 (registry not configured), a thrown
    // network error, and a 200 that is not the envelope all mean the
    // same thing — and it is NOT "no cars are parked". A rollback to an
    // image that cannot deserialize the row (StepMatch is
    // deny_unknown_fields) lands here, mid-incident, which is exactly
    // when a confident wrong list costs the most.
    for (const station of [
      () => json('no such station', 404),
      () => json('station registry not configured', 503),
      () => Promise.reject(new Error('connection refused')),
      () => json({ hello: 'not an envelope' }),
    ]) {
      stub(station as () => Response | Promise<Response>);
      const y = await fetchYard();
      expect(y?.dock).toEqual([]);
      expect(y?.dockStation).toEqual({ source: 'unavailable' });
      // And the yard still renders: the car read is independent.
      expect(y?.cars.map(c => c.id)).toEqual(['c1']);
    }
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

// The converge-wait window (2026-09-02): `deployed` completes in
// seconds, but the cluster takes ~10 minutes to converge on the merge.
// A train there is deployed=completed, converged=ready, job open. It
// must stay a LIVE, in-transit train — not vanish as an inert ARRIVED
// row — until the cluster actually converges.
describe('a converging train stays live, not arrived', () => {
  const converging = () =>
    train({
      id: 'conv',
      status: 'open',
      steps: [
        at('pr', '2026-09-07T08:00:00Z'),
        s('ci', 'completed', { result: 'green' }),
        at('merged', '2026-09-07T08:05:00Z'),
        at('deployed', '2026-09-07T08:06:00Z'),
        s('converged', 'ready'),
      ],
    });

  test('trainStatus is CONVERGING while the converged step is not done', () => {
    expect(trainStatus(converging())).toBe('CONVERGING');
  });

  test('it is in transit, keeps the live dot, and shows the converging phase', () => {
    const now = Date.parse('2026-09-07T08:10:00Z');
    const y = assembleYard([converging()], [], null, now);
    // Open trains are in flight; the split puts it in transit, not the yard.
    const { inTransit, inYard } = splitAtDeparture(y.inFlight);
    expect(inTransit.map(t => t.id)).toEqual(['conv']);
    expect(inYard).toEqual([]);
    const row = y.inFlight[0]!;
    expect(row.status).toBe('CONVERGING');
    // The one live train — it keeps the pulsing dot.
    expect(row.live).toBe(true);
    expect(row.eta.phase).toBe('converging');
    // Not an arrival: it is still open, the cluster has not converged.
    expect(y.arrivals).toEqual([]);
    // The converge-start instant (the deploy's) is surfaced for the
    // elapsed "converging for …" chip.
    expect(row.convergingSince).toBe('2026-09-07T08:06:00Z');
  });

  test('once the converged step completes, it is ARRIVED', () => {
    const arrived = train({
      id: 'conv',
      status: 'open',
      steps: [
        at('deployed', '2026-09-07T08:06:00Z'),
        s('converged', 'completed'),
      ],
    });
    expect(trainStatus(arrived)).toBe('ARRIVED');
    // And it is no longer live.
    const now = Date.parse('2026-09-07T08:20:00Z');
    const y = assembleYard([arrived], [], null, now);
    expect(y.inFlight[0]?.live).toBe(false);
    expect(y.inFlight[0]?.convergingSince).toBeNull();
  });
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
    expect(etaPhase('CONVERGING', 'green')).toBe('converging');
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
      // 600 of the 1800 s leg is behind it.
      progress: 600 / 1800,
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
      progress: 0.5,
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
      // Overdue is clamped the same way: the leg is 100% behind it.
      progress: 1,
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
      progress: 600 / 1800,
    });
    // An arrived train is not in flight and gets no estimate.
    expect(y.arrivals[0]?.eta).toEqual({ kind: 'phase', phase: 'arrived' });
  });
});


// ---------------------------------------------------------------------
// Delivery stats — the yard's scoreboard (feedback 898761cb).
// ---------------------------------------------------------------------

function ver(
  version: number,
  merged: number,
  abandoned: number,
  median: number | null = 0,
  samples = merged + abandoned,
): TerminalVersion {
  const outcomes: Record<string, number> = {};
  if (merged) outcomes.merged = merged;
  if (abandoned) outcomes.abandoned = abandoned;
  return {
    version,
    total: merged + abandoned,
    outcomes,
    cycle_time_days: { median, p90: null, samples },
  };
}

describe('deliveryStats', () => {
  test('reports abandon rate, cycle time and delivered count for the latest resolved version', () => {
    const report: TerminalReport = { kind: 'ship-a-change', versions: [ver(18, 8, 2, 0)] };
    const stats = deliveryStats(report);
    expect(stats).toHaveLength(3);
    const [abandon, cycle, delivered] = stats as [
      (typeof stats)[0],
      (typeof stats)[0],
      (typeof stats)[0],
    ];
    expect(abandon.label).toBe('abandon rate');
    expect(abandon.value).toBe('20%'); // 2 of 10 resolved
    expect(cycle.value).toBe('0m');
    expect(delivered.value).toBe('8');
  });

  /**
   * Sub-day medians exist now that packets carry precise open/close
   * stamps — a same-day close used to be `0d`, which hid exactly the
   * improvement this scoreboard exists to show.
   */
  test('renders a sub-hour median in minutes, sub-day in hours', () => {
    // Sub-hour reads in MINUTES (feedback b4c1b53a): at a 2-hour train
    // cadence, cycles land under an hour and `0.5h` makes the reader
    // do the arithmetic the scoreboard exists to have done.
    const halfHour: TerminalReport = {
      kind: 'ship-a-change',
      versions: [ver(18, 8, 2, 1800 / 86400)],
    };
    expect(deliveryStats(halfHour)[1]!.value).toBe('30m');

    const halfDay: TerminalReport = { kind: 'ship-a-change', versions: [ver(18, 8, 2, 0.5)] };
    expect(deliveryStats(halfDay)[1]!.value).toBe('12h');
  });

  test('keeps day-or-longer medians in days, to one decimal when fractional', () => {
    const whole: TerminalReport = { kind: 'ship-a-change', versions: [ver(18, 8, 2, 3)] };
    expect(deliveryStats(whole)[1]!.value).toBe('3d');

    const fractional: TerminalReport = { kind: 'ship-a-change', versions: [ver(18, 8, 2, 3.44)] };
    expect(deliveryStats(fractional)[1]!.value).toBe('3.4d');

    // 0.999 days rounds to 24h — that reads as a day, not as hours.
    const nearlyADay: TerminalReport = { kind: 'ship-a-change', versions: [ver(18, 8, 2, 0.999)] };
    expect(deliveryStats(nearlyADay)[1]!.value).toBe('1d');
  });

  test('puts the previous version beside it so the direction is visible', () => {
    const report: TerminalReport = {
      kind: 'ship-a-change',
      versions: [ver(14, 20, 5), ver(18, 8, 2)],
    };
    const abandon = deliveryStats(report)[0]!;
    expect(abandon.value).toBe('20%'); // v18: 2/10
    expect(abandon.previous).toBe('20%'); // v14: 5/25
  });

  /**
   * THE TRAP THIS EXISTS FOR. The newest version usually has packets
   * still in flight and NOTHING resolved — reading it naively prints 0%
   * and looks like a triumph. On 2026-08-28 v24 and v25 held 8 packets
   * between them with zero resolved.
   */
  test('skips versions that have resolved nothing rather than reporting 0%', () => {
    const inflight: TerminalVersion = {
      version: 25,
      total: 3,
      by_status: { open: 3 },
      outcomes: {},
      cycle_time_days: { median: null, p90: null, samples: 0 },
    };
    const report: TerminalReport = { kind: 'ship-a-change', versions: [inflight, ver(18, 8, 2)] };
    const { current } = comparableVersions(report);
    expect(current?.version).toBe(18);
    expect(deliveryStats(report)[0]!.value).toBe('20%');
  });

  test('marks a small sample provisional so it is not read as a trend', () => {
    const report: TerminalReport = { kind: 'ship-a-change', versions: [ver(19, 1, 1)] };
    const abandon = deliveryStats(report)[0]!;
    expect(abandon.value).toBe('50%');
    expect(abandon.samples).toBe(2);
    expect(abandon.provisional).toBe(true);
  });

  test('has no previous when only one version has resolved anything', () => {
    const report: TerminalReport = { kind: 'ship-a-change', versions: [ver(18, 8, 2)] };
    expect(deliveryStats(report)[0]!.previous).toBeNull();
  });

  test('returns nothing at all rather than fake numbers when the report is empty', () => {
    expect(deliveryStats(null)).toEqual([]);
    expect(deliveryStats({ kind: 'ship-a-change', versions: [] })).toEqual([]);
  });
});

describe('awaitingProof', () => {
  const car = (id: string, status: string, slug: string, stepStatus = 'ready') => ({
    id,
    kind: 'ship-a-change',
    title: id,
    status,
    steps: [{ spec_slug: slug, status: stepStatus, title: slug }],
  });

  test('finds merged cars parked at proven — the ones the yard shows nowhere', () => {
    const cars = [
      car('a', 'open', 'proven'),
      car('b', 'open', 'review'),
      car('c', 'closed', 'proven'),
    ];
    expect(awaitingProof(cars as never).map((c) => c.id)).toEqual(['a']);
  });

  test('ignores a car whose proven step is already completed', () => {
    expect(awaitingProof([car('a', 'open', 'proven', 'completed')] as never)).toEqual([]);
  });
});

// ---------------------------------------------------------------------
// The approach — the car lifecycle upstream of the dock (f930cda2).
//
// WHAT THIS SUITE IS FOR, AND WHAT IT NO LONGER ANSWERS. Which gate-runs
// are spent, green, held or red is `/api/yard/status`'s answer, computed
// in boss-jobs off one marker list (`stranded::SPENT_MARKERS`); the cases
// that used to live here — a superseded red, a same-day green answering
// it, which run is a branch's latest, the freshness window — are pinned
// in `stranded.rs` and `yard.rs` instead. What is left for this lens is
// the publish-dock rows, the mapping of the server's four lanes onto the
// approach order, and the contract that a missing status is ADDITIVE.
// ---------------------------------------------------------------------

import { approach, publishRows, type ApproachLanes } from './yard';

const NOW = Date.parse('2026-08-31T21:00:00Z');

function gateRun(over: Partial<JobLite>): JobLite {
  return {
    id: 'g1', kind: 'gate-run', title: 'Gate: x', status: 'open',
    opened_on: '2026-08-31', metadata: { branch: 'fix/x', sha: 'a'.repeat(40) },
    steps: [], ...over,
  };
}

const verdictStep = (verdict: string, head = 'a'.repeat(40)) =>
  ({ title: 'Record the receipt', status: 'completed',
     metadata: { verdict, receipt: JSON.stringify({ verdict, head, mode: 'full' }) } });

function ship(branch: string, over: Partial<JobLite> = {}): JobLite {
  return {
    id: `car-${branch}`, kind: 'ship-a-change', title: branch, status: 'open',
    opened_on: '2026-08-31', metadata: { branch }, steps: [], ...over,
  };
}

const publishEnv = (jobs: readonly JobLite[]): StationQueueEnvelope => ({
  station: 'publish-dock', kind: 'batch', discipline: ['priority', 'age'],
  over_limit: false, total: jobs.length, data: jobs,
});

/** The server's verdict lanes, as the status payload carries them. */
const lanes = (over: Partial<ApproachLanes> = {}): ApproachLanes => ({
  stranded: [],
  held: [],
  garage: [],
  limbo: [],
  ...over,
});

const publishRequest = (branch: string, over: Partial<JobLite> = {}): JobLite => ({
  id: `p-${branch}`, kind: 'publish-request', title: `Publish ${branch}`, status: 'open',
  opened_on: '2026-08-31', metadata: { branch, head_sha: 'b'.repeat(40), requested_by: 'pod' },
  steps: [], ...over,
});

describe('the approach lane', () => {
  test('open publish-requests ride in front, with the requester on the row', () => {
    const rows = approach(publishRows(publishEnv([publishRequest('fix/y')])), lanes());
    expect(rows.map(r => ({ state: r.state, note: r.note, verdict: r.verdict }))).toEqual([
      { state: 'publishing', note: 'pod', verdict: null },
    ]);
  });

  test('a closed publish-request is done asking — no row', () => {
    const rows = approach(publishRows(publishEnv([publishRequest('fix/y', { status: 'closed' })])), lanes());
    expect(rows).toEqual([]);
  });

  test('the four server lanes stand in order: publishing, red, gate exit, green, held', () => {
    const rows = approach(
      publishRows(publishEnv([publishRequest('fix/pub')])),
      lanes({
        garage: [{ branch: 'fix/red', failed_check: 'test', since: '2026-08-31', packet_id: 'g-red', sha: 'r'.repeat(40) }],
        limbo: [{ branch: 'fix/lost', verdict: 'lost', since: '2026-08-31', packet_id: 'g-lost', sha: null }],
        stranded: [{ branch: 'fix/green', packet_id: 'g-green', sha: null, since: '2026-08-31' }],
        held: [{ branch: 'fix/held', reason: 'waiting for #240', since: '2026-08-31', packet_id: 'g-held', sha: null }],
      }),
    );
    expect(rows.map(r => [r.branch, r.state])).toEqual([
      ['fix/pub', 'publishing'],
      ['fix/red', 'gated-red'],
      ['fix/lost', 'gate-lost'],
      ['fix/green', 'gated-green'],
      ['fix/held', 'held'],
    ]);
  });

  test('a stranded green carries the packet, head and instant the server sent', () => {
    // The lane row is complete enough to DRAW: the packet so the row
    // opens, the head so the wagon is labelled, the instant so its age
    // reads. Recovering these from a window of gate-run packets is what
    // grew the second copy of "is this green spent?" in the first place.
    const rows = approach(
      [],
      lanes({
        stranded: [{
          branch: 'feat/x',
          packet_id: 'f802558d-2ebd-4b91-b158-fc4a27ddf5a2',
          sha: 'c'.repeat(40),
          since: '2026-09-09T18:00:00Z',
        }],
      }),
    );
    expect(rows).toEqual([{
      id: 'f802558d-2ebd-4b91-b158-fc4a27ddf5a2',
      branch: 'feat/x',
      sha: 'c'.repeat(40),
      state: 'gated-green',
      opened_on: '2026-09-09T18:00:00Z',
      note: null,
      hold: null,
      verdict: 'green',
    }]);
  });

  test('a held green reads held, with the operator\'s reason — not stranded', () => {
    const rows = approach([], lanes({
      held: [{ branch: 'fix/x', reason: 'lands at the next restart', since: '2026-08-31', packet_id: 'g-h', sha: null }],
    }));
    expect(rows.map(r => ({ state: r.state, hold: r.hold, verdict: r.verdict }))).toEqual([
      { state: 'held', hold: 'lands at the next restart', verdict: 'green' },
    ]);
  });

  test('a garaged row is red with the judged verdict; a gate-exit row carries the unjudged one', () => {
    // `lost` is NOT red: the environment died before anything was
    // judged, and "we do not know" must read neither as rework nor as
    // fine — the gate exit, not the garage.
    const red = approach([], lanes({
      garage: [{ branch: 'fix/r', failed_check: 'clippy', since: '2026-08-31', packet_id: 'g-r', sha: null }],
    }));
    expect(red.map(r => [r.state, r.verdict])).toEqual([['gated-red', 'failed']]);
    const unreadable = approach([], lanes({
      limbo: [{ branch: 'fix/u', verdict: 'unreadable', since: '2026-08-31', packet_id: 'g-u', sha: null }],
    }));
    expect(unreadable.map(r => [r.state, r.verdict])).toEqual([['gate-lost', 'unreadable']]);
  });

  test('no status is ADDITIVE: the station rows still draw and the gate lanes read empty', () => {
    // The status endpoint being down must never take the page with it.
    const rows = approach(publishRows(publishEnv([publishRequest('fix/y')])), null);
    expect(rows.map(r => r.state)).toEqual(['publishing']);
    expect(approach([], null)).toEqual([]);
  });

  test('a lane row with no packet id still draws — it just opens nothing', () => {
    // An older server sends a lane without `packet_id`; the parser reads
    // that as ''. A branch an operator can see beats a row suppressed.
    const rows = approach([], lanes({
      stranded: [{ branch: 'fix/x', packet_id: '', sha: null, since: '2026-08-31' }],
    }));
    expect(rows.map(r => ({ id: r.id, branch: r.branch }))).toEqual([{ id: '', branch: 'fix/x' }]);
  });

  test('assembleYard without the new feeds still assembles, publishing empty', () => {
    const y = assembleYard([], [], null, NOW, null);
    expect(y.publishing).toEqual([]);
  });
});

// ---------------------------------------------------------------------
// THE PHANTOM CAR, 2026-09-10. feat/a-probe-declares-where-it-runs stood
// on the approach as a gated-green wagon all day, on every browser,
// session and device, while the server's /api/yard/status correctly
// reported `stranded: []`. "Is this gate-run spent?" was answered in two
// places: boss-jobs' `stranded::SPENT_MARKERS` (superseded, rerailed_to,
// park_skipped), shared by four Rust readers, and this lens, which knew
// only `superseded` — so a green stamped by `boss rerail` or by the
// auto-park handler was ignored everywhere and drawn here anyway.
// CLAUDE.md §9a. These pin the collapse: the page still HOLDS the
// gate-run packets (the signals panel reads them), and the approach
// draws nothing from them.
// ---------------------------------------------------------------------

describe('a spent gate-run is not on the approach', () => {
  const spent = (marker: Record<string, unknown>): JobLite =>
    gateRun({
      status: 'closed',
      metadata: { branch: 'fix/x', sha: 'a'.repeat(40), ...marker },
      steps: [verdictStep('green')],
    });

  for (const [what, marker] of [
    ['a re-railed green (`boss rerail --finish` stamped rerailed_to)', { rerailed_to: 'fix/x-rerail' }],
    ['a park_skipped green (the auto-park handler looked and declined)', { park_skipped: 'already aboard a train' }],
    ['a superseded green (the branch landed under another name)', { superseded: true }],
  ] as const) {
    test(`${what} draws no approach row`, () => {
      const y = assembleYard([], [], null, NOW, null, [spent(marker)], null);
      // The packet is still in the page's hands...
      expect(y.packets.gateRuns).toHaveLength(1);
      // ...and the approach asks the server, which strands none of them.
      expect(approach(y.publishing, lanes())).toEqual([]);
    });
  }
});

// ---------------------------------------------------------------------
// The departure line is the merge (0bba59f7, ratified 2026-08-31).
// ---------------------------------------------------------------------

describe('splitAtDeparture', () => {
  const row = (id: string, status: string) =>
    ({ id, status }) as unknown as Parameters<typeof splitAtDeparture>[0][number];
  test('pre-merge trains are yard work; post-merge (converging included) is transit', () => {
    const { inYard, inTransit } = splitAtDeparture([
      row('a', 'BOARDING'), row('b', 'BOARDED'), row('c', 'DEPARTED'),
      row('e', 'CONVERGING'), row('d', 'ARRIVED'),
    ]);
    expect(inYard.map(t => t.id)).toEqual(['a', 'b']);
    expect(inTransit.map(t => t.id)).toEqual(['c', 'e', 'd']);
  });
  test('a red-CI train still assembling is YARD — red is status, not a wreck', () => {
    // Placement only: nothing about the lamp changes, only which
    // section holds the train (the honesty note on 0bba59f7).
    const { inYard } = splitAtDeparture([row('r', 'BOARDED')]);
    expect(inYard.length).toBe(1);
  });
});

describe('a troubled train looks troubled', () => {
  const train = (over: Record<string, unknown> = {}) => ({
    id: 't1',
    kind: 'pr-train',
    title: 'PR train',
    status: 'open',
    steps: [{ title: 'Open the batched PR', status: 'completed', metadata: {} }],
    ...over,
  }) as never;

  test('surfaces a converge alarm the conductor already filed', () => {
    const t = train({ metadata: { converge_alarm_filed: true } });
    expect(trainTrouble(t)?.kind).toBe('converge-overdue');
    expect(troubleLabel(trainTrouble(t)!)).toBe('CONVERGE OVERDUE');
  });

  test('surfaces a stall stamp', () => {
    const t = train({ metadata: { stalled_since: '2026-09-02T17:00:00Z' } });
    expect(trainTrouble(t)?.kind).toBe('stalled');
  });

  test('an arrived or closed train is never troubled', () => {
    // Its history is not a live problem.
    const closed = train({ status: 'closed', metadata: { converge_alarm_filed: true } });
    expect(trainTrouble(closed)).toBeNull();
    const arrived = train({
      status: 'open',
      metadata: { converge_alarm_filed: true },
      steps: [{ title: 'Deployed to the playground', status: 'completed', metadata: {} }],
    });
    expect(trainTrouble(arrived)).toBeNull();
  });

  test('a healthy in-flight train is not troubled', () => {
    expect(trainTrouble(train({ metadata: {} }))).toBeNull();
    // An empty stall stamp is not a stall.
    expect(trainTrouble(train({ metadata: { stalled_since: '' } }))).toBeNull();
  });
});

// ---------------------------------------------------------------------
// The yard's cancel button (backlog 7a24caf3). The page writes ONE
// stamp — `cancel_requested` — through the metadata merge, and the
// conductor's reconcile honours it. Everything the button may or may
// not do is a pure rule here, so the page carries no judgement of its
// own.
// ---------------------------------------------------------------------

describe('cancelRequestBody', () => {
  const at = '2026-09-07T21:00:00.000Z';

  test('is exactly the stamp the conductor reads', () => {
    expect(cancelRequestBody('emp-007', 'CI red on a flaky test, re-gate the cars', at)).toEqual({
      cancel_requested: { by: 'emp-007', reason: 'CI red on a flaky test, re-gate the cars', at },
    });
  });

  test('refuses an empty or whitespace-only reason', () => {
    // A stamp with no reason answers nothing when someone asks later
    // why the train was pulled.
    expect(cancelRequestBody('emp-007', '', at)).toBeNull();
    expect(cancelRequestBody('emp-007', '   \n', at)).toBeNull();
  });

  test('trims the reason it stamps', () => {
    expect(cancelRequestBody('emp-007', '  stalled 4h  ', at)?.cancel_requested.reason).toBe(
      'stalled 4h',
    );
  });

  test('refuses a stamp with no actor — provenance is not optional', () => {
    expect(cancelRequestBody('', 'stalled', at)).toBeNull();
  });
});

describe('canOfferCancel', () => {
  const row = (over: Partial<TrainRow> = {}): TrainRow =>
    ({
      id: 't1',
      title: 'PR train',
      status: 'BOARDED',
      lamp: 'failing',
      cars: [],
      live: false,
      outcome: 'unknown',
      arrivedAt: { ms: 0, at: '', basis: 'opened_on' },
      eta: { kind: 'phase', phase: 'blocked' },
      trouble: { kind: 'ci-red' },
      cancelRequested: null,
      cancelRefused: false,
      ...over,
    }) as TrainRow;

  test('in the yard, troubled, privileged, not yet requested → offered', () => {
    expect(canOfferCancel(row(), 'in-yard', true)).toBe(true);
    expect(canOfferCancel(row({ trouble: { kind: 'stalled' } }), 'in-yard', true)).toBe(true);
    expect(canOfferCancel(row({ trouble: { kind: 'converge-overdue' } }), 'in-yard', true)).toBe(
      true,
    );
  });

  test('never in transit — past the merge is irreversible (the departure line)', () => {
    expect(canOfferCancel(row({ status: 'DEPARTED' }), 'in-transit', true)).toBe(false);
  });

  test('never on a train that is not in trouble', () => {
    expect(canOfferCancel(row({ trouble: null, lamp: 'green' }), 'in-yard', true)).toBe(false);
  });

  test('never for an unprivileged viewer', () => {
    expect(canOfferCancel(row(), 'in-yard', false)).toBe(false);
  });

  test('never twice — a pending request hides the button', () => {
    const pending = row({
      cancelRequested: { by: 'emp-007', reason: 'stalled', at: '2026-09-07T21:00:00Z' },
    });
    expect(canOfferCancel(pending, 'in-yard', true)).toBe(false);
  });

  test('the privileged role is platform-admin, spelled the way boss_core spells it', () => {
    expect(CANCEL_ROLE).toBe('platform-admin');
  });
});

describe('TrainRow carries the cancel stamps off the job metadata', () => {
  const none = new Map<string, JobLite>();

  test('a pending request is read back so a reload shows it', () => {
    const j = train({
      metadata: {
        cancel_requested: { by: 'emp-007', reason: 'stalled', at: '2026-09-07T21:00:00Z' },
      },
    });
    const r = toTrainRow(j, none, false);
    expect(r.cancelRequested).toEqual({
      by: 'emp-007',
      reason: 'stalled',
      at: '2026-09-07T21:00:00Z',
    });
    expect(r.cancelRefused).toBe(false);
  });

  test('no stamp, no request', () => {
    expect(toTrainRow(train({ metadata: {} }), none, false).cancelRequested).toBeNull();
    expect(toTrainRow(train({ metadata: null }), none, false).cancelRequested).toBeNull();
  });

  test('a malformed stamp is not a request', () => {
    expect(
      toTrainRow(train({ metadata: { cancel_requested: 'yes' } }), none, false).cancelRequested,
    ).toBeNull();
  });

  test("the conductor's refusal is read back too — the chip must not claim a cancel that was refused", () => {
    const j = train({
      metadata: {
        cancel_requested: { by: 'emp-007', reason: 'stalled', at: '2026-09-07T21:00:00Z' },
        cancel_refused: { reason: 'already merged', at: '2026-09-07T21:04:00Z' },
      },
    });
    expect(toTrainRow(j, none, false).cancelRefused).toBe(true);
  });
});

// ---------------------------------------------------------------------
// The yard floor's inputs (the-yard-is-a-floor-you-can-follow). A wagon
// keeps its identity across stations only if the page can name the car
// behind a gating branch, read a head to paint beside the branch, and
// tell a lost verdict from a red one.
// ---------------------------------------------------------------------

describe('a car carries its head', () => {
  test('boarded_head names it, shortened to seven', () => {
    const j = ship('fix/x', { metadata: { branch: 'fix/x', boarded_head: '9fa9a19d4b7aac0a5a59c5de88b2a817df4b0889' } });
    expect(assembleYard([], [j]).cars[0]?.head).toBe('9fa9a19');
  });

  test('before boarding, the gate receipt on the packet names it', () => {
    const j = ship('fix/x', {
      steps: [{ spec_slug: 'gate', title: 'Gate', status: 'completed',
        metadata: { receipt: JSON.stringify({ verdict: 'green', head: 'e5aeec53a545151b99c985704070272c049c37ec' }) } }],
    });
    expect(assembleYard([], [j]).cars[0]?.head).toBe('e5aeec5');
  });

  test('no head on record is null — never a fabricated sha', () => {
    const j = ship('fix/x', {
      steps: [{ spec_slug: 'gate', title: 'Gate', status: 'completed', metadata: { receipt: 'not json' } }],
    });
    expect(assembleYard([], [j]).cars[0]?.head).toBeNull();
    expect(assembleYard([], [ship('fix/y')]).cars[0]?.head).toBeNull();
  });
});

describe('the yard names every open car', () => {
  test('cars = every open ship-a-change with a branch, so a gating branch can be matched to its car', () => {
    const open = ship('fix/a');
    const closed = ship('fix/b', { status: 'closed' });
    const branchless = ship('', { id: 'nobranch', metadata: {} });
    const y = assembleYard([], [open, closed, branchless]);
    expect(y.cars.map(c => c.id)).toEqual(['car-fix/a']);
  });
});

