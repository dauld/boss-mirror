import { describe, expect, it } from 'bun:test';
import { regionCanvas } from './region-canvas';
import { CHAR_W, MIN_FLOOR_H, actorLayout, actors, ageText, fitText, runsDrawn, type Actor } from './shop-floor';
import { SILENT_BOUND_HOURS, type AgentRun, type Crew, type Session } from '../crew/crew';

// THE SHOP FLOOR DRAWS AGENTS AS ACTORS (design 62de32ae, decision 8;
// car E on backlog c3105b2a). Measured 2026-09-24: every session on the
// floor was `claude@algedonic.dev`, and five builder runs stood on one
// of them — the floor drew a platform per session with the runs as
// anonymous marks, so which run was building what, and for how long,
// was nowhere on the picture. These pin the model under the drawing:
// one lamp per session and per run, each labelled and aged, grouped
// under the identity they share.

const NOW = '2026-09-24T15:40:00Z';

const session = (over: Partial<Session> = {}): Session => ({
  id: '2a718af7-5dbb-4fe4-91a8-b5183280da80',
  title: 'Session: claude@algedonic.dev on boss-dev-xl6cv',
  actor: 'claude@algedonic.dev',
  host: 'boss-dev-xl6cv',
  cwd: '/work/boss',
  startedAt: '2026-09-24T11:14:17Z',
  lastActiveAt: '2026-09-24T15:32:49Z',
  promptCount: 133,
  untrackedRuns: 0,
  ...over,
});

const run = (over: Partial<AgentRun> & Pick<AgentRun, 'id'>): AgentRun => ({
  title: 'builder run: Feedback on /it: the regions map should be a world map',
  packet: 'c3105b2a-001d-4ae4-a172-5b7f8aba2598',
  step: 'build',
  agent: 'claude@algedonic.dev',
  model: 'opus-5[1m]',
  budgetUsd: 5,
  effort: 'high',
  host: 'boss-dev-xl6cv',
  at: 'Building (active)',
  openedAt: '2026-09-24T15:29:23Z',
  session: '2a718af7-5dbb-4fe4-91a8-b5183280da80',
  lastMovedAt: '2026-09-24T15:29:23Z',
  building: true,
  ...over,
});

describe('ageText — an age as the lamp prints it', () => {
  it('reads minutes, then hours and minutes, then days', () => {
    expect(ageText(0)).toBe('0m');
    expect(ageText(11 * 60_000)).toBe('11m');
    expect(ageText((4 * 60 + 41) * 60_000)).toBe('4h 41m');
    expect(ageText(3 * 3_600_000)).toBe('3h');
    expect(ageText(50 * 3_600_000)).toBe('2d');
  });
});

describe('actors — one lamp per session and per run, under the identity they share', () => {
  it('groups two sessions of ONE identity under it, and labels each run with its run and its item', () => {
    const working: Crew = {
      session: session(),
      runs: [run({ id: 'fa4014f2-1794-4268-9881-f4e616f7d0ad' }), run({ id: 'eb49bf92-0000', packet: '6f471ff6-68f6', openedAt: '2026-09-24T15:16:13Z' })],
      idle: false,
    };
    const resting: Crew = {
      session: session({ id: '6bcc486a-0000', host: 'boss-dev-vh9fw', lastActiveAt: '2026-09-24T10:51:00Z' }),
      runs: [],
      idle: true,
    };
    const out = actors([resting, working], [], NOW);
    expect(out).toHaveLength(1);
    const a = out[0]!;
    expect(a.identity).toBe('claude@algedonic.dev');
    expect(a.runs).toBe(2);
    // The session at work leads, the idle one follows.
    expect(a.sessions.map((s) => s.key)).toEqual([
      'session:2a718af7-5dbb-4fe4-91a8-b5183280da80',
      'session:6bcc486a-0000',
    ]);
    const [busy, idle] = a.sessions;
    expect(busy!.lamp).toBe('at-work');
    expect(busy!.label).toBe('session 2a718af7 · boss-dev-xl6cv');
    expect(busy!.age).toBe('at work · last prompt 7m ago');
    expect(idle!.lamp).toBe('idle');
    expect(idle!.age).toBe('idle 4h 49m');
    // Each run: its own id, the item it builds and the step, and how
    // long it has been at it — the oldest first.
    expect(busy!.runs.map((r) => r.label)).toEqual(['run eb49bf92 → 6f471ff6 build', 'run fa4014f2 → c3105b2a build']);
    expect(busy!.runs[1]!.lamp).toBe('at-work');
    expect(busy!.runs[1]!.age).toBe('at work 10m');
    expect(busy!.runs[1]!.title).toBe('Feedback on /it: the regions map should be a world map');
    expect(runsDrawn(out)).toBe(2);
  });

  it('keys every lamp uniquely even when every session is the same identity — the 846ab934 crash', () => {
    const crews: ReadonlyArray<Crew> = ['a', 'b', 'c'].map((id) => ({
      session: session({ id: `${id}-session` }),
      runs: [run({ id: `${id}-run`, session: `${id}-session` })],
      idle: false,
    }));
    const keys = actors(crews, [], NOW).flatMap((a) => a.sessions.flatMap((s) => [s.key, ...s.runs.map((r) => r.key)]));
    expect(new Set(keys).size).toBe(keys.length);
    expect(keys).toHaveLength(6);
  });

  it('a session with no heartbeat is unknown, never idle and never at work', () => {
    const unmeasured: Crew = { session: session({ lastActiveAt: null, startedAt: null }), runs: [], idle: null };
    const s = actors([unmeasured], [], NOW)[0]!.sessions[0]!;
    expect(s.lamp).toBe('unknown');
    expect(s.age).toBe('silence not measured — no heartbeat on the packet');
  });

  it('a run unmoved past the age-out bound is silent, and says the bound', () => {
    const stale = run({ id: 'dead0000', openedAt: '2026-09-24T09:00:00Z', lastMovedAt: '2026-09-24T09:00:00Z' });
    const s = actors([{ session: session(), runs: [stale], idle: false }], [], NOW)[0]!.sessions[0]!;
    expect(s.runs[0]!.lamp).toBe('silent');
    // The crew board's own words for it (crew.ts `silenceText`).
    expect(s.runs[0]!.age).toBe(`6.7h unmoved — past the ${SILENT_BOUND_HOURS}h bound`);
  });

  it('a run past its build says where it stands and since when, and is not at work', () => {
    const built = run({ id: 'b0000000', building: false, at: 'Report recorded (ready, not yet done)', lastMovedAt: '2026-09-24T15:37:00Z' });
    const r = actors([{ session: session(), runs: [built], idle: false }], [], NOW)[0]!.sessions[0]!.runs[0]!;
    expect(r.lamp).toBe('waiting');
    expect(r.age).toBe('Report recorded (ready, not yet done) · 3m');
  });

  it('draws the runs no open session claims under their own identity, rather than dropping them', () => {
    const orphan = run({ id: '0ce8157f-0000', session: null, agent: 'agent-claude' });
    const out = actors([{ session: session(), runs: [], idle: false }], [orphan], NOW);
    expect(out.map((a) => a.identity)).toEqual(['agent-claude', 'claude@algedonic.dev']);
    const none = out[0]!.sessions[0]!;
    expect(none.key).toBe('session:none:agent-claude');
    expect(none.lamp).toBe('unknown');
    expect(none.label).toBe('no open session');
    expect(none.runs.map((r) => r.key)).toEqual(['run:0ce8157f-0000']);
    expect(runsDrawn(out)).toBe(1);
  });
});

describe('fitText — a row\'s words cut to its room', () => {
  it('keeps text that fits and cuts the rest with an ellipsis, never past the room', () => {
    expect(fitText('run fa4014f2', 600)).toBe('run fa4014f2');
    const cut = fitText('Feedback on /it: the regions map should be a world map', 10 * CHAR_W);
    expect(cut).toBe('Feedback…');
    expect(cut.length * CHAR_W).toBeLessThanOrEqual(10 * CHAR_W);
  });
});

describe('actorLayout — the rows on the region canvas', () => {
  const two: ReadonlyArray<Actor> = actors(
    [
      { session: session(), runs: [run({ id: 'r1' }), run({ id: 'r2' })], idle: false },
      { session: session({ id: 's2' }), runs: [], idle: true },
    ],
    [],
    NOW,
  );

  it('stacks an identity, its sessions and their runs, each indented under the last, top to bottom', () => {
    const laid = actorLayout(regionCanvas('shop-floor'), two);
    expect(laid.rows.map((r) => r.kind)).toEqual(['actor', 'session', 'run', 'run', 'session']);
    const ys = laid.rows.map((r) => r.y);
    expect([...ys].sort((a, b) => a - b)).toEqual(ys);
    const [actor, sess, first] = laid.rows;
    expect(sess!.x).toBeGreaterThan(actor!.x);
    expect(first!.x).toBeGreaterThan(sess!.x);
    expect(new Set(laid.rows.map((r) => r.key)).size).toBe(laid.rows.length);
  });

  it('grows the canvas rather than hiding a run — a lamp drawn nowhere is the false-empty class', () => {
    const crowd: ReadonlyArray<Crew> = Array.from({ length: 12 }, (_, i) => ({
      session: session({ id: `s${i}` }),
      runs: [run({ id: `r${i}a`, session: `s${i}` }), run({ id: `r${i}b`, session: `s${i}` })],
      idle: false,
    }));
    const t = regionCanvas('shop-floor');
    const laid = actorLayout(t, actors(crowd, [], NOW));
    expect(laid.rows.filter((r) => r.kind === 'run')).toHaveLength(24);
    expect(laid.height).toBeGreaterThan(t.h);
    const last = laid.rows[laid.rows.length - 1]!;
    expect(last.y).toBeLessThan(laid.height);
  });

  it('is as tall as its lamps — a small floor is not drawn in the fixed 420px box, and never a sliver', () => {
    const t = regionCanvas('shop-floor');
    const laid = actorLayout(t, two);
    const last = laid.rows[laid.rows.length - 1]!;
    expect(laid.height).toBeLessThan(t.h);
    expect(laid.height).toBeGreaterThan(last.y);
    expect(actorLayout(t, []).height).toBe(MIN_FLOOR_H);
  });
});
