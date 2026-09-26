// How long a step has sat at ready/active — the queue-age lens,
// `GET /api/jobs/queue-age` (2a0b034e), keyed by step.
//
// The fact is the projection's `became_ready_at` stamp, and the Job /
// Step read does not carry it on purpose: boss-jobs port.rs records "A
// LENS, NOT A FIELD" — hoisting it onto Step measured at a 97-site
// change to Tier-1 core, and a listing field would be a second
// definition of the same instant. So a page that wants "since when"
// makes this ONE extra read and joins it by step id.
//
// This reader was the incidents page's (1a242883); it moved here when
// the department thirds became its second step-keyed reader (backlog
// 66a5d5be), so the two cannot parse or print a wait differently
// (CLAUDE.md §9a).

import { fetchRemote, type Remote } from '../data/remote';

export const QUEUE_AGE_PATH = '/api/jobs/queue-age';

/// `exact: false` means the stamp is an `updated_at` fallback: a LOWER
/// bound, and it has to render as one.
export type StepWait = Readonly<{ sinceMs: number; exact: boolean }>;
export type StepWaits = Readonly<{ now: number | null; byStep: ReadonlyMap<string, StepWait> }>;

const text = (v: unknown): string | null =>
  typeof v === 'string' && v.trim() !== '' ? v : null;

/// Parse the lens's body. A body without a `data` list is a throw, so a
/// wrong-shaped answer renders as unreadable, never as "no waits".
export function parseStepWaits(raw: unknown): StepWaits {
  const body = raw as { data?: unknown; now?: unknown } | null;
  if (typeof body !== 'object' || body === null || !Array.isArray(body.data)) {
    throw new Error('queue-age: the answer carries no data list');
  }
  const byStep = new Map<string, StepWait>(
    (body.data as ReadonlyArray<Record<string, unknown>>).flatMap((r) => {
      const id = text(r['step_id']);
      const sinceMs = Date.parse(text(r['since']) ?? '');
      return id === null || Number.isNaN(sinceMs)
        ? []
        : [[id, { sinceMs, exact: r['exact'] === true }] as const];
    }),
  );
  const now = Date.parse(text(body.now) ?? '');
  return { now: Number.isNaN(now) ? null : now, byStep };
}

/// The one read, ready or failed — a failure carries its message, so
/// the page can say why rather than render no ages.
export function loadStepWaits(): Promise<Exclude<Remote<StepWaits>, { kind: 'loading' }>> {
  return fetchRemote(QUEUE_AGE_PATH, parseStepWaits);
}

/// A span as a card reads it: `<1m`, `12m`, `5h 12m`, `3d 4h`.
export function durationText(ms: number): string {
  const min = Math.floor(Math.max(0, ms) / 60_000);
  if (min < 1) return '<1m';
  if (min < 60) return `${min}m`;
  const h = Math.floor(min / 60);
  if (h < 24) return `${h}h ${min % 60}m`;
  return `${Math.floor(h / 24)}d ${h % 24}h`;
}

/// How long a step has waited at `nowMs`, with `≥` when the stamp is
/// the fallback — a floor, never passed off as the figure.
export function waitedText(w: StepWait, nowMs: number): string {
  return `${w.exact ? '' : '≥'}${durationText(nowMs - w.sinceMs)}`;
}

/// Ages are measured against the server's clock when the lens sent one
/// (the stack may run a simulated clock), else the caller's.
export function lensNow(waits: StepWaits, fallbackMs: number): number {
  return waits.now ?? fallbackMs;
}
