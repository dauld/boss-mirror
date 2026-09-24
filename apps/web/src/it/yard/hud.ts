// THE HUD FRAME — whole-system figures, fixed, one row per third
// (design 00774ca8, approved by David 2026-09-24; car F of design
// 62de32ae). The map answers about the PARTS; this answers about the
// WHOLE (design 17567423): per third of the operator surface, is work
// arriving faster than it leaves, and is anything past its bound; and,
// beside the rows, is any machine failed.
//
// NOTHING HERE ADDS ANYTHING UP. Every figure is a field of the one
// `/api/yard/regions` payload that `boss orient` also prints — the
// server's `thirds` block (balance, net included, and the stuck reading)
// and its `machines` cell. The one aggregate that used to stand on the
// world view, `borders.ts::summaryLine`, summed border rows on the
// client and double-counted about 260 backlog items; it is retired with
// this frame (decision 10). The row order and each row's membership come
// from the payload too — this file names no third.
//
// UNKNOWN AND ZERO LOOK DIFFERENT (decision 5, the bar 17567423 set as
// the one that cannot be negotiated). A figure is one of four pictures:
//
//   value   a measured number, plain — or on the troubled plate where
//           the design says so (a stuck count above zero, a failed
//           machine)
//   zero    a true zero: `0`, with no mark at all
//   floor   `≥ n` followed by `?` in its own band — the server counted n
//           and says some of it could not be counted
//   unread  `?` in a dashed housing, never 0 and never blank — the input
//           could not be read
//
// and when the whole read fails every cell turns `unread` and the header
// says "read failed HH:MMZ · last good HH:MMZ". The last good VALUES are
// not kept on screen: a stale figure shown as current is an answer where
// there should have been an error. Only the rows' names survive, so the
// frame keeps its shape.
//
// THE ONE STATED EXCEPTION (decision 4): the delivery row — the row whose
// regions include `arrivals` — shows arrivals per day against the window
// before, read from the SAME field the arrivals territory reads
// (`arrivals.trend`). One field rendered twice cannot drift.

import type { Remote } from '../../data/remote';
import type { MachineAt, Regions, Third } from './regions';

export type Figure =
  | Readonly<{ kind: 'value'; text: string; troubled: boolean }>
  | Readonly<{ kind: 'zero' }>
  | Readonly<{ kind: 'floor'; text: string; troubled: boolean; why: string }>
  | Readonly<{ kind: 'unread'; why: string }>;

export type HudRow = Readonly<{
  /** The server's name for the third — `queue-management`. */
  third: string;
  /** The same name as a heading — `Queue management`. */
  label: string;
  regions: ReadonlyArray<string>;
  /** The net with its direction, per day. */
  net: Figure;
  /** The two rates it is the difference of, and their unit — a net
   *  without its rates cannot be checked. */
  into: Figure;
  out: Figure;
  unit: string;
  /** What one packet in, and one out, is — the hover. */
  means: string;
  stuck: Figure;
  waiting: Figure;
  /** "oldest 7d" — the oldest STUCK packet, as the server aged it. */
  oldest: string | null;
  /** Decision 4's exception: only on the row that owns `arrivals`. */
  arrivals: Readonly<{ current: Figure; previous: Figure }> | null;
}>;

export type HudMachines = Readonly<{
  failed: Figure;
  unjudged: Figure;
  total: Figure;
  /** Each failed or unjudged machine, a link to its region's map. */
  listed: ReadonlyArray<MachineAt>;
}>;

export type Hud = Readonly<{
  /** "read 4s ago, window 24h" — or the failure, with the last good. */
  header: string;
  /** Where the read is: `ok`, `loading` (nothing landed yet) or
   *  `failed` — in both of the last two every cell is unread, and only
   *  a failure is drawn as one. */
  read: 'ok' | 'loading' | 'failed';
  /** The whole read failed (or has not landed): every cell is unread. */
  failed: boolean;
  rows: ReadonlyArray<HudRow>;
  machines: HudMachines;
}>;

/** A number as the frame prints it: one decimal at most, no `.0`. */
const num = (v: number): string => String(Math.round(v * 10) / 10);

const unread = (why: string): Figure => ({ kind: 'unread', why });

/** A count or rate the server gave, as a figure: zero is its own
 *  picture, null is unread. */
function figure(v: number | null, why: string, troubled = false): Figure {
  if (v === null) return unread(why);
  if (v === 0) return { kind: 'zero' };
  return { kind: 'value', text: num(v), troubled };
}

/** The net with its direction: `+12/day`, `−3/day`, or a plain zero. */
function netFigure(v: number | null, why: string): Figure {
  if (v === null) return unread(why);
  if (v === 0) return { kind: 'zero' };
  // Balance wears no colour until a band is declared for it (decision
  // 7): growth alone is not a crossed band.
  return { kind: 'value', text: `${v > 0 ? '+' : '−'}${num(Math.abs(v))}/day`, troubled: false };
}

/** `queue-management` → `Queue management`. */
export function labelOf(third: string): string {
  const words = third.replace(/-/g, ' ');
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/** `170` hours → `7d`, `5` → `5h`. */
function ageText(hours: number): string {
  return hours >= 48 ? `${Math.floor(hours / 24)}d` : `${hours}h`;
}

/** THE STUCK FIGURE, exactly as the server gives it: above zero it is
 *  ours and not moving, so troubled (decision 7); a non-empty `unknown`
 *  makes it a floor. */
function stuckFigure(t: Third): Figure {
  const n = t.stuck.stuck;
  if (n === null) return unread('the server sent no stuck count for this third');
  if (t.stuck.unknown.length > 0) {
    return { kind: 'floor', text: `≥ ${n}`, troubled: n > 0, why: t.stuck.unknown.join(' · ') };
  }
  return figure(n, '', true);
}

function rowOf(t: Third, data: Regions): HudRow {
  const why = t.balance.why ?? 'not read';
  const owns = t.regions.includes('arrivals');
  const trend = owns ? data.regions.find((r) => r.name === 'arrivals')?.trend : undefined;
  const noTrend = 'the arrivals region sent no trend';
  return {
    third: t.third,
    label: labelOf(t.third),
    regions: t.regions,
    net: netFigure(t.balance.net, why),
    into: figure(t.balance.in, why),
    out: figure(t.balance.out, why),
    unit: t.balance.unit,
    means: `in: ${t.balance.in_means} · out: ${t.balance.out_means}`,
    stuck: stuckFigure(t),
    waiting: figure(t.stuck.waiting, 'the server sent no waiting count for this third'),
    oldest: t.stuck.oldest_hours === null ? null : `oldest ${ageText(t.stuck.oldest_hours)}`,
    arrivals: owns
      ? {
          current: figure(trend?.current ?? null, noTrend),
          previous: figure(trend?.previous ?? null, noTrend),
        }
      : null,
  };
}

/** A row whose every figure is unread — the shape kept, the values not. */
function blankRow(third: string, regions: ReadonlyArray<string>, why: string): HudRow {
  return {
    third,
    label: third === '' ? '?' : labelOf(third),
    regions,
    net: unread(why),
    into: unread(why),
    out: unread(why),
    unit: '',
    means: '',
    stuck: unread(why),
    waiting: unread(why),
    oldest: null,
    arrivals: regions.includes('arrivals') ? { current: unread(why), previous: unread(why) } : null,
  };
}

/** The frame keeps three rows before any payload has named them: the
 *  operator surface has three thirds (David, 2026-09-08), and a frame
 *  that grew rows as reads landed would move under the eye. */
const UNNAMED_ROWS = 3;

function blankMachines(why: string): HudMachines {
  return { failed: unread(why), unjudged: unread(why), total: unread(why), listed: [] };
}

const hhmm = (ms: number): string => `${new Date(ms).toISOString().slice(11, 16)}Z`;

/** How long ago the read landed: `4s`, `3m`, `2h`. */
function ago(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  return `${Math.floor(s / 3600)}h`;
}

export type LastGood = Readonly<{ at: number; data: Regions }>;

/** THE FRAME from the read. `read` is the latest attempt, landed at
 *  `readAt`; `lastGood` is the newest read that succeeded — its time for
 *  the failure header and its row NAMES for the shape, never its values. */
export function hudOf(
  read: Remote<Regions>,
  readAt: number | null,
  lastGood: LastGood | null,
  now: number,
): Hud {
  if (read.kind !== 'ready') {
    const why = read.kind === 'failed' ? `the read failed — ${read.error}` : 'not read yet';
    const shape = lastGood?.data.thirds ?? [];
    const rows =
      shape.length > 0
        ? shape.map((t) => blankRow(t.third, t.regions, why))
        : Array.from({ length: UNNAMED_ROWS }, () => blankRow('', [], why));
    const header =
      read.kind === 'loading'
        ? 'reading…'
        : `read failed ${hhmm(readAt ?? now)} · ${lastGood ? `last good ${hhmm(lastGood.at)}` : 'no good read yet'}`;
    return { header, read: read.kind, failed: true, rows, machines: blankMachines(why) };
  }
  const data = read.data;
  const header = `read ${ago(now - (readAt ?? now))} ago, window ${data.window_hours}h`;
  const older = 'the server sends no thirds block';
  const rows =
    data.thirds.length > 0
      ? data.thirds.map((t) => rowOf(t, data))
      : Array.from({ length: UNNAMED_ROWS }, () => blankRow('', [], older));
  const m = data.machines;
  const machines: HudMachines =
    m === null
      ? blankMachines('the server sends no machine count')
      : {
          failed: figure(m.failed, '', true),
          unjudged: figure(m.unknown, ''),
          total: figure(m.total, ''),
          listed: m.failed_or_unknown,
        };
  return { header, read: 'ok', failed: false, rows, machines };
}
