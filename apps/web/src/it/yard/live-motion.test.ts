import { describe, expect, it } from 'bun:test';
import {
  BURST_MS,
  EMPTY,
  LIVE_TEXT,
  MAX_DOTS,
  PING_MS,
  TRANSIT_MS,
  admit,
  busy,
  dotAt,
  moreOf,
  parseMove,
  parseResync,
  returned,
  step,
  type Geometry,
  type Motion,
  type Move,
} from './live-motion';
import { transitGeometry } from './live-geometry';
import { SECTIONS, STATIONS } from './transit';

/** A two-station line and nothing else, so each rule reads in isolation. */
const geometry: Geometry = {
  path: (from, to) => (from === 'a' && to === 'b' ? 'M0 0 H100' : null),
  stationAt: (name) => (name === 'a' ? { x: 0, y: 0 } : name === 'b' ? { x: 100, y: 0 } : name === 'c' ? { x: 0, y: 100 } : null),
  lineOf: (from, to) => (from === 'a' && to === 'b' ? 'delivery' : null),
};

let seq = 0;
const move = (over: Partial<Move> = {}): Move => ({
  seq: ++seq,
  at: '2026-09-26T03:18:00Z',
  packet: `p-${seq}`,
  kind: 'ship-a-change',
  label: 'feat/a-thing',
  from: 'a',
  to: 'b',
  declared: true,
  cause_event_id: '00000000-0000-0000-0000-000000000001',
  cause_seq: 101,
  cause_kind: 'jobs.step.updated',
  handoff_from: null,
  lineage: null,
  aboard: [],
  ...over,
});

const live = { hidden: false, reduced: false };
const at = (now: number) => ({ now, ...live });

describe('the wire (crates/core/boss-jobs/src/moves.rs Recorded, flattened)', () => {
  it('reads a move frame as the stream sends it', () => {
    const m = parseMove(
      JSON.stringify({
        seq: 7, at: '2026-09-26T03:18:00Z', packet: 'car-1', kind: 'ship-a-change', label: 'fix/x',
        from: 'gates', to: 'dock', declared: true, cause_event_id: 'e', cause_seq: 104, cause_kind: 'jobs.job.created',
        handoff_from: 'gate-1', lineage: 'branch', aboard: [],
      }),
    );
    expect(m?.seq).toBe(7);
    expect(m?.handoff_from).toBe('gate-1');
  });
  it('an off-ramp is to: null, an unjudged route is declared: null', () => {
    const m = parseMove(JSON.stringify({ ...move(), to: null, declared: null }));
    expect(m?.to).toBeNull();
    expect(m?.declared).toBeNull();
  });
  it('refuses a frame that is not a move rather than drawing half of one', () => {
    for (const bad of ['', 'not json', '[]', JSON.stringify({ seq: 'x' }), JSON.stringify({ ...move(), aboard: 'x' })]) {
      expect(parseMove(bad)).toBeNull();
    }
  });
  it('reads the resync frame', () => {
    expect(parseResync(JSON.stringify({ frame: 'resync', seq: 9, reason: 'connected' }))).toEqual({ seq: 9, reason: 'connected' });
    expect(parseResync('{}')).toBeNull();
  });
});

describe('one move is one transit, then one ping', () => {
  it('a move sets off a dot along its route, and on arrival the station pings', () => {
    const m1 = admit(EMPTY, move(), geometry, at(0));
    expect(m1.dots).toHaveLength(1);
    expect(m1.pings).toHaveLength(0);
    expect(dotAt(m1.dots[0]!, 0).x).toBe(0);
    expect(dotAt(m1.dots[0]!, TRANSIT_MS / 2).x).toBeCloseTo(50);
    const arrived = step(m1, TRANSIT_MS);
    expect(arrived.dots).toHaveLength(0);
    expect(arrived.pings).toEqual([expect.objectContaining({ station: 'b', k: 1, line: 'delivery' })]);
    // The ping swells once and is gone.
    expect(step(arrived, TRANSIT_MS + PING_MS).pings).toHaveLength(0);
    expect(busy(step(arrived, TRANSIT_MS + PING_MS))).toBe(false);
  });

  it('every section takes the same time on screen, whatever its length', () => {
    const d = admit(EMPTY, move(), geometry, at(0)).dots[0]!;
    expect(d.end - d.start).toBe(TRANSIT_MS);
  });

  it('a burst of arrivals inside a second is one ping carrying ×k', () => {
    const three = [0, 1, 2].reduce<Motion>((m) => admit(m, move(), geometry, at(0)), EMPTY);
    const landed = step(three, TRANSIT_MS);
    expect(landed.pings).toHaveLength(1);
    expect(landed.pings[0]!.k).toBe(3);
    // Past the burst window the next arrival pings on its own.
    const later = step(admit(landed, move(), geometry, at(BURST_MS + 1)), TRANSIT_MS + BURST_MS + 1);
    expect(later.pings.filter((p) => p.station === 'b')).toHaveLength(1);
    expect(later.pings.at(-1)!.k).toBe(1);
  });

  it('a step that removes nothing hands back the same value, so the page does not re-render every frame', () => {
    const m1 = admit(EMPTY, move(), geometry, at(0));
    expect(step(m1, 10)).toBe(m1);
  });
});

describe('at most forty dots', () => {
  it('41 moves are 40 dots and "+1" on the section', () => {
    const m = Array.from({ length: 41 }).reduce<Motion>((acc) => admit(acc, move(), geometry, at(0)), EMPTY);
    expect(MAX_DOTS).toBe(40);
    expect(m.dots).toHaveLength(40);
    expect(moreOf(m)).toEqual([expect.objectContaining({ key: 'a→b', n: 1 })]);
    // The 41st arrives at once: a ping with no transit.
    expect(m.pings).toEqual([expect.objectContaining({ station: 'b', k: 1 })]);
    // The "+1" stands as long as its crossing would have.
    expect(moreOf(step(m, TRANSIT_MS))).toEqual([]);
  });
});

describe('where a move goes', () => {
  it('an undeclared move rides the dashed-red undeclared route, a straight line where no route is drawn', () => {
    const d = admit(EMPTY, move({ from: 'c', to: 'b', declared: false }), geometry, at(0)).dots[0]!;
    expect(d.undeclared).toBe(true);
    expect(d.d).toBe('M0 100 L100 0');
  });

  it('a declared move the geometry has no line for still travels, and is not drawn red', () => {
    const d = admit(EMPTY, move({ from: 'c', to: 'b', declared: true }), geometry, at(0)).dots[0]!;
    expect(d.undeclared).toBe(false);
  });

  it('a move to an off-ramp (to: null) travels off the map and pings nowhere', () => {
    const m = admit(EMPTY, move({ to: null }), geometry, at(0));
    const d = m.dots[0]!;
    expect(d.off).toBe(true);
    expect(dotAt(d, TRANSIT_MS).y).toBeGreaterThan(400);
    // It fades as it leaves.
    expect(dotAt(d, TRANSIT_MS).opacity).toBeLessThan(0.1);
    expect(step(m, TRANSIT_MS).pings).toHaveLength(0);
  });

  it('a packet filed onto the map (from: null) with no link pings where it lands, and nothing travels', () => {
    const m = admit(EMPTY, move({ from: null }), geometry, at(0));
    expect(m.dots).toHaveLength(0);
    expect(m.pings).toEqual([expect.objectContaining({ station: 'b' })]);
  });

  it('a hand-off is one continuous stroke: the old identity fades where it stood, the new one sets off from there', () => {
    const d = admit(EMPTY, move({ handoff_from: 'gate-1', lineage: 'branch' }), geometry, at(0)).dots[0]!;
    expect(d.handoff).toEqual({ x: 0, y: 0 });
    expect(admit(EMPTY, move(), geometry, at(0)).dots[0]!.handoff).toBeNull();
  });

  it('a train carries one pip per car aboard', () => {
    const d = admit(EMPTY, move({ kind: 'pr-train', aboard: ['c1', 'c2', 'c3'] }), geometry, at(0)).dots[0]!;
    expect(d.pips).toBe(3);
  });

  it('a station the map cannot place draws nothing rather than a dot at the origin', () => {
    const m = admit(EMPTY, move({ from: 'nowhere', to: 'elsewhere' }), geometry, at(0));
    expect(m.dots).toHaveLength(0);
    expect(m.pings).toHaveLength(0);
  });
});

describe('a hidden tab draws nothing', () => {
  it('moves while hidden only count, and on return each station pings once with its count', () => {
    const hidden = { now: 0, hidden: true, reduced: false };
    const m = [move(), move(), move({ from: 'b', to: null })].reduce<Motion>((acc, mv) => admit(acc, mv, geometry, hidden), EMPTY);
    expect(m.dots).toHaveLength(0);
    expect(m.pings).toHaveLength(0);
    expect(busy(m)).toBe(false);
    const back = returned(m, geometry, 50_000);
    expect(back.away).toBe(3);
    expect(back.motion.pings).toEqual([
      expect.objectContaining({ station: 'b', k: 3 }),
    ]);
    expect(back.motion.dots).toHaveLength(0);
    expect(returned(back.motion, geometry, 50_001).away).toBe(0);
  });
});

describe('reduced motion', () => {
  it('a move is a static count flash at its station, with no transit', () => {
    const m = admit(EMPTY, move(), geometry, { now: 0, hidden: false, reduced: true });
    expect(m.dots).toHaveLength(0);
    expect(m.pings).toEqual([expect.objectContaining({ station: 'b', still: true })]);
  });
});

describe('the transit map wiring (until car R3 serves the routes)', () => {
  it('every section of the transit map is a route, and every station a place', () => {
    for (const s of SECTIONS) expect(transitGeometry.path(s.from, s.to)).toBe(s.d);
    for (const s of STATIONS) expect(transitGeometry.stationAt(s.name)).toEqual({ x: s.x, y: s.y });
    expect(transitGeometry.lineOf?.('arrivals', 'publish')).toBe('publish');
  });
  it('says what a dot is, always', () => {
    expect(LIVE_TEXT).toContain('each dot is one packet that moved at the time on its label');
    expect(LIVE_TEXT).toContain('6 s');
  });
});
