// THE DEPARTMENT MAP'S SELECTION — design e765b3fc, car N1.
//
// The IT landing is the map on top and the selection's detail below it
// (David, added_2026_09_25_david_3 on feedback 84cba7e2: "the operating
// map sits at the top always and clicking stations or lines pulls up
// detail below"). The selection rides in the query, `/it?at=<name>`, and
// this reads it against the regions the SERVER served — so the panel
// names a station only when the read carried one, and a name it did not
// carry is said rather than drawn as an empty, quiet-looking panel.
//
// Stations only on this car; a section (`dock→track`) is car N2's, and
// until then it reads as a name the map does not carry.
import { regionTitle } from './region-page';
import type { Region, Regions } from './regions';

export type MapSelection =
  | Readonly<{ kind: 'none' }>
  | Readonly<{ kind: 'station'; name: string; title: string; region: Region }>
  | Readonly<{ kind: 'unknown'; at: string }>;

export function selectionOf(at: string | undefined, regions: Regions): MapSelection {
  if (at === undefined || at === '') return { kind: 'none' };
  const region = regions.regions.find((r) => r.name === at);
  return region === undefined
    ? { kind: 'unknown', at }
    : { kind: 'station', name: at, title: regionTitle(at), region };
}
