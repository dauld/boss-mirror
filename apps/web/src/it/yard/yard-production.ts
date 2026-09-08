// The production panel — what the floor produced today, from the
// arrivals the page already computes. Nothing here is a new endpoint.
//
// HONESTY ABOUT THE WINDOW. The board shows the last few arrived trains
// (`ARRIVALS_SHOWN`) and the last few cancelled ones (`CANCELLED_SHOWN`).
// On a busy day today's trains can outnumber the window, and then a
// count over the window is a floor, not the day's total. `carsWindowed`
// / `trainsWindowed` say so — the tile renders "≥ N" — rather than let
// a truncated page answer a smaller question (a-limit-is-not-a-filter).
//
// The median car lead time is the server's `journey_seconds` on its
// recent trains, when it sends one; today it sends null (a server car
// is adding it), and the tile reads "—", never a number derived from
// something else.

import { ARRIVALS_SHOWN, CANCELLED_SHOWN, type TrainRow, type YardState } from './yard';
import type { YardStatus } from './yard-status';

export type Production = Readonly<{
  /** The UTC day the counts are over, `YYYY-MM-DD`. */
  day: string;
  carsLanded: number;
  carsWindowed: boolean;
  trainsArrived: number;
  trainsCancelled: number;
  trainsWindowed: boolean;
  awaitingProof: number;
  medianJourneyS: number | null;
  journeySamples: number;
  /** Cars landed per UTC hour of the day, 24 buckets. */
  perHour: readonly number[];
}>;

export function utcDay(ms: number): string {
  return new Date(ms).toISOString().slice(0, 10);
}

const onDay = (t: TrainRow, day: string): boolean => t.arrivedAt.ms > 0 && utcDay(t.arrivedAt.ms) === day;

/** A full window whose oldest member is still today may be cut off. */
function windowed(rows: readonly TrainRow[], shown: number, day: string): boolean {
  if (rows.length < shown) return false;
  const oldest = rows.reduce<TrainRow | null>((o, t) => (o === null || t.arrivedAt.ms < o.arrivedAt.ms ? t : o), null);
  return oldest !== null && onDay(oldest, day);
}

function median(xs: readonly number[]): number | null {
  if (xs.length === 0) return null;
  const sorted = [...xs].sort((a, b) => a - b);
  const mid = sorted.length >> 1;
  return sorted.length % 2 === 1 ? sorted[mid]! : (sorted[mid - 1]! + sorted[mid]!) / 2;
}

export function production(yard: YardState, status: YardStatus | null, nowMs: number): Production {
  const day = utcDay(nowMs);
  const arrivedToday = yard.arrivals.filter(t => onDay(t, day));
  const cancelledToday = yard.cancelled.filter(t => onDay(t, day));
  const perHour = arrivedToday.reduce<number[]>((acc, t) => {
    const h = new Date(t.arrivedAt.ms).getUTCHours();
    acc[h] = (acc[h] ?? 0) + t.cars.length;
    return acc;
  }, new Array<number>(24).fill(0));
  const journeys = (status?.recent ?? [])
    .map(r => r.journey_seconds)
    .filter((s): s is number => typeof s === 'number' && Number.isFinite(s));
  return {
    day,
    carsLanded: arrivedToday.reduce((n, t) => n + t.cars.length, 0),
    carsWindowed: windowed(yard.arrivals, ARRIVALS_SHOWN, day),
    trainsArrived: arrivedToday.length,
    trainsCancelled: cancelledToday.length,
    trainsWindowed: windowed(yard.arrivals, ARRIVALS_SHOWN, day) || windowed(yard.cancelled, CANCELLED_SHOWN, day),
    awaitingProof: yard.awaitingProof.length,
    medianJourneyS: median(journeys),
    journeySamples: journeys.length,
    perHour,
  };
}

// ---------------------------------------------------------------------
// The chart: cars landed per hour, one column an hour, to scale, inside
// its own viewBox. Geometry only — the Svelte draws it with the app's
// tokens (one hue for the one series; text in text tokens).
// ---------------------------------------------------------------------

export type Bar = Readonly<{ hour: number; n: number; x: number; y: number; w: number; h: number; current: boolean }>;

export type BarChart = Readonly<{
  w: number;
  h: number;
  /** The baseline's y; bars grow up from it. */
  baseline: number;
  plotH: number;
  max: number;
  bars: readonly Bar[];
}>;

const CHART_W = 360;
const CHART_H = 96;
const PAD_L = 6;
const PAD_R = 6;
const PAD_T = 14;
const PAD_B = 16;
const GAP = 2;

/** 24 columns across the width, the tallest at the plot height, a 2px
 *  surface gap between neighbours. An empty day is 24 zero-height bars
 *  and `max` 0. */
export function landedBars(perHour: readonly number[], currentHour: number): BarChart {
  const max = perHour.reduce((a, b) => Math.max(a, b), 0);
  const plotH = CHART_H - PAD_T - PAD_B;
  const baseline = PAD_T + plotH;
  const slot = (CHART_W - PAD_L - PAD_R) / 24;
  const w = Math.min(slot - GAP, 24);
  const bars = Array.from({ length: 24 }, (_, hour): Bar => {
    const n = perHour[hour] ?? 0;
    const h = max > 0 ? (n / max) * plotH : 0;
    return { hour, n, x: PAD_L + hour * slot + (slot - w) / 2, y: baseline - h, w, h, current: hour === currentHour };
  });
  return { w: CHART_W, h: CHART_H, baseline, plotH, max, bars };
}
