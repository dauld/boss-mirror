// THE SELECTION'S PANEL — design e765b3fc, car N2 (David, 2026-09-25, on
// feedback 84cba7e2: "I want to put more of that data behind a map
// selection for display at the bottom. So I can click a line segment and
// see info on how it is performing, but we don't need to keep it all on
// the main map").
//
// The map on top now carries a station's name, its one number and its
// state, and nothing else. Everything it used to write beside them — the
// headway on every section, the hold sentences, the machine's name and
// status, the KPI line, the verdict — is read here instead, for the one
// station or section selected, in the order the design names:
// rate, waiting, stuck, trend, the verdict and its why, the machines, and
// the recent crossings.
//
// NOTHING HERE COUNTS OR JUDGES. Every line is a reading the server
// already sent in `/api/yard/regions` or `/api/yard/borders`, turned into
// words by the functions that already word it (borders.ts, regions.ts,
// transit.ts) — so the panel and the map it replaced cannot disagree. A
// reading that was not taken is "no reading", never zero, and a station
// is read through its OWN sections (region-page.ts `regionRails`), in
// then out, never through another station's.
//
// RECENT CROSSINGS are the one field the record cannot fill yet: the
// last crossing of each section is served, but the list of the packets
// that crossed needs the moves record (design e765b3fc section 3, car
// M1), which does not exist. The panel says so rather than drawing an
// empty list that would read as nothing crossing.
import { crossedText, machineText, rateText, unlistedText, waitingText, type Border, type Borders, type HoldsByClass } from './borders';
import { regionRails, type RegionRail } from './region-page';
import { bandText, stateText, trendText, type Region, type RegionName } from './regions';
import { sourceLines, type Route } from './routes';
import { headwayText, stationLabel } from './transit';

export type PanelField = 'route' | 'crossing' | 'rate' | 'waiting' | 'stuck' | 'trend' | 'verdict' | 'machines' | 'crossings';

export type PanelCell = Readonly<{ field: PanelField; label: string; lines: ReadonlyArray<string> }>;

export const FIELD_LABEL: Readonly<Record<PanelField, string>> = {
  route: 'What declares it',
  crossing: 'One crossing',
  rate: 'Rate',
  waiting: 'Waiting',
  stuck: 'Stuck',
  trend: 'Trend',
  verdict: 'Why',
  machines: 'Machines',
  crossings: 'Recent crossings',
};

/** Said in the crossings field until the moves record exists. */
export const MOVES_NOT_KEPT =
  'each packet that crossed is listed here once the yard keeps a record of its moves — it does not yet';

const NO_RAILS = 'no reading — the sections could not be read';

/** A served route the borders read keeps no rate for yet — the train's
 *  own line, the garage's returns (design e765b3fc §2c: "until then, a
 *  new route answers no reading, never 0"). Its rate comes from the
 *  moves record, car M3. */
export const NO_RATE_YET = 'no reading yet — this route is served from the protocols, and its rate comes with the moves record';

const cellOf = (field: PanelField, lines: ReadonlyArray<string>): PanelCell => ({ field, label: FIELD_LABEL[field], lines });

/** On whom a queue waits, by the server's own classes (`holds_by_class`)
 *  — each class that holds any, and nothing for an empty queue. */
export function classesText(h: HoldsByClass): string {
  return [
    h.machine > 0 ? `${h.machine} on a machine` : '',
    h.person > 0 ? `${h.person} on a person or the world` : '',
    h.unknown > 0 ? `${h.unknown} cannot tell` : '',
    h.stuck > 0 ? `${h.stuck} stuck` : '',
  ]
    .filter((s) => s !== '')
    .join(' · ');
}

/** The queue on one section, with whom it waits on. */
function queueText(b: Border): string {
  const classes = b.holds_by_class === null ? '' : classesText(b.holds_by_class);
  return classes === '' ? waitingText(b) : `${waitingText(b)} — ${classes}`;
}

/** The holds the server listed, and what it counted but did not list. */
function holdLines(b: Border): ReadonlyArray<string> {
  const unlisted = unlistedText(b);
  return [...b.holds.map((h) => `${h.what} — ${h.why}`), ...(unlisted === '' ? [] : [unlisted])];
}

const stuckText = (b: Border): string | null =>
  b.holds_by_class === null ? 'no reading' : b.holds_by_class.stuck > 0 ? `${b.holds_by_class.stuck} stuck` : null;

const railHead = (r: RegionRail): string =>
  r.side === 'in' ? `in from ${stationLabel(r.other)}` : `out to ${stationLabel(r.other)}`;

/** A station's panel, read through the sections that enter and leave it. */
export function stationCells(region: Region, borders: Borders | null, now: string): ReadonlyArray<PanelCell> {
  const rails = borders === null ? null : regionRails(borders, region.name);
  const perRail = (lines: (r: RegionRail) => ReadonlyArray<string>, none: string): ReadonlyArray<string> => {
    if (rails === null) return [NO_RAILS];
    const out = rails.flatMap(lines);
    return out.length > 0 ? out : [none];
  };
  const noRail = `no section enters or leaves ${stationLabel(region.name)} on the map`;
  const band = bandText(region);
  return [
    cellOf('rate', perRail((r) => [`${railHead(r)}: ${headwayText(r.border)}`], noRail)),
    cellOf('waiting', perRail((r) => [`${railHead(r)}: ${queueText(r.border)}`, ...holdLines(r.border)], noRail)),
    cellOf(
      'stuck',
      perRail((r) => {
        const s = stuckText(r.border);
        return s === null ? [] : [`${railHead(r)}: ${s}`];
      }, 'nothing stuck on its sections'),
    ),
    cellOf('trend', [`${region.trend.metric}: ${trendText(region.trend)}`, ...region.kpi.map((m) => m.text)]),
    cellOf('verdict', [stateText(region), ...(band === null ? [] : [band]), region.why]),
    cellOf(
      'machines',
      region.machines.length === 0
        ? ['no machine of ours works here']
        : region.machines.map((m) => `${m.name} · ${m.state} — ${m.why}`),
    ),
    cellOf(
      'crossings',
      rails === null ? [NO_RAILS] : [...rails.map((r) => `${railHead(r)}: ${crossedText(r.border, now)}`), MOVES_NOT_KEPT],
    ),
  ];
}

const SECTION_FIELDS: ReadonlyArray<PanelField> = ['crossing', 'rate', 'waiting', 'stuck', 'trend', 'verdict', 'machines', 'crossings'];

/** A section's panel — one border, every field its own. `null` is a
 *  section the page selected while the borders read had not answered. */
export function sectionCells(
  b: Border | null,
  now: string,
  served: Readonly<{ route: Route; windowHours: number; bordersRead: boolean }> | null = null,
): ReadonlyArray<PanelCell> {
  // WHAT DECLARES IT (car R3): the section is a route the server serves,
  // and its sources say why it is on the map — the protocol step that
  // makes the move, the hand-off, the moves the record counted.
  const declared = served === null ? [] : [cellOf('route', sourceLines(served.route, served.windowHours))];
  // No border: the borders read failed, or it answered and keeps no rate
  // for this route yet — two different facts, each said as itself.
  const none = served !== null && served.bordersRead ? NO_RATE_YET : NO_RAILS;
  if (b === null) return [...declared, ...SECTION_FIELDS.map((f) => cellOf(f, [none]))];
  return [
    ...declared,
    cellOf('crossing', [b.crossing]),
    cellOf('rate', [headwayText(b)]),
    cellOf('waiting', [queueText(b), ...holdLines(b)]),
    cellOf('stuck', [stuckText(b) ?? 'nothing stuck']),
    cellOf('trend', [rateText(b.rate)]),
    cellOf('verdict', [b.state, b.why, b.flowing_why]),
    cellOf('machines', [machineText(b.machine), ...(b.machine.why === '' ? [] : [b.machine.why])]),
    cellOf('crossings', [crossedText(b, now), MOVES_NOT_KEPT]),
  ];
}

/** A board a station's panel carries below its floor — each one a page
 *  of its own until car N3 of design e765b3fc (2026-09-25):
 *  `yard-status` was /it/operate/yard-status, `conductor`
 *  /it/operate/conductor, `feedback` /it/design/feedback and `backlog`
 *  /it/design/backlog. */
export type StationBoard = 'yard-status' | 'conductor' | 'feedback' | 'backlog';

/** WHERE EACH RETIRED PAGE'S CONTENT LIVES NOW — the design's page table
 *  (e765b3fc section 5), as data the page draws from. A train's block,
 *  phase and ETA and the recent trains are the track's detail, and the
 *  conductor is the track's machine; the dock's cars and its boarding
 *  sentence are the dock's; the stranded and held greens are the
 *  garage's (the garage region claims them). A feedback packet is
 *  inbound, so its board is receiving's; a backlog item is inbound until
 *  triaged and then stands at a station, so its board is receiving's AND
 *  marshalling's. The yard status board draws, for each station, only
 *  its own lanes (YardStatusPanel.svelte `part`). */
export const STATION_BOARDS: Readonly<Partial<Record<RegionName, ReadonlyArray<StationBoard>>>> = {
  track: ['yard-status', 'conductor'],
  dock: ['yard-status'],
  garage: ['yard-status'],
  receiving: ['feedback', 'backlog'],
  marshalling: ['backlog'],
};

/** The boards `station`'s panel carries, in the order it draws them —
 *  none for a station the table does not name, or a name that is not a
 *  station at all. */
export function boardsOf(station: string): ReadonlyArray<StationBoard> {
  return (STATION_BOARDS as Readonly<Record<string, ReadonlyArray<StationBoard> | undefined>>)[station] ?? [];
}
