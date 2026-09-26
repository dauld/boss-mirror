// THE ROUTES READ — design e765b3fc, car R3. The page parses what
// `GET /api/yard/routes` serves and draws exactly that; these pin the
// parse (a wrong server is a failed read, never an empty map) and the
// words a route's title and panel say.
import { describe, expect, it } from 'bun:test';
import { ROUTES, routesPayload } from '../../../tests/fixtures/yard';
import { exitNames, movesOn, parseRoutes, sourceLines, type Route } from './routes';

describe('parseRoutes', () => {
  it('reads every route, its two ends — null for off the map — and its sources', () => {
    const r = parseRoutes(routesPayload());
    expect(r.routes).toHaveLength(ROUTES.length);
    expect(r.observed).toBe(true);
    expect(r.window_hours).toBe(24);
    const exit = r.routes.find((x) => x.from === 'dock' && x.to === null)!;
    expect(exit.sources.map((s) => (s.source === 'workflow' ? s.step : s.source))).toEqual(['settled', 'cancelled']);
    expect(r.routes.find((x) => x.from === null && x.to === 'receiving')).toBeDefined();
  });

  it('refuses a payload that is not the routes read — a failed read, never a map with no edges', () => {
    expect(() => parseRoutes([])).toThrow('yard routes: expected an object');
    expect(() => parseRoutes({ routes: 'x' })).toThrow('expected a routes list');
  });

  it('refuses a route served with no source — the server pins that none is, and the page will not draw one', () => {
    expect(() => parseRoutes({ routes: [{ from: 'a', to: 'b', declared: true, sources: [] }] })).toThrow('served with no source');
  });

  it('refuses a route with neither end on the map, and a source it cannot name', () => {
    expect(() => parseRoutes({ routes: [{ from: null, to: null, declared: true, sources: [{ source: 'observed', moves: 1 }] }] })).toThrow(
      'neither end',
    );
    expect(() => parseRoutes({ routes: [{ from: 'a', to: 'b', declared: true, sources: [{ source: 'rumour' }] }] })).toThrow('unknown kind');
  });
});

describe('a route in words', () => {
  const exit: Route = {
    from: 'marshalling',
    to: null,
    declared: true,
    sources: [
      { source: 'workflow', workflow: 'backlog-item', version: 4, step: 'duplicate', via: 'closed' },
      { source: 'workflow', workflow: 'user-feedback', version: 4, step: 'duplicate', via: 'closed' },
      { source: 'workflow', workflow: 'backlog-item', version: 4, step: 'stale', via: 'closed' },
      { source: 'hand-off', by: 'rule:refresh-publish-drift-daily', from: null, to: null, why: 'superseded' },
      { source: 'observed', moves: 12, last_at: '' },
      { source: 'observed', moves: 3, last_at: '' },
    ],
  };

  it('an exit names each terminal once, and the hand-off by its maker', () => {
    expect(exitNames(exit)).toEqual(['duplicate', 'stale', 'refresh-publish-drift-daily']);
  });

  it('counts the moves the record observed, and none where it counted none', () => {
    expect(movesOn(exit)).toBe(15);
    expect(movesOn({ ...exit, sources: exit.sources.slice(0, 1) })).toBeNull();
  });

  it('says one line per source', () => {
    expect(sourceLines(exit, 24)).toHaveLength(exit.sources.length);
    expect(sourceLines(exit, 24)[0]).toBe('backlog-item v4, step duplicate (closed)');
    expect(sourceLines(exit, 24)[3]).toBe('rule:refresh-publish-drift-daily: superseded');
  });
});
