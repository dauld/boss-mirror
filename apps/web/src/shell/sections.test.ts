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
  SECTION_FOR_ROUTE,
  appForRoute,
} from './sections';
import { ROUTE_CATALOG, appForSection } from './nav-catalog';

const catalogKeys = new Set(Object.keys(ROUTE_CATALOG));
const sections = new Set(Object.values(SECTION_FOR_ROUTE));

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
    expect(SECTION_FOR_ROUTE.systemReceivingYard).toBe('system-receiving');
    expect(SECTION_FOR_ROUTE.systemMarshallingYard).toBe('system-marshalling');
    expect(appForRoute({ kind: 'systemReceivingYard' })).toBe('it');
    expect(appForRoute({ kind: 'systemMarshallingYard' })).toBe('it');
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
