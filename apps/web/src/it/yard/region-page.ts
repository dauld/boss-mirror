// A REGION OWNS ITS PAGE (design 62de32ae, decision 7; approved by
// David 2026-09-24, folded into fe77a1d2's car 3, which had moved the
// Train Yard's deck under the region map WHOLE).
//
// The review of 2026-09-24 (finding 8) measured the half-swap: at
// /it/yard/<region> the map was the region's, and everything around it
// was still the world's. The heading read "The IT world"; the world's
// summary line ("1516 crossings · 666 waiting at the borders") sat
// between the region map and its floor; every floor region's departure
// board listed every car in every place, so the garage's board showed
// dock cars and trains at CI; and "No alerts — every machine is working
// or idle by design" stood directly under a TROUBLED shed.
//
// This module is the selections and the words a region's page is made
// of. It derives no count and no state: every number is the borders
// read's, the floor's or the regions read's, and what a region's panel
// leaves to the other regions is COUNTED, never dropped — a board that
// quietly filters reads as a floor with nothing else on it (the
// a-limit-is-not-a-filter class).

import { machineText, waitingText, type Border, type Borders } from './borders';
import { MACHINE_REGION } from './floor-slices';
import { regionOfStation, type FloorRegion } from './region-contents';
import type { Region, RegionState } from './regions';
import type { Platform } from './world-interior';
import type { Alert } from './yard-alerts';
import type { BoardRow, Station } from './yard-floor';

/** The region as a heading names it: `shop-floor` is "Shop floor". */
export function regionTitle(name: string): string {
  const words = name.replace(/-/g, ' ');
  return words.charAt(0).toUpperCase() + words.slice(1);
}

// ---------------------------------------------------------------------
// The region's in and out rails — the world's summary line, scoped.
// ---------------------------------------------------------------------

export type RegionRail = Readonly<{ side: 'in' | 'out'; other: string; border: Border }>;

/** The borders that enter and leave `region`, inbound first, each in
 *  the order the server listed it. */
export function regionRails(borders: Borders, region: string): ReadonlyArray<RegionRail> {
  const into = borders.borders
    .filter((b) => b.to === region)
    .map((b): RegionRail => ({ side: 'in', other: b.from, border: b }));
  const out = borders.borders
    .filter((b) => b.from === region)
    .map((b): RegionRail => ({ side: 'out', other: b.to, border: b }));
  return [...into, ...out];
}

const perDay = (b: Border): string =>
  b.rate.current === null ? 'rate: no reading' : `${Math.round(b.rate.current * 10) / 10}/day`;

/** One line per rail: "in from gates · 175/day · 6 waiting ·
 *  auto-park-on-gate-green · fired 7m ago", and a rail that is not
 *  clear says its state and the server's why. An unmeasured rate or
 *  queue reads "no reading" (borders.ts), never zero. */
export function railsText(rails: ReadonlyArray<RegionRail>): ReadonlyArray<string> {
  return rails.map((r) => {
    const b = r.border;
    const head = r.side === 'in' ? `in from ${r.other}` : `out to ${r.other}`;
    const judged = b.state === 'clear' ? '' : ` · ${b.state} — ${b.why}`;
    return `${head} · ${perDay(b)} · ${waitingText(b)} · ${machineText(b.machine)}${judged}`;
  });
}

/** The rails as the page prints them — or the sentence that says there
 *  are none, rather than a blank where a line was. */
export function railLines(borders: Borders, region: string): ReadonlyArray<string> {
  const lines = railsText(regionRails(borders, region));
  return lines.length > 0 ? lines : [`no rail enters or leaves the ${region} region on the map`];
}

// ---------------------------------------------------------------------
// The departure board and the alerts strip, scoped to the region.
// ---------------------------------------------------------------------

/** What the region's panels need of the floor: where each wagon stands,
 *  and the board's rows in the board's order. `Scene` satisfies it. */
export type FloorView = Readonly<{
  wagons: ReadonlyArray<Readonly<{ id: string; station: Station }>>;
  boardRows: ReadonlyArray<BoardRow>;
}>;

/** A region's share of the floor, and how much stands elsewhere. */
export type Scoped<T> = Readonly<{ here: ReadonlyArray<T>; elsewhere: number }>;

const regionOfWagon = (floor: FloorView, id: string): FloorRegion | null => {
  const w = floor.wagons.find((x) => x.id === id);
  return w === undefined ? null : regionOfStation(w.station);
};

/** The departure board filtered to the region's stations, in the
 *  board's own order. A row whose wagon cannot be placed is KEPT: it is
 *  the floor's to explain, and a row dropped for want of a place is a
 *  car drawn nowhere. */
export function boardFor(
  floor: FloorView,
  region: string,
): Readonly<{ rows: ReadonlyArray<BoardRow>; elsewhere: number }> {
  const rows = floor.boardRows.filter((r) => {
    const at = regionOfWagon(floor, r.id);
    return at === null || at === region;
  });
  return { rows, elsewhere: floor.boardRows.length - rows.length };
}

/** The areas an alert may name as its subject, each in the region that
 *  draws it — read off the definitions that place them on the floor
 *  (region-contents.ts for a station, floor-slices.ts for a machine),
 *  not a third copy of either. */
const AREA_REGION: Readonly<Record<string, FloorRegion>> = {
  track: regionOfStation('train'),
  dock: regionOfStation('dock'),
  garage: regionOfStation('garage'),
  approach: regionOfStation('approach'),
  'gate-queue': regionOfStation('gate-queue'),
  arrivals: regionOfStation('arrivals'),
  cancelled: regionOfStation('cancelled'),
  'inspection-shed': regionOfStation('inspection-shed'),
  ...MACHINE_REGION,
};

/** The region an alert's subject stands in; null when the subject is
 *  not one this page can place. */
function regionOfAlert(a: Alert, floor: FloorView): FloorRegion | null {
  if (a.subject.startsWith('car:')) return regionOfWagon(floor, a.subject.slice(4));
  if (a.subject.startsWith('train:')) return regionOfStation('train');
  if (a.subject.startsWith('bay:')) return regionOfStation('gate');
  return AREA_REGION[a.subject] ?? null;
}

/** The alerts strip scoped to the region. An alert this page cannot
 *  place is shown on EVERY region's strip: an alarm that stands in no
 *  region would be read nowhere. */
export function alertsFor(alerts: ReadonlyArray<Alert>, region: string, floor: FloorView): Scoped<Alert> {
  const here = alerts.filter((a) => {
    const at = regionOfAlert(a, floor);
    return at === null || at === region;
  });
  return { here, elsewhere: alerts.length - here.length };
}

/** The strip's line when nothing here alerts. It must not contradict
 *  the region's head above it: under a troubled shed "every machine is
 *  working or idle by design" read as a clear shed (review finding 8).
 *  An alert is a MACHINE's or a packet's trouble; the region's state is
 *  the regions read's own judgement, and the line says which is which.
 *  `reading` is the read not yet landed, which claims nothing either
 *  way; `unread` is the read that failed. */
export function quietText(region: string, state: RegionState | 'reading' | 'unread', elsewhere: number): string {
  const said =
    state === 'reading'
      ? `No alerts in the ${region}.`
      : state === 'unread'
      ? `No alerts in the ${region} — the region's own state could not be read.`
      : state === 'clear'
        ? `No alerts in the ${region} — every machine here is working or idle by design.`
        : `No machine alerts in the ${region} — its ${state} state above is the region's own reading, not a machine's.`;
  if (elsewhere === 0) return said;
  return `${said} ${elsewhere} ${elsewhere === 1 ? 'alert' : 'alerts'} elsewhere on the floor.`;
}

// ---------------------------------------------------------------------
// The interior against its head (decision 5).
// ---------------------------------------------------------------------

/** Why a region's platforms may stand more than its head counts, where
 *  the region has a reason of its own. Marshalling's platforms are its
 *  STATIONS, each standing marshalling's own members there — the
 *  server's `places`, the partition per station (car E; until then each
 *  drew its full depth, 562 under a head of 236) — and a packet whose
 *  step two stations' predicates match stands at both, while the head
 *  counts it once. So what the platforms stand past the head is exactly
 *  those extra standings. */
const WHY_THE_PLATFORMS_DIFFER: Readonly<Record<string, (extra: number, r: Region) => string>> = {
  marshalling: (extra, r) =>
    // A server that sends no places leaves each station at its depth.
    r.places.length === 0
      ? 'a packet stands at every station it matches, including one still in receiving or held by another region, ' +
        'and the head counts each once, only while it is marshalling’s'
      : extra > 0
        ? `the ${extra} more are packets standing at more than one station, drawn at each and counted once`
        : 'the stations stand fewer than the head counts — the station reads and the regions read were taken apart',
};

/** The sentence an interior owes when it does not draw the head's count
 *  (decision 5: "each interior draws the header's count, or names what
 *  it leaves out"), in the interior's own words — "the platforms
 *  stand", "the floor draws". Null when the two agree, and when either
 *  side is a number nobody could take: the head already says "no
 *  reading", and an unknown is drawn as `?`. */
export function countNote(r: Region | undefined, drawn: number | null, drawing: string): string | null {
  if (r === undefined || r.count === null || drawn === null || drawn === r.count) return null;
  const unit = r.unit === '' ? '' : ` ${r.unit}`;
  const why = WHY_THE_PLATFORMS_DIFFER[r.name]?.(drawn - r.count, r);
  return `${drawing} ${drawn} — the head counts ${r.count}${unit}${why ? `: ${why}` : ''}`;
}

/** The platforms against the head: `countNote` over what they stand,
 *  or nothing to compare when any platform's count is unknown. */
export function drawnNote(r: Region | undefined, platforms: ReadonlyArray<Platform>): string | null {
  if (platforms.some((p) => p.standing === null)) return null;
  return countNote(
    r,
    platforms.reduce((n, p) => n + (p.standing ?? 0), 0),
    'the platforms stand',
  );
}

/** A FLOOR SLICE AGAINST THE PLACES THE SERVER COUNTED (car E). The
 *  shed's slice is drawn from the floor's own reads; its head, and its
 *  three places, are the server's. The review drew five wagons under
 *  SHED 11 with nothing to say where the six were; this names each
 *  place the slice falls short in (or over), by the two counts. Null
 *  when every place is drawn at its count, and for a region with no
 *  places or no count. */
export function placesNote(
  r: Region | undefined,
  wagons: ReadonlyArray<Readonly<{ station: string }>>,
): string | null {
  if (r === undefined || r.count === null || r.places.length === 0) return null;
  const off = r.places
    .map((p) => ({ ...p, drawn: wagons.filter((w) => w.station === p.name).length }))
    .filter((p) => p.drawn !== p.count);
  if (off.length === 0) return null;
  const drawn = r.places.reduce((n, p) => n + wagons.filter((w) => w.station === p.name).length, 0);
  const unit = r.unit === '' ? '' : ` ${r.unit}`;
  return `the slice draws ${drawn} of the head’s ${r.count}${unit} — ${off
    .map((p) => `${p.name} ${p.drawn} of ${p.count}`)
    .join(' · ')}`;
}
