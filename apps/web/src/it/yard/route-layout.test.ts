// THE ROUTE LAYOUT — design e765b3fc, car R3. The map draws exactly the
// routes /api/yard/routes serves, laid out by the octilinear router, and
// the moves of car M2 travel the same paths through `routePath`. These
// pin both halves: what is drawn is what is served, and how it is laid.
import { describe, expect, it } from 'bun:test';
import { ROUTES, routesPayload, type FixtureRoute } from '../../../tests/fixtures/yard';
import { ARC_ABOVE, ARC_BELOW, linesOf, octilinear, routePath, sectionKey, sectionsOf } from './route-layout';
import { parseRoutes } from './routes';
import { STATIONS } from './transit';
import { pointAt } from './world-motion';

/** The fixture's routes, as the page parses them off the wire. */
const served = (routes: ReadonlyArray<FixtureRoute> = ROUTES) => parseRoutes(routesPayload(routes));
const laid = (routes?: ReadonlyArray<FixtureRoute>) => sectionsOf(served(routes)).sections;
const find = (key: string) => laid().find((s) => s.key === key)!;
const station = (name: string) => STATIONS.find((s) => s.name === name)!;

// THE EDGES ARE SERVED (design e765b3fc, car R3): the map draws exactly
// the routes /api/yard/routes answers — one drawn element per route,
// no more and no fewer — laid out by the octilinear router.
describe('the map draws exactly the routes the server serves', () => {
  it('one drawn element per served route, keyed by its two ends', () => {
    expect(laid().map((s) => s.key)).toEqual(ROUTES.map((r) => sectionKey(r.from, r.to)));
  });

  it('a route the server starts serving arrives on the map, and one it stops serving leaves it — no web change', () => {
    const more = laid([...ROUTES, { from: 'track', to: 'garage', declared: true, sources: [{ source: 'observed', moves: 1, last_at: '' }] }]);
    expect(more.map((s) => s.key)).toContain('track→garage');
    const fewer = laid(ROUTES.filter((r) => !(r.from === 'gates' && r.to === 'track')));
    expect(fewer.map((s) => s.key)).not.toContain('gates→track');
    expect(fewer).toHaveLength(ROUTES.length - 1);
  });

  it('draws nothing it was not served: no routes, no sections', () => {
    expect(laid([])).toEqual([]);
  });

  it('names a section, an exit and an entry by their ends, the key a move (car M2) finds its path by', () => {
    expect(find('gates→track').kind).toBe('section');
    expect(find('dock→').kind).toBe('exit');
    expect(find('→receiving').kind).toBe('entry');
    expect(sectionKey('dock', null)).toBe('dock→');
    expect(sectionKey(null, 'receiving')).toBe('→receiving');
  });

  it('says, rather than drops, a route to a region this layout has no station for', () => {
    const out = sectionsOf(served([...ROUTES, { from: 'gates', to: 'the-moon', declared: true, sources: [{ source: 'observed', moves: 1, last_at: '' }] }]));
    expect(out.unplaced).toEqual(['gates→the-moon']);
    expect(out.sections).toHaveLength(ROUTES.length);
  });

  it('carries whether a route is declared — observed-only routes are drawn apart (dashed red)', () => {
    expect(find('shed→arrivals').declared).toBe(false);
    expect(find('gates→track').declared).toBe(true);
  });

  it('puts a route on the line of the station it serves off the main line', () => {
    expect(find('receiving→marshalling').line).toBe('delivery');
    expect(find('gates→track').line).toBe('delivery');
    expect(find('gates→garage').line).toBe('siding');
    expect(find('garage→dock').line).toBe('siding');
    expect(linesOf(laid())).toEqual(['delivery', 'siding']);
  });
});

describe('the octilinear router', () => {
  it('runs every section from its from-station to its to-station', () => {
    for (const s of laid().filter((x) => x.kind === 'section')) {
      const a = station(s.from!);
      const b = station(s.to!);
      const start = pointAt(s.walked, 0);
      const end = pointAt(s.walked, s.walked.length);
      expect([s.key, Math.round(start.x), Math.round(start.y)]).toEqual([s.key, a.x, a.y]);
      expect([s.key, Math.round(end.x), Math.round(end.y)]).toEqual([s.key, b.x, b.y]);
    }
  });

  it('draws at fixed angles only — every leg horizontal, vertical or 45°', () => {
    for (const s of laid()) {
      const pts = s.walked.points;
      for (let i = 1; i < pts.length; i++) {
        const dx = Math.abs(pts[i]!.x - pts[i - 1]!.x);
        const dy = Math.abs(pts[i]!.y - pts[i - 1]!.y);
        expect([s.key, dx === 0 || dy === 0 || Math.abs(dx - dy) < 0.01]).toEqual([s.key, true]);
      }
    }
  });

  it('runs straight to the next station on the line', () => {
    expect(find('receiving→marshalling').walked.points).toHaveLength(2);
  });

  it('lifts a forward run over the station it passes — the train departs from the gates OVER the dock to the track', () => {
    const pts = find('gates→track').walked.points;
    const dock = station('dock');
    expect(Math.min(...pts.map((p) => p.y))).toBe(dock.y - ARC_ABOVE);
    // It never touches the dock's ring.
    const nearest = Math.min(...pts.map((p) => Math.hypot(p.x - dock.x, p.y - dock.y)));
    expect(nearest).toBeGreaterThan(20);
  });

  it('drops a backward run UNDER the line — the train made up at the dock runs back to the gates', () => {
    const pts = find('dock→gates').walked.points;
    expect(Math.max(...pts.map((p) => p.y))).toBe(station('dock').y + ARC_BELOW);
    const back = find('shop-floor→marshalling').walked.points;
    expect(Math.max(...back.map((p) => p.y))).toBeGreaterThan(station('shop-floor').y);
  });

  it('keeps the two directions of a two-way run apart — the garage and the dock, both ways', () => {
    const out = find('dock→garage').walked;
    const back = find('garage→dock').walked;
    const mid = (w: typeof out) => pointAt(w, w.length / 2);
    expect(Math.hypot(mid(out).x - mid(back).x, mid(out).y - mid(back).y)).toBeGreaterThan(8);
  });

  it('moves a straight two-way run a lane apart', () => {
    const a = { name: 'a', x: 100, y: 100, below: false, line: 'delivery' as const };
    const b = { name: 'b', x: 100, y: 300, below: true, line: 'siding' as const };
    const there = octilinear(a, b, [a, b], true);
    const back = octilinear(b, a, [a, b], true);
    expect(there[0]!.x).not.toBe(back[1]!.x);
    expect(octilinear(a, b, [a, b], false)[0]).toEqual({ x: 100, y: 100 });
  });

  it('draws an exit away from the line, and an entry into its station', () => {
    const exit = find('dock→').walked.points;
    expect(exit[0]!.y).toBeGreaterThan(station('dock').y);
    expect(exit[exit.length - 1]!.y).toBeGreaterThan(exit[0]!.y);
    const entry = find('→receiving').walked.points;
    expect(entry[0]!.y).toBeLessThan(entry[entry.length - 1]!.y);
    expect(entry[entry.length - 1]!.y).toBeLessThan(station('receiving').y);
  });
});

// THE ONE LOOKUP THE MOVES TRAVEL BY (car M2 builds on it): the path of
// the move (from, to) is the path the layer draws, or null where no
// route is served — a dot never runs on a line the map does not draw.
describe('routePath — the path a move (from, to) travels', () => {
  const routes = served();

  it('answers the drawn section, exit or entry, the same path the layer draws', () => {
    const train = routePath(routes, 'gates', 'track')!;
    expect(train.kind).toBe('section');
    expect(train.d).toBe(find('gates→track').d);
    expect(pointAt(train.walked, 0)).toEqual({ x: station('gates').x, y: station('gates').y });
    expect(routePath(routes, 'dock', null)?.kind).toBe('exit');
    expect(routePath(routes, null, 'receiving')?.kind).toBe('entry');
  });

  it('answers null for a move on a route the server does not serve', () => {
    expect(routePath(routes, 'dock', 'track')).toBeNull();
    expect(routePath(routes, 'arrivals', null)).toBeNull();
    expect(routePath(served([]), 'gates', 'track')).toBeNull();
  });
});
