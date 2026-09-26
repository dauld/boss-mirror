// THE DEPARTMENT MAP'S SELECTION — design e765b3fc, cars N1 and N2.
//
// The IT landing is the map on top and the selection's detail below it
// (David, added_2026_09_25_david_3 on feedback 84cba7e2: "the operating
// map sits at the top always and clicking stations or lines pulls up
// detail below"). The selection rides in the query, `/it?at=<name>`, and
// this reads it against the reads the SERVER served — so the panel names
// a station or a section only when the read carried one, and a name it
// did not carry is said rather than drawn as an empty, quiet-looking
// panel.
//
// A SECTION (car N2): "I can click a line segment and see info on how it
// is performing" (added_2026_09_25_david_2). `?at=dock->track` names the
// border from the dock to the track. The design wrote the arrow as `→`,
// and a link copied out of it opens the same section: both spellings
// read as one selection, keyed one way (`->`, which a URL carries without
// ambiguity). The map keeps its own `→` key (transit.ts `sectionKey`);
// `markOf` is the one crossing between the two.
//
// A SECTION IS A SERVED ROUTE (car R3): the map draws the routes
// `/api/yard/routes` serves, so a drawn section the borders read has no
// rate for yet — the train's gates → track, the garage's return to the
// dock — is still a section, and its panel says what declares it and
// that its rate is not read yet, rather than that nothing is named so.
import type { Border, Borders } from './borders';
import { regionTitle } from './region-page';
import { REGION_NAMES, type Region, type Regions } from './regions';
import type { Route, Routes } from './routes';
import { sectionKey } from './route-layout';

export type MapSelection =
  | Readonly<{ kind: 'none' }>
  | Readonly<{ kind: 'station'; name: string; title: string; region: Region }>
  /** `border` is null while the borders read has not answered, or
   *  failed, or where the route is served and has no rate yet: the
   *  panel says which. `route` is the served route, null while the
   *  routes read is out. */
  | Readonly<{ kind: 'section'; key: string; from: string; to: string; title: string; border: Border | null; route: Route | null }>
  | Readonly<{ kind: 'unknown'; at: string }>;

/** The two spellings of a section: `from->to`, and the design's arrow. */
const SECTION = /^([a-z-]+?)(?:->|→)([a-z-]+)$/;

export function selectionOf(
  at: string | undefined,
  regions: Regions,
  borders: Borders | null,
  routes: Routes | null = null,
): MapSelection {
  if (at === undefined || at === '') return { kind: 'none' };
  const m = at.match(SECTION);
  if (m !== null) {
    const [, from, to] = m as unknown as [string, string, string];
    const border = borders?.borders.find((b) => b.from === from && b.to === to) ?? null;
    const route = routes?.routes.find((r) => r.from === from && r.to === to) ?? null;
    // Named by neither read that answered: nothing on the map is this.
    // With both reads out, the page already says so, and the panel must
    // not add that the section does not exist.
    const read = borders !== null || routes !== null;
    if (read && border === null && route === null) return { kind: 'unknown', at };
    return { kind: 'section', key: `${from}->${to}`, from, to, title: `${regionTitle(from)} → ${regionTitle(to)}`, border, route };
  }
  const region = regions.regions.find((r) => r.name === at);
  return region === undefined
    ? { kind: 'unknown', at }
    : { kind: 'station', name: at, title: regionTitle(at), region };
}

/** What the map marks as selected, in the map's own keys: a station's
 *  name, or a section's `from→to`. Nothing for no selection, and nothing
 *  for a name the map does not carry — there is nothing on it to mark. */
export function markOf(s: MapSelection): string | null {
  if (s.kind === 'station') return s.name;
  if (s.kind === 'section') return sectionKey(s.from, s.to);
  return null;
}

/** A station named while the regions read is out (design e765b3fc, car
 *  N3): its panel still opens, on the name alone, because what stands in
 *  it — the floor deck, the queue boards and the boards of the pages
 *  that retired into it — reads its own endpoints, as the floor page it
 *  replaced did. Only a name the map lays out is a station; a section, a
 *  typo or no selection names nothing. */
export function unreadStationOf(at: string | undefined): Readonly<{ name: string; title: string }> | null {
  if (at === undefined || !(REGION_NAMES as ReadonlyArray<string>).includes(at)) return null;
  return { name: at, title: regionTitle(at) };
}
