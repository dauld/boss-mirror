// Lint test: every sidebar path defined in AppShell.svelte must be
// matched by a non-catch-all branch in router.ts.
//
// Why this exists: the router has a catch-all
// `return { kind: 'home' }` at the end of parseRoute(). When a
// sidebar path doesn't match any earlier branch, clicking it
// silently renders the home page (which itself falls through to
// MePage in App.svelte) — no console error, no 404, just the
// wrong page. The /schedule bug on 2026-05-22 was exactly this:
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

import { describe, it, expect, beforeAll, afterAll } from 'bun:test';
import { parseRoute } from '../router';
import { ROUTE_CATALOG } from './nav-catalog';

// parseRoute touches `window.location.search` inside its `/jobs`
// branch. Stub a minimal Location for tests so we don't need
// happy-dom for one property access.
const originalWindow = (globalThis as { window?: unknown }).window;
beforeAll(() => {
  (globalThis as { window?: { location: { search: string; pathname: string } } }).window = {
    location: { search: '', pathname: '/' },
  };
});
afterAll(() => {
  (globalThis as { window?: unknown }).window = originalWindow;
});

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
      // The catch-all returns { kind: 'home' }. If a sidebar path
      // intentionally lands on home, that's a configuration smell
      // — the home view has its own entry point ("/"), and any
      // *labeled* sidebar item should resolve to its own route.
      expect(
        route.kind,
        `sidebar path "${path}" fell through to the catch-all '{kind: "home"}' — ` +
          `router.ts has no branch matching it. Either add a branch to parseRoute() ` +
          `or repoint the sidebar entry in AppShell.svelte's ALL_NAV.`,
      ).not.toBe('home');
    });
  }

  it('the catch-all itself still works (regression: parseRoute returns home for unknown paths)', () => {
    expect(parseRoute('/definitely-not-a-real-route-xyzzy').kind).toBe('home');
  });
});
