// Every section id `sectionForRoute` produces must resolve in
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
import * as sectionsModule from './sections';
import {
  DYNAMIC_APP_SECTIONS,
  HOME_CHROME_SECTIONS,
  ROUTE_KINDS,
  SECTIONS_PRODUCED,
  appForRoute,
  moduleForRoute,
  sectionForRoute,
} from './sections';
import { ROUTE_CATALOG, appForSection } from './nav-catalog';
import type { Route } from '../router';

const catalogKeys = new Set(Object.keys(ROUTE_CATALOG));
// Every section a route can light. sections.ts derives it, so this file
// does not know how many answers there are.
const sections = SECTIONS_PRODUCED;

describe('one reader answers which row a route lights', () => {
  test('the kind-keyed map is not a public answer', () => {
    // Backlog c6f91515 (2026-09-20): once a row depends on a route
    // PARAMETER, a kind-keyed map answers plausibly and wrongly — both
    // retired yards would light the Train Yard's row, and nothing
    // errors. So the map is private to sections.ts, and
    // `sectionForRoute` is the one place to look. Re-exporting it (or
    // the region map beside it) hands the next author the wrong half.
    const exported = Object.keys(sectionsModule);
    expect(exported).not.toContain('SECTION_FOR_ROUTE');
    expect(exported).not.toContain('SECTION_FOR_KIND');
    expect(exported).not.toContain('REGION_SECTIONS');
  });

  test('every section produced is one some route lights', () => {
    // SECTIONS_PRODUCED is derived, not listed: each entry must be
    // reachable through the reader, or it is a ghost the pins below
    // would count as covered.
    const lit = new Set(ROUTE_KINDS.map((kind) => sectionForRoute({ kind } as Route)));
    expect([...sections].filter((s) => !lit.has(s)).sort()).toEqual([]);
  });
});

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
    expect(appForRoute({ kind: 'systemYard', at: 'dock' })).toBe('it');
    expect(appForRoute({ kind: 'accounts' })).toBe('sales');
    expect(appForRoute({ kind: 'jobs' })).toBe('home');
    expect(appForRoute({ kind: 'me' })).toBe('home');
  });

  test('an unmatched path renders in the department it was under (design ee3a3a2f Q4)', () => {
    // The /it catch-all returned the yard, so the IT chrome came free;
    // the not-found that replaced it must keep the reader in IT.
    expect(appForRoute({ kind: 'notFound', path: '/it/no-such' })).toBe('it');
    expect(appForRoute({ kind: 'notFound', path: '/dashboard/it/no-such' })).toBe('it');
    expect(appForRoute({ kind: 'notFound', path: '/ux/no-such' })).toBe('home');
    expect(appForRoute({ kind: 'notFound', path: '/items' })).toBe('home');
  });

  test('every station on the map lights the one Department Map row, not Operate', () => {
    // Design e765b3fc, car N1 (2026-09-25): the Receiving Yard, the
    // Marshalling Yard and the Crew Board lost their sidebar rows to the
    // one "Department Map" row, so every selection on the map lights
    // that row. Car N3 retired the floor pages and the crew board, so a
    // selection is the only way in, and a retired path — not found —
    // still renders in the IT chrome under the same row.
    for (const at of ['gates', 'receiving', 'marshalling', 'dock', 'shop-floor']) {
      expect(sectionForRoute({ kind: 'systemYard', at }), at).toBe('system-yard');
      expect(appForRoute({ kind: 'systemYard', at }), at).toBe('it');
    }
    expect(sectionForRoute({ kind: 'notFound', path: '/it/yard/dock' })).toBe('system-yard');
    expect(sectionForRoute({ kind: 'notFound', path: '/it/crew' })).toBe('system-yard');
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

  test('My schedule answers to its own row, not the Release calendar', () => {
    // Backlog eff0c5e5 (page audit 0e4fef17, gap 1; David approved
    // 2026-09-24): /ux/calendar/me IS the `schedule` row's path, but
    // its kind lit the `calendar` row — so it highlighted Release
    // calendar under Production and was gated on the calendar module,
    // which the live instance has off, and every visit answered
    // ModuleDisabled for a page whose own row is always-on in Home.
    expect(ROUTE_CATALOG.schedule.path).toBe('/ux/calendar/me');
    expect(sectionForRoute({ kind: 'myCalendar' })).toBe('schedule');
    expect(appForRoute({ kind: 'myCalendar' })).toBe('home');
    expect(moduleForRoute({ kind: 'myCalendar' })).toBeNull();
  });

  test('the service schedule answers to the Service queue, not My schedule', () => {
    // Backlog 3b50fe11 (decided 2026-09-24): /ux/service/schedule is the
    // service department's week grid of every tech, not a personal
    // schedule, yet its kind lit the `schedule` row — so once eff0c5e5
    // pointed myCalendar there too, two different pages highlighted
    // Home > My schedule, and the tech grid was ungated. It lights the
    // service department's own row and takes that row's module gate.
    expect(sectionForRoute({ kind: 'schedule' })).toBe('service');
    expect(appForRoute({ kind: 'schedule' })).toBe(ROUTE_CATALOG.service.app!);
    expect(moduleForRoute({ kind: 'schedule' })).toEqual({ id: 'support', label: 'Service queue' });
    // One page per My schedule row: only /ux/calendar/me lights it.
    const lightingMySchedule = ROUTE_KINDS.filter((kind) => sectionForRoute({ kind } as Route) === 'schedule');
    expect(lightingMySchedule).toEqual(['myCalendar']);
  });

  test('a route requires exactly the module that hides its own nav row', () => {
    // The drift this refuses: the module that HIDES a sidebar row and
    // the module that gates the ROUTE behind it are one fact.
    for (const kind of ROUTE_KINDS) {
      const entry = (ROUTE_CATALOG as Record<string, { module?: string } | undefined>)[
        sectionForRoute({ kind } as Route)
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
