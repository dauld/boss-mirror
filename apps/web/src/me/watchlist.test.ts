import { afterEach, describe, expect, test } from 'bun:test';
import {
  dismissFromWatchlist,
  outcomeOf,
  watchlistPacket,
  watchlistStateFromResponse,
  windowNote,
  type WatchlistJob,
} from './watchlist';

function job(over: Partial<WatchlistJob> = {}): WatchlistJob {
  return {
    id: 'j1',
    kind: 'user-feedback',
    title: 'The column picker forgets my choice',
    status: 'open',
    opened_on: '2026-08-04',
    closed_on: null,
    tags: ['feedback', 'bug'],
    metadata: { route: '/ux/jobs' },
    simulated: false,
    ...over,
  };
}

function closedAs(outcome: string | null, over: Partial<WatchlistJob> = {}) {
  return job({
    status: 'closed',
    closed_on: '2026-08-12',
    metadata: { route: '/ux/jobs', ...(outcome ? { outcome } : {}) },
    ...over,
  });
}

describe('outcomeOf', () => {
  test('an in-flight packet has no terminal state to show', () => {
    expect(outcomeOf(job())).toBeNull();
    expect(outcomeOf(job({ status: 'draft' }))).toBeNull();
  });

  test('the three feedback terminals read in plain words', () => {
    // The vocabulary comes off the packet (`metadata.outcome`, stamped
    // by the terminal close), never off the workflow kind — a new
    // disposition needs no code change here.
    expect(outcomeOf(closedAs('completed'))?.label).toBe('completed');
    expect(outcomeOf(closedAs('duplicate'))?.label).toBe('duplicate');
    expect(outcomeOf(closedAs('declined'))?.label).toBe('declined');
  });

  test('tone maps onto the DL status tokens, and never invents one', () => {
    // --ok: the filer's report produced a change.
    expect(outcomeOf(closedAs('completed'))?.tone).toBe('ok');
    // --warn: it ended without one. Not an error — a decision the
    // filer should notice.
    expect(outcomeOf(closedAs('declined'))?.tone).toBe('warn');
    // --static: it went somewhere else. Nothing to flag.
    expect(outcomeOf(closedAs('duplicate'))?.tone).toBe('static');
  });

  test('an unfamiliar outcome still shows, quietly', () => {
    // A disposition this lens has never heard of is information, not a
    // reason to render nothing. It reads as itself in the neutral tone.
    const seen = outcomeOf(closedAs('escalated'));
    expect(seen?.label).toBe('escalated');
    expect(seen?.tone).toBe('static');
  });

  test('a closed packet with no recorded outcome says only that it closed', () => {
    // Cancelled packets and pre-outcome closes land here. Saying
    // "closed" is the whole of what is known; inventing "completed"
    // would be a claim the log does not make.
    expect(outcomeOf(closedAs(null))).toEqual({ label: 'closed', tone: 'static' });
    expect(outcomeOf(job({ status: 'cancelled', closed_on: '2026-08-12' }))).toEqual({
      label: 'closed',
      tone: 'static',
    });
  });
});

describe('watchlistPacket', () => {
  test('maps a packet onto the shared card grammar', () => {
    const card = watchlistPacket(job());
    expect(card.id).toBe('j1');
    expect(card.kind).toBe('user-feedback');
    expect(card.title).toBe('The column picker forgets my choice');
    expect(card.sim).toBe(false);
  });

  test('the mono line is where the packet was filed, else when', () => {
    expect(watchlistPacket(job()).branch).toBe('/ux/jobs');
    expect(watchlistPacket(job({ metadata: {} })).branch).toBe('filed 2026-08-04');
  });

  test('a closed packet says when it closed, so the card is legible alone', () => {
    expect(watchlistPacket(closedAs('completed')).branch).toBe('/ux/jobs · closed 2026-08-12');
  });

  test('the outcome is NOT a tag chip — it carries a tone the chips cannot', () => {
    expect(watchlistPacket(closedAs('declined')).tags).not.toContain('declined');
  });

  test('simulated packets stay visibly simulated', () => {
    expect(watchlistPacket(job({ simulated: true })).sim).toBe(true);
    expect(watchlistPacket(job({ tags: ['sim'] })).sim).toBe(true);
  });

  // fb3b5ce1 (2026-09-14): the yard's reader learned `red_trains` and
  // this lens, building its own literal, silently did not. The card is
  // now built through the yard's one constructor, so a fact on the
  // packet reaches this card the day the yard learns to read it.
  test('a packet a red train released reaches the card with its strike count', () => {
    const card = watchlistPacket(job({ metadata: { route: '/ux/jobs', red_trains: 2 } }));
    expect(card.redTrains).toBe(2);
    // The lens still decides its own provenance line and chips.
    expect(card.branch).toBe('/ux/jobs');
    expect(card.tags).toEqual(['bug']);
    // Absent on the record is zero strikes, as on the dock.
    expect(watchlistPacket(job()).redTrains).toBe(0);
  });
});

describe('watchlistStateFromResponse', () => {
  const envelope = (over: Record<string, unknown> = {}) => ({
    station: 'my-watchlist',
    kind: 'actor',
    discipline: ['recency'],
    wip_limit: null,
    over_limit: false,
    terminal_window_days: 14,
    total: 1,
    data: [job()],
    ...over,
  });

  test('the strike count rides the envelope through to the entry', () => {
    const state = watchlistStateFromResponse(200, envelope({
      data: [job({ metadata: { route: '/ux/jobs', red_trains: 1 } })],
    }));
    expect(state.kind).toBe('ready');
    if (state.kind !== 'ready') return;
    expect(state.entries[0]?.card.redTrains).toBe(1);
  });

  test('a served envelope becomes cards in the server’s order', () => {
    const state = watchlistStateFromResponse(200, envelope({
      data: [closedAs('completed'), job()],
      total: 2,
    }));
    expect(state.kind).toBe('ready');
    if (state.kind !== 'ready') return;
    // Ordering is the station's discipline (recency), decided by the
    // registry row. The lens preserves it rather than re-sorting —
    // two orderings of one queue is one too many.
    expect(state.entries.map(e => e.outcome?.label ?? 'open')).toEqual(['completed', 'open']);
    expect(state.windowDays).toBe(14);
  });

  test('an empty watchlist is a state of its own, not an error', () => {
    const state = watchlistStateFromResponse(200, envelope({ data: [], total: 0 }));
    expect(state.kind).toBe('ready');
    if (state.kind !== 'ready') return;
    expect(state.entries).toEqual([]);
  });

  test('404 and 503 mean the station has not reached this deployment', () => {
    // Same reading as the station map: an older cluster or an
    // unconfigured registry is not a failure the page should shout
    // about.
    expect(watchlistStateFromResponse(404, null).kind).toBe('unavailable');
    expect(watchlistStateFromResponse(503, null).kind).toBe('unavailable');
  });

  test('anything else is an error, and a non-envelope body is too', () => {
    expect(watchlistStateFromResponse(500, null).kind).toBe('error');
    expect(watchlistStateFromResponse(200, []).kind).toBe('error');
    expect(watchlistStateFromResponse(200, { nope: true }).kind).toBe('error');
  });
});

describe('windowNote', () => {
  test('says how long a closed packet lingers, in the words the row declared', () => {
    expect(windowNote(14)).toBe('including packets closed in the last 14 days');
    expect(windowNote(1)).toBe('including packets closed in the last day');
  });

  test('a station with no window says nothing about closed packets', () => {
    expect(windowNote(null)).toBeNull();
  });
});

describe('dismissFromWatchlist', () => {
  const realFetch = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = realFetch;
  });

  test('one PATCH to the metadata merge, carrying exactly the one key', async () => {
    // The regression this pins (2026-08-21 UX audit): dismiss used to
    // GET the whole job and PUT it back — a read-modify-write over the
    // ENVELOPE that resurrected a concurrently-closed packet open with
    // its outcome erased. The fix is structural: no read, no full-job
    // write, just the server-side merge with the single key it means.
    const calls: { url: string; method: string; body: string | null }[] = [];
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      calls.push({
        url: String(input),
        method: init?.method ?? 'GET',
        body: typeof init?.body === 'string' ? init.body : null,
      });
      return new Response(null, { status: 204 });
    }) as typeof fetch;

    expect(await dismissFromWatchlist('job-1')).toBe(true);
    expect(calls).toHaveLength(1);
    expect(calls[0]?.method).toBe('PATCH');
    expect(calls[0]?.url).toBe('/api/jobs/job-1/metadata');
    expect(JSON.parse(calls[0]?.body ?? '{}')).toEqual({ watchlist_dismissed: 'true' });
  });

  test('a refused merge reports false rather than pretending', async () => {
    globalThis.fetch = (async (_input: RequestInfo | URL) =>
      new Response('forbidden', { status: 403 })) as typeof fetch;
    expect(await dismissFromWatchlist('job-1')).toBe(false);
  });
});
