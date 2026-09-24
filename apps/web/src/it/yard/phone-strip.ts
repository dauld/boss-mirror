// THE PHONE STRIP MAP (design 62de32ae decision 12, approved by David
// 2026-09-24; car G). On a phone the IT world is not the SVG shrunk
// until its type is unreadable: it is a vertical strip, one row per
// region in flow order, grouped under the three thirds, each row the
// region's state, its KPI and the rail coming INTO it — what waits there
// and how fast it crosses. The review of 2026-09-24 rendered /it at
// 390px and found no phone view at all: a 1240-unit world with a 900px
// floor inside a desktop shell, 1246px wide.
//
// NOTHING HERE DERIVES A NUMBER. The state, the KPI and the rail's
// waiting and rate are the server's (regions.ts, borders.ts), in their
// own words; this module only decides which row each goes on.
//
// THE THIRDS ARE THE SERVER'S WHEN IT SENDS THEM. Car F of the same
// design (design 00774ca8, the HUD frame) makes `boss_jobs::regions::
// THIRDS` a partition and ships it on the regions read as `thirds`,
// each with its regions in flow order. Until that payload arrives this
// strip stands in the same partition as the layout declares it
// (LAYOUT_THIRDS), and says which it used — so the day the server sends
// the block, the strip reads it without an edit here, and this copy is
// the one to delete.
//
// NOTHING IS DROPPED. A region no third names gets a row under its own
// heading, and a region a third names that the payload did not carry
// gets a row reading "no reading" — the map's rule that unknown is drawn
// as unknown, applied to membership.

import type { Border, Borders } from './borders';
import { waitingText } from './borders';
import type { Region, Regions } from './regions';

/** THE ONE PHONE BREAKPOINT is web-kit's (ui/phone.ts), because the
 *  chrome bar turns over at it too. The shell's collapse (styles.css)
 *  and the swap from world to strip (MapPage.svelte) are pinned to it by
 *  phone-strip.test.ts: a width between two numbers would get a strip
 *  inside a desktop shell. */
export { PHONE_MAX_WIDTH, PHONE_QUERY } from '@boss/web-kit/ui/phone';

export type Membership = Readonly<{ third: string; regions: ReadonlyArray<string> }>;

/** The partition car F declares (`THIRDS` in boss-jobs/src/regions.rs
 *  on that car), each third's regions in the order a car walks them.
 *  The stand-in until the server sends its own — see the header. */
export const LAYOUT_THIRDS: ReadonlyArray<Membership> = [
  { third: 'queue-management', regions: ['receiving', 'marshalling'] },
  { third: 'actors-building', regions: ['shop-floor', 'gates', 'garage'] },
  { third: 'delivery', regions: ['dock', 'track', 'arrivals', 'shed', 'publish'] },
];

/** The heading a third prints. A third this client has no words for
 *  prints its own name, de-hyphenated — never nothing. */
const THIRD_LABELS: Readonly<Record<string, string>> = {
  'queue-management': 'Queue management',
  'actors-building': 'Actors building',
  delivery: 'Delivery',
  unplaced: 'In no third',
};
export const thirdLabel = (third: string): string => THIRD_LABELS[third] ?? third.replace(/-/g, ' ');

const isMembership = (v: unknown): v is Membership => {
  if (typeof v !== 'object' || v === null) return false;
  const o = v as Readonly<Record<string, unknown>>;
  return typeof o.third === 'string' && Array.isArray(o.regions) && o.regions.every((r) => typeof r === 'string');
};

/** Which thirds to group under: the payload's `thirds` block when the
 *  server sent a well-formed one, else the layout's. Read structurally,
 *  so this compiles against the payload type with or without car F. */
export function thirdsOf(read: Regions): Readonly<{ source: 'server' | 'layout'; thirds: ReadonlyArray<Membership> }> {
  const payload: Readonly<Record<string, unknown>> = read;
  const served = payload.thirds;
  if (Array.isArray(served) && served.length > 0 && served.every(isMembership)) {
    return { source: 'server', thirds: served.map((t) => ({ third: t.third, regions: [...t.regions] })) };
  }
  return { source: 'layout', thirds: LAYOUT_THIRDS };
}

/** One row of the strip. `region` is undefined when the read did not
 *  carry it; `rails` is null when the borders were not read — both are
 *  drawn as "no reading", never as clear or empty. */
export type StripRow = Readonly<{
  name: string;
  region: Region | undefined;
  rails: ReadonlyArray<Border> | null;
}>;

export type StripGroup = Readonly<{ third: string; label: string; rows: ReadonlyArray<StripRow> }>;

/** The line's order — the layout's thirds flattened, then anything else
 *  the payload carries, in its own order. Orders the regions no third
 *  names. */
const FLOW: ReadonlyArray<string> = LAYOUT_THIRDS.flatMap((t) => t.regions);
const flowRank = (name: string, payload: ReadonlyArray<string>): number => {
  const at = FLOW.indexOf(name);
  return at >= 0 ? at : FLOW.length + payload.indexOf(name);
};

export function stripGroups(read: Regions, borders: Borders | null): ReadonlyArray<StripGroup> {
  const { thirds } = thirdsOf(read);
  const payload = read.regions.map((r) => r.name);
  const row = (name: string): StripRow => ({
    name,
    region: read.regions.find((r) => r.name === name),
    rails: borders === null ? null : borders.borders.filter((b) => b.to === name),
  });
  const placed = new Set(thirds.flatMap((t) => t.regions));
  const groups: ReadonlyArray<StripGroup> = thirds.map((t) => ({
    third: t.third,
    label: thirdLabel(t.third),
    rows: t.regions.map(row),
  }));
  const known = [...new Set([...FLOW, ...payload])];
  const unplaced = known
    .filter((n) => !placed.has(n))
    .sort((a, b) => flowRank(a, payload) - flowRank(b, payload));
  return unplaced.length === 0
    ? groups
    : [...groups, { third: 'unplaced', label: thirdLabel('unplaced'), rows: unplaced.map(row) }];
}

/** What a non-clear row says under its KPI — the same verdict the
 *  world's territory prints: the band that decided the state (decision
 *  1), and for trouble the why after it; an older payload with no band
 *  gives the why alone. A clear row says nothing more. A region the
 *  read did not carry says so. */
export function verdictOf(r: Region | undefined): ReadonlyArray<string> {
  if (r === undefined) return ['the regions read answered nothing for this region'];
  if (r.state === 'clear') return [];
  const band = r.band?.reads ?? '';
  if (band === '') return [r.why];
  return r.state === 'troubled' ? [band, r.why] : [band];
}

const num = (v: number): string => String(Math.round(v * 10) / 10);

/** A rail into a row's region, in one line: where it comes from, what
 *  waits on it, and this window's crossing rate — "no reading" for
 *  either half the server could not measure, never 0. */
export function railLine(b: Border): string {
  const rate = b.rate.current === null ? 'rate: no reading' : `${num(b.rate.current)} /day`;
  return `from ${b.from} · ${waitingText(b)} · ${rate}`;
}
