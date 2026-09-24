// RECEIVING AND MARSHALLING GET INTERIORS (design d2154293, car 4).
//
// Car 3 zoomed the camera into a territory and drew what is MOVING
// inside it — the wagons the floor's Scene stands at that region's
// stations. These two regions hold no rolling stock in transit: they
// hold QUEUES. So their interior is a PLATFORM PER QUEUE — a station
// for marshalling, a channel for receiving — with the packets standing
// on it as marks, the queue's bound drawn on the track, and the
// packets past that bound flagged. Until this car both said "this
// region's floor is a page of its own", which was an apology printed
// where the region's own activity belongs.
//
// ONE DEFINITION PER NUMBER. Nothing here reads an endpoint and
// nothing here derives a count: the marshalling platforms are built
// from the sidings `marshalling.ts` already joined (depth from
// `/api/stations/load`, rate from `/api/stations/flow`), the receiving
// platforms from the inbound rows `receiving.ts` already parsed and
// classified. The pages that own those reads hand them up, the way
// YardPage hands its Scene up for the other six territories — one read
// of a region on the page, never two.
//
// A NUMBER NOBODY COULD TAKE IS UNKNOWN, NEVER NOUGHT. A station whose
// flow the cube is blind to (the dock's Job-metadata clause, a
// per-actor watchlist) carries `rate: null`, and the map draws a `?`
// in its own band — on a queue board a 0 reads as "nothing is being
// worked", which is a different fact and usually a false one. The same
// holds for `standing`: null draws no marks at all, where a real zero
// draws an empty track.
//
// Everything here is pure arithmetic over the layout in world.ts, so a
// platform that lies fails world-interior.test.ts before it is a
// picture. WorldMap.svelte owns the strokes.

import { AGE_THRESHOLDS, CHANNELS, ageDays, type Channel, type InboundRow } from '../receiving/receiving';
import type { Crew } from '../crew/crew';
import type { Siding } from '../marshalling/marshalling';
import { contentsBox, overflowNoteAt, type NoteAt } from './region-contents';
import type { Territory } from './world';

/** Which standing packets are flagged, and from which end of the
 *  queue: the oldest stand at the HEAD (receiving's age bands), the
 *  ones past a WIP bound at the TAIL (marshalling's discipline). */
export type Flag = Readonly<{ from: 'head' | 'tail'; n: number }>;

/** One queue, drawn as a platform with its packets standing on it. */
export type Platform = Readonly<{
  /** WHICH queue — unique among a region's platforms, and what the map
   *  keys it on. Apart from `name` since backlog 846ab934: the shop
   *  floor named its crews by actor and keyed them on that name, and
   *  live every crew is the same actor, so the keyed each threw and
   *  the region never drew. A station and a channel are their own key;
   *  a crew is `session:<id>`, the spelling the server's machines use. */
  key: string;
  /** The queue's own name — a station, or an inbound channel. The
   *  label, which need not be unique. */
  name: string;
  /** Packets standing on it. `null` is a count nobody could take. */
  standing: number | null;
  /** The bound the queue is read against, where it declares one. */
  bound: number | null;
  /** What LEFT in the window. `null` where nobody counted it. */
  rate: number | null;
  flag: Flag;
  /** One short line: the oldest packet's age, what left in the window,
   *  and — where a number is unknown — the clause that blinded it. */
  note: string;
}>;

/** What a zoomed region hands the map. A read that failed is a failure
 *  here too: a region drawn empty would read as a calm one. */
export type Deck =
  | Readonly<{ kind: 'reading' }>
  | Readonly<{ kind: 'unavailable'; why: string }>
  | Readonly<{ kind: 'ready'; region: string; platforms: ReadonlyArray<Platform> }>;

/** The regions whose interior is platforms rather than wagons in
 *  transit. The other six are region-contents.ts's INTERIOR_REGIONS.
 *  The shop floor joined them on backlog 94c6ffd0: a crew is a
 *  platform and its runs are what stands on it — the same picture as a
 *  queue, because a crew IS one, bounded by how much it can build at
 *  once rather than by a WIP limit. */
export const PLATFORM_REGIONS: ReadonlyArray<string> = ['receiving', 'marshalling', 'shop-floor'];

export function hasPlatforms(region: string): boolean {
  return PLATFORM_REGIONS.includes(region);
}

// ---------------------------------------------------------------------
// What each region's platforms say.
// ---------------------------------------------------------------------

/** Marshalling: a platform per station, in the order `joinSidings`
 *  already put them — the queues under pressure first. */
export function marshallingPlatforms(
  sidings: ReadonlyArray<Siding>,
  windowHours: number,
): ReadonlyArray<Platform> {
  return sidings.map((s): Platform => {
    const over = s.wipLimit !== null && s.depth > s.wipLimit ? s.depth - s.wipLimit : 0;
    const oldest = s.oldestAgeDays === null ? null : `oldest ${s.oldestAgeDays} d`;
    const rate = s.flow.kind === 'counted' ? s.flow.served : null;
    const left =
      s.flow.kind === 'counted'
        ? `${s.flow.served} left in ${windowHours}h`
        : `rate not counted — ${s.flow.reason}`;
    return {
      key: s.station,
      name: s.station,
      standing: s.depth,
      bound: s.wipLimit,
      rate,
      flag: { from: 'tail', n: over },
      note: [oldest, left].filter((p) => p !== null).join(' · '),
    };
  });
}

/** THE SHOP FLOOR: a platform per CREW — one open session — with the
 *  runs it dispatched standing on it (design 511fa7d4 car 2b, backlog
 *  94c6ffd0). The busiest crew leads, then the idle ones, so a floor
 *  with someone working reads as working.
 *
 *  A crew whose silence could not be judged is `null`-idle upstream
 *  and says so here rather than claiming either; and `rate` is null on
 *  every platform, because what a crew FINISHED in the window is not
 *  in the reads this board makes — the runs it lists are the open
 *  ones. A `?` is the honest mark for it; a 0 would say the crew
 *  shipped nothing.
 *
 *  `unlinked` is the runs no listed session claims — a hand dispatch,
 *  or a session outside the read window. They get a platform of their
 *  own rather than being dropped, because a run drawn nowhere is the
 *  false-empty class.
 *
 *  A CREW IS A SESSION, NOT AN ACTOR (backlog 846ab934). One agent
 *  identity runs many sessions at once — on 2026-09-24 all three crews
 *  on the floor were `claude@algedonic.dev` — so the platform is keyed
 *  by the session and labelled with who AND which session, or two
 *  crews of one actor could not be told apart on the picture either. */
export function crewPlatforms(
  crews: ReadonlyArray<Crew>,
  unlinked: ReadonlyArray<unknown>,
): ReadonlyArray<Platform> {
  const platforms = crews.map((c): Platform => {
    const who = c.session.actor ?? c.session.title;
    const state = c.idle === null ? 'silence not measured' : c.idle ? 'idle' : 'at work';
    const prompts = c.session.promptCount === null ? null : `${c.session.promptCount} prompts`;
    return {
      key: `session:${c.session.id}`,
      name: `${who} · ${c.session.id.slice(0, 8)}`,
      standing: c.runs.length,
      bound: null,
      rate: null,
      flag: { from: 'tail', n: 0 },
      note: [state, prompts, c.session.host].filter((p) => p !== null && p !== '').join(' · '),
    };
  });
  const sorted = [...platforms].sort(
    (a, b) => (b.standing ?? 0) - (a.standing ?? 0) || a.name.localeCompare(b.name),
  );
  return unlinked.length === 0
    ? sorted
    : [
        ...sorted,
        {
          key: 'no session',
          name: 'no session',
          standing: unlinked.length,
          bound: null,
          rate: null,
          flag: { from: 'tail', n: unlinked.length },
          note: 'dispatched by hand, or by a session this read did not reach',
        },
      ];
}

/** Receiving: a platform per channel, the channels holding flagged
 *  work first, then the deepest. `rows` is every inbound packet the
 *  page read — open ones stand, ones that closed inside the window are
 *  what left. */
export function receivingPlatforms(
  rows: ReadonlyArray<InboundRow>,
  today: string,
  days: ReadonlyArray<string>,
): ReadonlyArray<Platform> {
  const window = new Set(days);
  const platforms = CHANNELS.map((channel: Channel): Platform => {
    const mine = rows.filter((r) => r.channel === channel);
    const ages = mine
      .filter((r) => r.status === 'open')
      .map((r) => ageDays(r.openedOn, today))
      .sort((a, b) => b - a);
    const stale = ages.filter((d) => d > AGE_THRESHOLDS.stale).length;
    const oldest = ages[0];
    return {
      key: channel,
      name: channel,
      standing: ages.length,
      // A channel has no WIP bound — what it is read against is the
      // age band, which the flag carries.
      bound: null,
      rate: mine.filter((r) => r.closedOn !== null && window.has(r.closedOn)).length,
      flag: { from: 'head', n: stale },
      note:
        oldest === undefined
          ? 'nothing standing'
          : `oldest ${oldest} d${stale > 0 ? ` · ${stale} past the ${AGE_THRESHOLDS.stale}-day band` : ''}`,
    };
  });
  return [...platforms].sort(
    (a, b) => b.flag.n - a.flag.n || (b.standing ?? 0) - (a.standing ?? 0) || a.name.localeCompare(b.name),
  );
}

// ---------------------------------------------------------------------
// Where the platforms go inside the outline.
// ---------------------------------------------------------------------

/** One standing packet, placed. */
export type PlacedMark = Readonly<{ x: number; y: number; w: number; h: number; flagged: boolean }>;

export type PlacedPlatform = Readonly<{
  platform: Platform;
  /** The platform's own block, inside the territory's rect. */
  x: number;
  y: number;
  w: number;
  h: number;
  /** The track the marks stand on. */
  trackY: number;
  marks: ReadonlyArray<PlacedMark>;
  /** Packets the track had no room for — counted, never dropped. */
  overflow: number;
  /** How many packets each mark stands for. 1 when the whole queue
   *  fits; more when it does not, and the renderer says so — a track
   *  of 18 marks under a count of 30 is a scale, not a miscount. */
  perMark: number;
  /** Where the bound falls along the track, or null when the queue is
   *  no longer than its bound: a marker at the very end would say
   *  nothing, and one past it would be a guess about where. */
  boundX: number | null;
}>;

/** A platform: a name line, then the track its packets stand on. */
const PLATFORM_H = 24;
const LABEL_H = 11;
const MARK_W = 4;
const MARK_H = 8;
const MARK_GAP = 2;
/** Room kept at the right of every track for the rate figure, so a
 *  full queue's marks never run under the number beside them. */
const RATE_W = 26;

/** Stack the platforms under the territory's zoomed header. What does
 *  not fit is COUNTED at both levels — platforms past the rect's room,
 *  and marks past the track's — because a region that drew a tidy
 *  subset would read as a quiet one. */
export function platformLayout(
  t: Territory,
  platforms: ReadonlyArray<Platform>,
): Readonly<{ placed: ReadonlyArray<PlacedPlatform>; hidden: number; note: NoteAt }> {
  // The room the region's contents may use — head off the top, edge
  // and MACHINERY STRIP off the bottom. One definition, shared with
  // `interiorLayout` (backlog 3a916816): this divided the same canvas
  // without the strip's term, so a platform row and a machine glyph
  // could be placed in the same pixels.
  const box = contentsBox(t, PLATFORM_H);
  const w = box.w;
  const rows = Math.max(1, Math.floor(box.h / PLATFORM_H));
  const per = Math.max(1, Math.floor((w - RATE_W + MARK_GAP) / (MARK_W + MARK_GAP)));
  const placed = platforms.slice(0, rows).map((platform, i): PlacedPlatform => {
    const y = box.y + i * PLATFORM_H;
    const trackY = y + LABEL_H;
    const standing = platform.standing ?? 0;
    const drawn = Math.min(standing, per);
    // A queue longer than the track is drawn as the WHOLE queue at a
    // scale: each mark stands for `perMark` packets, so the bound and
    // the packets past it stay where they belong in the queue. Marks
    // that simply stopped at the track's end would put a 24-deep bound
    // off the picture exactly when it matters — a 30-deep station over
    // its limit would draw no bound at all.
    const perMark = drawn === 0 ? 1 : standing / drawn;
    const trackLen = drawn === 0 ? 0 : drawn * (MARK_W + MARK_GAP) - MARK_GAP;
    const marks = Array.from({ length: drawn }, (_, k): PlacedMark => ({
      x: box.x + k * (MARK_W + MARK_GAP),
      y: trackY,
      w: MARK_W,
      h: MARK_H,
      flagged:
        platform.flag.from === 'head'
          ? k * perMark < platform.flag.n
          : (k + 1) * perMark > standing - platform.flag.n,
    }));
    const bound = platform.bound;
    return {
      platform,
      x: box.x,
      y,
      w,
      h: PLATFORM_H,
      trackY,
      marks,
      overflow: standing - drawn,
      perMark,
      boundX:
        bound !== null && bound > 0 && bound < standing
          ? box.x + (bound / standing) * trackLen
          : null,
    };
  });
  return { placed, hidden: Math.max(0, platforms.length - placed.length), note: overflowNoteAt(box) };
}
