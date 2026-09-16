// The reading half of the surface-open record (backlog 628f182b):
// every number on the Surfaces section is a pure function of the
// roll-up rows, and the never-opened list is the catalog minus them.

import { describe, expect, test } from 'bun:test';
import { catalogPaths, neverOpened, parseRollup, perActor, windowFor, type RouteCount } from './surfaces';
import { ROUTE_CATALOG } from '../../shell/nav-catalog';

const row = (actor_id: string, route: string, opens: number, last_at = '2026-09-16T12:00:00Z'): RouteCount => ({
  actor_id,
  route,
  opens,
  last_at,
});

describe('parseRollup', () => {
  test('reads the envelope and drops a row that is not a count', () => {
    const r = parseRollup({
      since: '2026-09-09T00:00:00Z',
      until: '2026-09-16T00:00:00Z',
      rows: [
        { actor_id: 'emp-david', route: '/it', opens: 4, last_at: '2026-09-16T11:00:00Z' },
        { actor_id: 'emp-david', route: '/it/codebase', opens: 'four' },
        { route: '/it/kb', opens: 1 },
      ],
    });
    expect(r.since).toBe('2026-09-09T00:00:00Z');
    expect(r.rows).toEqual([row('emp-david', '/it', 4, '2026-09-16T11:00:00Z')]);
  });

  test("a mock's bare array is an empty roll-up with no window, not a crash", () => {
    expect(parseRollup([])).toEqual({ since: null, until: null, rows: [] });
  });
});

describe('windowFor', () => {
  test('the last N days ending now', () => {
    const w = windowFor(7, new Date('2026-09-16T12:00:00Z'));
    expect(w).toEqual({ since: '2026-09-09T12:00:00.000Z', until: '2026-09-16T12:00:00.000Z' });
  });
});

describe('perActor', () => {
  test('folds opens and distinct routes per actor, busiest first, routes most-opened first', () => {
    const usage = perActor([
      row('emp-032', '/ux/me', 2),
      row('emp-david', '/it/codebase', 3),
      row('emp-david', '/it', 5),
      row('emp-david', '/ux/jobs/:jobId', 3),
    ]);
    expect(usage.map((u) => u.actor_id)).toEqual(['emp-david', 'emp-032']);
    expect(usage[0]).toEqual({
      actor_id: 'emp-david',
      opens: 11,
      distinct: 3,
      routes: [
        { route: '/it', opens: 5, last_at: '2026-09-16T12:00:00Z' },
        { route: '/it/codebase', opens: 3, last_at: '2026-09-16T12:00:00Z' },
        { route: '/ux/jobs/:jobId', opens: 3, last_at: '2026-09-16T12:00:00Z' },
      ],
    });
    expect(usage[1]!.distinct).toBe(1);
  });

  test('no rows is no actors', () => {
    expect(perActor([])).toEqual([]);
  });
});

describe('neverOpened', () => {
  test('the roster minus what any actor opened, in roster order', () => {
    const roster = ['/it', '/it/codebase', '/it/kb', '/ux/me'];
    expect(neverOpened([row('emp-david', '/it', 1), row('emp-032', '/ux/me', 1)], roster)).toEqual([
      '/it/codebase',
      '/it/kb',
    ]);
  });

  test('with no rows every catalogued surface is a candidate — and the page says why', () => {
    expect(neverOpened([], catalogPaths())).toEqual(catalogPaths());
  });

  test('the roster is every catalog path once (CLAUDE.md 9a: one catalog)', () => {
    const paths = catalogPaths();
    expect(new Set(paths).size).toBe(paths.length);
    for (const e of Object.values(ROUTE_CATALOG)) expect(paths).toContain(e.path);
  });
});
