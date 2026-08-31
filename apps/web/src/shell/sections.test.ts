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
import { HOME_CHROME_SECTIONS, SECTION_FOR_ROUTE } from './sections';
import { ROUTE_CATALOG, appForSection } from './nav-catalog';

const catalogKeys = new Set(Object.keys(ROUTE_CATALOG));
const sections = new Set(Object.values(SECTION_FOR_ROUTE));

describe('sections resolve in the nav catalog', () => {
  test('every section id is a catalog key or a documented home-chrome exception', () => {
    const unresolved = [...sections]
      .filter((s) => !catalogKeys.has(s) && !HOME_CHROME_SECTIONS.has(s))
      .sort();
    expect(
      unresolved,
      'these section ids miss ROUTE_CATALOG, so their pages render under the ' +
        'Home chrome with no sidebar highlight — use the catalog key, or add a ' +
        'HOME_CHROME_SECTIONS entry with the reason the surface has no app',
    ).toEqual([]);
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
