// Every section id SECTION_FOR_ROUTE produces must resolve in
// ROUTE_CATALOG — that key is what highlights the sidebar row and what
// `appForSection` uses to pick the app tab. A value that misses the
// catalog silently renders the page under Home chrome: right content,
// wrong app, no error.
//
// The route→section half of this fact is enforced at typecheck time
// (`Record<Route['kind'], string>` refuses a missing kind). This file
// pins the other half, which was unpinned and drifted: the IT tab's
// own landing page (/it/operate/audit) rendered under Home chrome
// because the section id was the camelCase route kind rather than the
// kebab-case catalog key. Reported as "I just clicked on the IT app
// and it is showing me still in home" (CLAUDE.md §9a).

import { describe, expect, test } from 'bun:test';
import {
  DYNAMIC_APP_SECTIONS,
  HOME_CHROME_SECTIONS,
  REGION_SECTIONS,
  SECTION_FOR_ROUTE,
  appForRoute,
  moduleForRoute,
  sectionForRoute,
} from './sections';
import { ROUTE_CATALOG, appForSection } from './nav-catalog';
import type { Route } from '../router';

const catalogKeys = new Set(Object.keys(ROUTE_CATALOG));
// Every section a route can light: the kind's own, plus the two a
// yard floor's REGION answers for (car 4 of design d2154293).
const sections = new Set([...Object.values(SECTION_FOR_ROUTE), ...Object.values(REGION_SECTIONS)]);

describe('sections resolve in the nav catalog', () => {
  test('every section id is a catalog key or a documented exception', () => {
    const unresolved = [...sections]
      .filter(
        (s) => !catalogKeys.has(s) && !HOME_CHROME_SECTIONS.has(s) && !DYNAMIC_APP_SECTIONS.has(s),
      )
      .sort();
    expect(
      unresolved,
      'these section ids miss ROUTE_CATALOG, so their pages render under the ' +
        'Home chrome with no sidebar highlight — use the catalog key, or add a ' +
        'HOME_CHROME_SECTIONS entry with the reason the surface has no app',
    ).toEqual([]);
  });

  test('a dynamic-app section is not also a catalog key, and is still produced', () => {
    // Two answers to "which app" — the route's and the catalog's —
    // is the drift this file exists to refuse. A dynamic section has
    // exactly one: the route's.
    const both = [...DYNAMIC_APP_SECTIONS.keys()].filter((s) => catalogKeys.has(s));
    expect(both).toEqual([]);
    const ghosts = [...DYNAMIC_APP_SECTIONS.keys()].filter((s) => !sections.has(s));
    expect(ghosts).toEqual([]);
  });

  test('a department route renders under its own department, never Home', () => {
    // The packet (cc76f755): a department with no surface got a tab
    // that landed on All jobs with Home highlighted. The tab must
    // highlight itself.
    expect(appForRoute({ kind: 'department', code: 'sales' })).toBe('sales');
    expect(appForRoute({ kind: 'department', code: 'operations' })).toBe('operations');
    // Every other route still answers through the catalog.
    expect(appForRoute({ kind: 'systemYard' })).toBe('it');
    expect(appForRoute({ kind: 'systemYardFloor', region: 'dock' })).toBe('it');
    expect(appForRoute({ kind: 'accounts' })).toBe('sales');
    expect(appForRoute({ kind: 'jobs' })).toBe('home');
    expect(appForRoute({ kind: 'me' })).toBe('home');
  });

  test('a yard with its own sidebar row highlights that row, not Operate', () => {
    // Feedback 92921c2f / design 55417146 (2026-09-18): the Receiving
    // Yard and the Marshalling Yard are sidebar rows now. The row that
    // highlights is `activeSection === item.id`, so their route kinds
    // must resolve to their own catalog keys — left on
    // 'system-incidents' they would light the Operate row while the
    // operator stands in a yard that has a row of its own.
    // Car 4 of design d2154293 retired the two PAGES: each yard is a
    // region of the world now, so the route is a yard floor and the
    // REGION decides the row — `sectionForRoute`, not the kind alone.
    // Read off the kind, both would light the Train Yard's row.
    expect(sectionForRoute({ kind: 'systemYardFloor', region: 'receiving' })).toBe('system-receiving');
    expect(sectionForRoute({ kind: 'systemYardFloor', region: 'marshalling' })).toBe('system-marshalling');
    expect(sectionForRoute({ kind: 'systemYardFloor', region: 'dock' })).toBe('system-yard');
    expect(appForRoute({ kind: 'systemYardFloor', region: 'receiving' })).toBe('it');
    expect(appForRoute({ kind: 'systemYardFloor', region: 'marshalling' })).toBe('it');
  });

  test('no exception names a section no longer produced', () => {
    // An exemption for a dead id reads as "handled" while covering
    // nothing, and quietly widens the hole when an id is renamed onto it.
    const ghosts = [...HOME_CHROME_SECTIONS.keys()].filter((s) => !sections.has(s)).sort();
    expect(ghosts).toEqual([]);
  });

  test('an exception really does land in the Home app', () => {
    // The exceptions are documented as "renders under Home chrome". If
    // one ever gains a catalog entry, it stops being an exception and
    // its row above should go — this catches the half-move.
    for (const s of HOME_CHROME_SECTIONS.keys()) {
      expect(appForSection(s), `${s} has a catalog entry now — drop its exception`).toBe('home');
    }
  });
});

describe('the module gate a route answers to', () => {
  // Backlog f9b43965: App.svelte carried a hand-written
  // `routeRequiredModule` switch beside the catalog's own `module`
  // field — one surface, two module ids. It named /ux/support's module
  // 'shipping', so a direct visit to the support page answered
  // "Shipments is not enabled" for a page with nothing to do with
  // shipments, while the nav row was hidden on 'support'. Same
  // CLAUDE.md §9a class as MODEL_ROUTES / MODEL_KINDS, and the same
  // fix: one answer derived from the catalog, not a second list.

  test('the support page is gated on support, not on shipping', () => {
    expect(moduleForRoute({ kind: 'support' })).toEqual({ id: 'support', label: 'Support' });
    expect(moduleForRoute({ kind: 'service' })).toEqual({ id: 'support', label: 'Service queue' });
  });

  test('a route requires exactly the module that hides its own nav row', () => {
    // The drift this refuses: the module that HIDES a sidebar row and
    // the module that gates the ROUTE behind it are one fact.
    for (const kind of Object.keys(SECTION_FOR_ROUTE) as ReadonlyArray<Route['kind']>) {
      const entry = (ROUTE_CATALOG as Record<string, { module?: string } | undefined>)[
        SECTION_FOR_ROUTE[kind]
      ];
      expect(moduleForRoute({ kind } as Route)?.id ?? null, kind).toEqual(entry?.module ?? null);
    }
  });

  test('a surface with no module in the catalog is never gated', () => {
    // Entries without a `module` field are always-on — the catalog
    // says so, and this is the only place that decides it.
    expect(moduleForRoute({ kind: 'jobs' })).toBeNull();
    expect(moduleForRoute({ kind: 'people' })).toBeNull();
    expect(moduleForRoute({ kind: 'me' })).toBeNull();
    // A department jobs view is one surface for every declared
    // department: no catalog row, so nothing to gate it on.
    expect(moduleForRoute({ kind: 'department', code: 'sales' })).toBeNull();
  });
});
