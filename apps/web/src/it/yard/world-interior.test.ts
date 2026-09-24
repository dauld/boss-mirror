import { describe, expect, it } from 'bun:test';
import { territoryOf } from './world';
import { regionCanvas } from './region-canvas';
import { MACHINERY_STRIP_H, machineryStrip } from './world-machines';
import type { Machine } from './regions';
import {
  PLATFORM_REGIONS,
  crewPlatforms,
  hasPlatforms,
  marshallingPlatforms,
  platformLayout,
  receivingPlatforms,
  type Platform,
} from './world-interior';
import type { AgentRun, Crew, Session } from '../crew/crew';
import type { Siding } from '../marshalling/marshalling';
import type { InboundRow } from '../receiving/receiving';

// THE TWO TERRITORIES WHOSE INTERIOR IS PLATFORMS (design d2154293,
// car 4). Receiving and marshalling hold queues, not rolling stock in
// transit, so their interior is a platform per queue with the packets
// standing on it — and every number on a platform is one the region's
// own read model already derived. These pin the arithmetic: which
// platforms a region has, what each one says, and where the marks go
// inside the outline. Nothing here touches a DOM, so a platform that
// lies is a failing unit test before it is a picture.

const siding = (over: Partial<Siding> & Pick<Siding, 'station' | 'depth'>): Siding => ({
  kind: 'batch',
  wipLimit: null,
  overLimit: false,
  oldestAgeDays: null,
  capabilityRoles: null,
  flow: { kind: 'counted', arrived: 0, served: 0, net: 0 },
  ...over,
});

const inbound = (over: Partial<InboundRow> & Pick<InboundRow, 'id' | 'openedOn'>): InboundRow => ({
  kind: 'backlog-item',
  title: `packet ${over.id}`,
  status: 'open',
  closedOn: null,
  priority: 'standard',
  channel: 'feedback',
  channelBasis: 'recorded',
  ready: [],
  ...over,
});

const byName = (platforms: ReadonlyArray<Platform>, name: string): Platform =>
  platforms.find((p) => p.name === name)!;

describe('which regions have platforms', () => {
  it('is receiving, marshalling and the shop floor, and nothing else', () => {
    expect([...PLATFORM_REGIONS]).toEqual(['receiving', 'marshalling', 'shop-floor']);
    expect(hasPlatforms('receiving')).toBe(true);
    expect(hasPlatforms('marshalling')).toBe(true);
    expect(hasPlatforms('shop-floor')).toBe(true);
    expect(hasPlatforms('dock')).toBe(false);
    expect(hasPlatforms('nowhere')).toBe(false);
  });
});

describe('the marshalling interior — one platform per station', () => {
  it('carries the depth, the WIP bound and what left in the window', () => {
    const p = marshallingPlatforms(
      [
        siding({
          station: 'loading-dock',
          depth: 4,
          wipLimit: 24,
          oldestAgeDays: 1,
          flow: { kind: 'counted', arrived: 9, served: 7, net: 2 },
        }),
      ],
      24,
    );
    expect(p).toHaveLength(1);
    expect(p[0]!.name).toBe('loading-dock');
    expect(p[0]!.standing).toBe(4);
    expect(p[0]!.bound).toBe(24);
    expect(p[0]!.rate).toBe(7);
    expect(p[0]!.flag.n).toBe(0);
    expect(p[0]!.note).toContain('oldest 1 d');
    expect(p[0]!.note).toContain('7 left in 24h');
  });

  it('flags the wagons standing PAST the station bound, from the tail', () => {
    const p = marshallingPlatforms([siding({ station: 'q.task', depth: 30, wipLimit: 24 })], 24);
    expect(p[0]!.flag).toEqual({ from: 'tail', n: 6 });
  });

  it('a rate the log could not count is unknown, never nought', () => {
    const p = marshallingPlatforms(
      [
        siding({
          station: 'my-watchlist',
          depth: 3,
          flow: { kind: 'unavailable', reason: 'the dock is a Job-metadata clause' },
        }),
      ],
      24,
    );
    // The DEPTH was read, so it stands; the RATE was not, so it is
    // null — the platform renders a `?` there rather than a 0, which
    // on a queue board reads as "nothing is being worked".
    expect(p[0]!.standing).toBe(3);
    expect(p[0]!.rate).toBeNull();
    expect(p[0]!.note).toContain('the dock is a Job-metadata clause');
  });

  it('keeps the read model order — the queues under pressure first', () => {
    const p = marshallingPlatforms(
      [siding({ station: 'first', depth: 9 }), siding({ station: 'second', depth: 0 })],
      24,
    );
    expect(p.map((x) => x.name)).toEqual(['first', 'second']);
  });
});

describe('the receiving interior — one platform per channel', () => {
  const today = '2026-09-20';
  const days = ['2026-09-19', '2026-09-20'];

  it('stands the open packets of each channel and counts what left in the window', () => {
    const p = receivingPlatforms(
      [
        inbound({ id: 'a', openedOn: '2026-09-19', channel: 'feedback' }),
        inbound({ id: 'b', openedOn: '2026-09-20', channel: 'feedback' }),
        inbound({ id: 'c', openedOn: '2026-09-01', channel: 'feedback', status: 'closed', closedOn: '2026-09-20' }),
      ],
      today,
      days,
    );
    const feedback = byName(p, 'feedback');
    expect(feedback.standing).toBe(2);
    expect(feedback.rate).toBe(1);
    expect(feedback.bound).toBeNull();
    expect(feedback.note).toContain('oldest 1 d');
  });

  it('flags the packets past the stale band, from the head — the oldest stand at the front', () => {
    const p = receivingPlatforms(
      [
        inbound({ id: 'old', openedOn: '2026-08-01', channel: 'design' }),
        inbound({ id: 'new', openedOn: '2026-09-20', channel: 'design' }),
      ],
      today,
      days,
    );
    const design = byName(p, 'design');
    expect(design.standing).toBe(2);
    expect(design.flag).toEqual({ from: 'head', n: 1 });
  });

  it('names every channel, even an empty one — a track nothing stands on is an answer', () => {
    const p = receivingPlatforms([], today, days);
    expect(p.map((x) => x.name)).toContain('unrecorded');
    expect(p.every((x) => x.standing === 0)).toBe(true);
  });

  it('puts the channels holding flagged work first, then the deepest', () => {
    const p = receivingPlatforms(
      [
        inbound({ id: 'a', openedOn: '2026-09-20', channel: 'session' }),
        inbound({ id: 'b', openedOn: '2026-09-20', channel: 'session' }),
        inbound({ id: 'c', openedOn: '2026-08-01', channel: 'monitoring' }),
      ],
      today,
      days,
    );
    expect(p[0]!.name).toBe('monitoring');
    expect(p[1]!.name).toBe('session');
  });
});

describe('where the platforms go inside the outline', () => {
  const t = territoryOf('marshalling')!;
  const platform = (name: string, standing: number | null, bound: number | null = null): Platform => ({
    key: name,
    name,
    standing,
    bound,
    rate: 0,
    flag: { from: 'tail', n: 0 },
    note: '',
  });

  it('stacks them under the zoomed header, inside the rect', () => {
    const out = platformLayout(t, [platform('a', 2), platform('b', 3)]);
    expect(out.placed).toHaveLength(2);
    expect(out.hidden).toBe(0);
    const [a, b] = out.placed;
    expect(a!.y).toBeGreaterThan(t.y);
    expect(b!.y).toBeGreaterThan(a!.y);
    expect(b!.y + b!.h).toBeLessThanOrEqual(t.y + t.h);
    expect(a!.x).toBeGreaterThanOrEqual(t.x);
    expect(a!.x + a!.w).toBeLessThanOrEqual(t.x + t.w);
  });

  it('counts the platforms that did not fit rather than dropping them', () => {
    const many = Array.from({ length: 40 }, (_, i) => platform(`s${i}`, 1));
    const out = platformLayout(t, many);
    expect(out.placed.length).toBeGreaterThan(0);
    expect(out.placed.length).toBeLessThan(40);
    expect(out.placed.length + out.hidden).toBe(40);
  });

  it('stands one mark per packet, and scales the track when the queue is longer than it', () => {
    const fits = platformLayout(t, [platform('shallow', 5)]).placed[0]!;
    expect(fits.marks).toHaveLength(5);
    expect(fits.perMark).toBe(1);
    expect(fits.overflow).toBe(0);
    const deep = platformLayout(t, [platform('deep', 400)]).placed[0]!;
    expect(deep.marks.length).toBeGreaterThan(0);
    expect(deep.marks.length + deep.overflow).toBe(400);
    // The whole queue is still drawn, at a scale the renderer states —
    // a full track under a count of 400 is a scale, not a miscount.
    expect(deep.perMark).toBeGreaterThan(1);
    expect(deep.marks.every((w) => w.x >= t.x && w.x + w.w <= t.x + t.w)).toBe(true);
  });

  it('keeps the bound on the picture for a queue past it — the case that matters most', () => {
    // A 30-deep station against a bound of 24 has a longer queue than
    // the track has marks. Drawn as marks that stop at the track end,
    // the bound would fall off the picture exactly where an operator
    // needs it; the track is the WHOLE queue instead.
    const over = platformLayout(t, [{ ...platform('q.task', 30, 24), flag: { from: 'tail', n: 6 } }]).placed[0]!;
    expect(over.boundX).not.toBeNull();
    expect(over.marks.some((m) => m.flagged)).toBe(true);
    expect(over.marks.every((m) => m.flagged)).toBe(false);
  });

  it('draws NO marks for a count nobody could take — an unknown is not an empty track', () => {
    const out = platformLayout(t, [platform('blind', null)]);
    const p = out.placed[0]!;
    expect(p.marks).toHaveLength(0);
    expect(p.overflow).toBe(0);
    // And it is distinguishable from a real zero by the platform
    // itself, which the renderer reads: null prints `?`, 0 prints 0.
    expect(p.platform.standing).toBeNull();
    expect(platformLayout(t, [platform('empty', 0)]).placed[0]!.platform.standing).toBe(0);
  });

  it('puts the bound marker where it falls on the track, and nowhere when it is off the end', () => {
    const onTrack = platformLayout(t, [platform('a', 10, 4)]).placed[0]!;
    expect(onTrack.boundX).not.toBeNull();
    expect(onTrack.boundX!).toBeGreaterThan(onTrack.x);
    expect(onTrack.boundX!).toBeLessThan(onTrack.x + onTrack.w);
    // A queue shorter than its bound has nothing over it: a marker at
    // the end of the track would say something the numbers do not.
    const offTrack = platformLayout(t, [platform('a', 2, 900)]).placed[0]!;
    expect(offTrack.boundX).toBeNull();
  });

  it('flags the marks the flag names, from the end it names', () => {
    const tail = platformLayout(t, [{ ...platform('a', 6, 4), flag: { from: 'tail', n: 2 } }]).placed[0]!;
    expect(tail.marks.map((w) => w.flagged)).toEqual([false, false, false, false, true, true]);
    const head = platformLayout(t, [{ ...platform('a', 3), flag: { from: 'head', n: 1 } }]).placed[0]!;
    expect(head.marks.map((w) => w.flagged)).toEqual([true, false, false]);
  });

  // THE MACHINERY STRIP IS NOT THE PLATFORMS' TO USE (backlog
  // 3a916816). `interiorLayout` subtracted it and `platformLayout` did
  // not — world-interior.ts did not import the height at all — so the
  // two functions dividing the same canvas disagreed about where its
  // bottom is. Every platform region (receiving, marshalling and the
  // shop floor) draws machinery too, so this is measured against the
  // glyphs themselves rather than against a boundary the test restates.
  it('reserves the machinery strip, so a platform and a machine glyph never share pixels', () => {
    const many = Array.from({ length: 40 }, (_, i) => platform(`s${i}`, 3));
    const machines: ReadonlyArray<Machine> = Array.from({ length: 6 }, (_, i) => ({
      id: `m${i}`,
      name: `machine ${i}`,
      state: 'running',
      why: 'at work',
    }));
    for (const rect of [t, regionCanvas('marshalling')]) {
      const { placed } = platformLayout(rect, many);
      const strip = machineryStrip(rect, machines);
      expect(placed.length, rect.name).toBeGreaterThan(0);
      expect(strip.placed.length, rect.name).toBe(machines.length);
      for (const p of placed) {
        expect(p.y + p.h, `${rect.name}/${p.platform.name}`).toBeLessThanOrEqual(
          rect.y + rect.h - MACHINERY_STRIP_H,
        );
        for (const g of strip.placed) {
          const apart = p.x + p.w <= g.x || g.x + g.w <= p.x || p.y + p.h <= g.y || g.y + g.h <= p.y;
          expect(apart, `${rect.name}: ${p.platform.name} sits on ${g.machine.id}`).toBe(true);
        }
      }
    }
  });
});

// ---------------------------------------------------------------------
// THE SHOP FLOOR'S PLATFORMS (backlog 94c6ffd0) — a crew is a platform,
// its open runs are what stands on it.
// ---------------------------------------------------------------------

describe('crewPlatforms — a platform per crew, the busiest first', () => {
  const session = (over: Partial<Session> = {}): Session => ({
    id: 's1',
    title: 'a session',
    actor: 'claude@algedonic.dev',
    host: 'boss-dev',
    cwd: '/work/boss',
    startedAt: '2026-09-22T09:00:00Z',
    lastActiveAt: '2026-09-22T09:30:00Z',
    promptCount: 12,
    untrackedRuns: 0,
    ...over,
  });
  const run = (id: string): AgentRun =>
    ({ id, title: 'a run', packet: null, step: null, agent: null, model: null,
       budgetUsd: null, effort: null, host: null, at: 'building', openedAt: null,
       session: null }) as AgentRun;

  it('stands the runs on their crew, busiest first, and never invents a rate', () => {
    const busy: Crew = { session: session({ id: 's1', actor: 'a@x' }), runs: [run('r1'), run('r2')], idle: false };
    const quiet: Crew = { session: session({ id: 's2', actor: 'b@x', promptCount: 3 }), runs: [], idle: true };
    const platforms = crewPlatforms([quiet, busy], []);
    expect(platforms.map((p) => p.name)).toEqual(['a@x · s1', 'b@x · s2']);
    expect(platforms[0]!.standing).toBe(2);
    // What a crew FINISHED in the window is not in this read, so the
    // rate is unknown — a 0 would say the crew shipped nothing.
    expect(platforms[0]!.rate).toBeNull();
    expect(platforms[0]!.note).toContain('at work');
    expect(platforms[1]!.note).toContain('idle');
  });

  // THE LIVE SHAPE (backlog 846ab934, measured 2026-09-24 by the IT map
  // review): three crews on the floor, every one of them the SAME actor,
  // `claude@algedonic.dev` — one agent identity, many sessions, which is
  // the normal case here. The platforms were named AND keyed by actor,
  // so the region map's keyed each threw each_key_duplicate and the
  // shop floor never drew. A crew is a SESSION; its key says which.
  it('keys and names two sessions of ONE actor as two platforms — a crew is a session, not an actor', () => {
    const one: Crew = { session: session({ id: '87b3cf48-f17b-4126-98b4-7673a3c3576b' }), runs: [run('r1')], idle: false };
    const two: Crew = { session: session({ id: '7bb6e37d-d168-40b8-9675-70a42229a2bb' }), runs: [], idle: true };
    const platforms = crewPlatforms([one, two], [run('r9')]);
    expect(platforms.map((p) => p.key)).toEqual([
      'session:87b3cf48-f17b-4126-98b4-7673a3c3576b',
      'session:7bb6e37d-d168-40b8-9675-70a42229a2bb',
      'no session',
    ]);
    // The label still says WHO, and tells the two apart by session.
    expect(platforms[0]!.name).toBe('claude@algedonic.dev · 87b3cf48');
    expect(platforms[1]!.name).toBe('claude@algedonic.dev · 7bb6e37d');
    expect(new Set(platforms.map((p) => p.name)).size).toBe(platforms.length);
  });

  it('keys a queue platform by the queue it is, so the region map keys never collide', () => {
    const marshalled = marshallingPlatforms([siding({ station: 'q.a', depth: 1 }), siding({ station: 'q.b', depth: 2 })], 24);
    expect(marshalled.map((p) => p.key).sort()).toEqual(['q.a', 'q.b']);
    const received = receivingPlatforms([], '2026-09-24', []);
    expect(received.map((p) => p.key)).toEqual(received.map((p) => p.name));
  });

  it('says so when a crew\'s silence could not be judged', () => {
    const unknown: Crew = { session: session(), runs: [], idle: null };
    expect(crewPlatforms([unknown], [])[0]!.note).toContain('silence not measured');
  });

  it('gives the runs no session claims a platform of their own rather than dropping them', () => {
    const platforms = crewPlatforms([], [run('r9')]);
    expect(platforms).toHaveLength(1);
    expect(platforms[0]!.name).toBe('no session');
    expect(platforms[0]!.standing).toBe(1);
    expect(platforms[0]!.flag).toEqual({ from: 'tail', n: 1 });
  });
});
