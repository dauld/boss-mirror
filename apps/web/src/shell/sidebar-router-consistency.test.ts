// Lint test: every sidebar path defined in AppShell.svelte must be
// matched by a non-catch-all branch in router.ts.
//
// Why this exists: the router has a catch-all at the end of
// parseRoute(). It returned `{ kind: 'home' }` until design ee3a3a2f,
// so a sidebar path that matched no earlier branch silently rendered
// the wrong page — no console error, no 404. It returns
// `{ kind: 'notFound' }` now and the page says so, but a labelled
// sidebar row that says "No page at this address" is still a broken
// row. The /schedule bug on 2026-05-22 was exactly this:
// sidebar `path: '/schedule'` but router only matched
// `/service/schedule`. The user clicked "My schedule" and landed
// on their profile page.
//
// Maintenance: when a sidebar entry is added or renamed in
// AppShell.svelte's `ALL_NAV` table, mirror the path here. The
// test fails on drift in either direction (sidebar adds a path
// router doesn't handle, or router removes a branch a sidebar
// path depended on).
//
// The path list is no longer hand-maintained: ROUTE_CATALOG moved out
// of AppShell.svelte into ./nav-catalog as a plain module, so this
// test imports the real registry. The previous version mirrored every
// path by hand (a Bun test can't reliably parse a TypeScript const out
// of a Svelte `<script>` block), which meant the drift-catching test
// could itself drift.

import { describe, it, expect } from 'bun:test';
import { parseRoute } from '../router';
import { ROUTE_CATALOG } from './nav-catalog';

// Every catalog path, straight from the registry the sidebar renders.
// Plus the plain sub-page links declared inline in nav groups (they
// have no catalog entry of their own) and the two always-available
// surfaces reachable outside the sidebar.
const INLINE_SUBPAGE_PATHS: ReadonlyArray<string> = [
  '/it/operate/audit', // "Audit Log" — plain sub-page link in the Run group
  '/it/operate/atlas', // "Atlas" — plain sub-page link in the Run group
  '/ux/manual',
  '/ux/me',
];

const SIDEBAR_PATHS: ReadonlyArray<string> = [
  ...new Set([
    ...Object.values(ROUTE_CATALOG).map((i) => i.path),
    ...INLINE_SUBPAGE_PATHS,
  ]),
];

describe('sidebar-router consistency', () => {
  for (const path of SIDEBAR_PATHS) {
    it(`sidebar path "${path}" resolves to a non-catch-all route`, () => {
      const route = parseRoute(path);
      // The catch-all returns { kind: 'notFound' }; any *labeled*
      // sidebar item should resolve to its own route.
      expect(
        route.kind,
        `sidebar path "${path}" fell through to the catch-all '{kind: "notFound"}' — ` +
          `router.ts has no branch matching it. Either add a branch to parseRoute() ` +
          `or repoint the sidebar entry in nav-catalog.ts.`,
      ).not.toBe('notFound');
    });
  }

  it('the catch-all itself still works: an unknown path is notFound, naming it', () => {
    expect(parseRoute('/definitely-not-a-real-route-xyzzy')).toEqual({
      kind: 'notFound',
      path: '/definitely-not-a-real-route-xyzzy',
    });
  });
});
