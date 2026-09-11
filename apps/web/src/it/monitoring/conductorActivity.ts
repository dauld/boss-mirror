// The Conductor's activity, read from the packets it already writes.
//
// The Conductor (the `boss train` reconcile loop) drives delivery:
// it boards PR-trains, waits on CI, merges, deploys, and confirms the
// cluster converged — and cancels a train with nothing to board. Every
// one of those is a completed Step on a `pr-train` Job, stamped with an
// RFC3339 `completed_at`. This module turns those Steps into a flat,
// newest-first feed an operator can read during a wait.
//
// It is a PURE function of the packets (Hickey: projections are pure
// functions of the log). The Svelte page fetches and renders; every
// decision about what an entry says, and when it happened, lives here
// where it can be tested. No fetch, no `href`, no clock: the timestamp
// is the packet's, and Pacific formatting is a pure string transform.
//
// Step addressing mirrors the conductor's own `find_step` and the
// yard lens (boss-cli/src/train.rs, boss-jobs/src/yard.rs): by spec
// slug with a title fallback, so a rename in the Workflow is a rename
// in one `StepRef` here — not a silently-empty feed.

import type { JobLite, StepLite } from '../yard/yard';

/** One line in the Conductor feed — a single pipeline action, already
 *  shaped for rendering. `ms`/`at` are the packet's own stamp; the page
 *  sorts and keys on `ms`, and never invents a time. */
export type TimelineEntry = Readonly<{
  /** Stable key for `{#each}` — one action fires at most once per train. */
  key: string;
  trainId: string;
  trainTitle: string;
  /** The batched PR's number, parsed from its url; null before the PR
   *  opens or when the url carries no number. */
  prNumber: number | null;
  prUrl: string | null;
  /** The action's stable kind — for styling/testing, not display. */
  action: TimelineAction;
  /** Plain-English action, e.g. "merged", "CI green", "cluster converged". */
  label: string;
  /** A short suffix: the merge sha, the car count, the converged sha —
   *  or null when the label says enough on its own. */
  detail: string | null;
  /** Epoch ms of `completed_at`, for sorting newest-first. */
  ms: number;
  /** The original RFC3339 stamp, kept for a title/tooltip. */
  at: string;
  /** `completed_at` rendered in Pacific time, e.g. "4:01 PM". */
  timePt: string;
  /** `completed_at`'s Pacific date, e.g. "Sep 5", for date separators. */
  datePt: string;
}>;

export type TimelineAction =
  | 'boarded'
  | 'ci-green'
  | 'ci-failed'
  | 'ci-verdict'
  | 'merged'
  | 'deployed'
  | 'converged'
  | 'cancelled';

/** The most entries the feed carries — recent activity, not history. */
export const MAX_ENTRIES = 60;

/** A step addressed the way the conductor writes it: slug first, title
 *  as the fallback for rows that predate the `spec_slug` column. */
type StepRef = Readonly<{ slug: string; title: string }>;

// The boarding step has been renamed across Workflow versions; any of
// these names means "the consist was fixed and the PR opened". Ordered
// by preference — `pr` is the true boarding moment (it carries the PR
// url), so it wins when more than one is present, and exactly ONE
// "boarded" entry is emitted per train.
const BOARD_CANDIDATES: readonly StepRef[] = [
  { slug: 'pr', title: 'Open the batched PR' },
  { slug: 'assemble', title: 'Assemble the consist' },
  { slug: 'collect', title: 'Collect what is ready to board' },
  { slug: 'board', title: 'Board the train' },
];
const PR: StepRef = { slug: 'pr', title: 'Open the batched PR' };
const CI: StepRef = { slug: 'ci', title: 'CI verdict' };
const MERGED: StepRef = { slug: 'merged', title: 'Merged into main' };
const DEPLOYED: StepRef = { slug: 'deployed', title: 'Deployed to the playground' };
const CONVERGED: StepRef = { slug: 'converged', title: 'Cluster converged' };
const CANCELLED: StepRef = { slug: 'cancelled', title: 'Cancelled — nothing to board' };

function findStep(steps: readonly StepLite[], ref: StepRef): StepLite | null {
  return (
    steps.find(s => (s.spec_slug ?? '') === ref.slug || s.title === ref.title) ??
    null
  );
}

/** A terminal/action reads STRICTLY completed. `skipped` is settled but
 *  did not happen — the terminal close marks unfired steps skipped, so
 *  a cancelled train carries a skipped `converged`; reading that as done
 *  is how an empty train would show a bogus "converged". */
const isCompleted = (s: StepLite | null): boolean =>
  !!s && s.status === 'completed';

/** The conductor's RFC3339 stamp on a step — the `completed_at` column
 *  when present, else the same key in the step's metadata (the column
 *  is day-granular today, so the instant rides in metadata). Returns
 *  null when there is no instant: an action with no honest time is left
 *  off the feed rather than placed at a guessed one. */
function completedAt(s: StepLite | null): string | null {
  if (!s) return null;
  if (typeof s.completed_at === 'string' && s.completed_at !== '') {
    return s.completed_at;
  }
  const md = (s.metadata as { completed_at?: unknown } | null)?.completed_at;
  return typeof md === 'string' && md !== '' ? md : null;
}

function metaStr(s: StepLite | null, key: string): string | null {
  const v = (s?.metadata as Record<string, unknown> | null | undefined)?.[key];
  return typeof v === 'string' && v !== '' ? v : null;
}

/** The PR number from a forge url — its trailing path segment, when
 *  numeric (`.../pulls/234` → 234). null keeps the row honest rather
 *  than printing a fabricated "#0". */
export function prNumberFromUrl(url: string | null): number | null {
  if (!url) return null;
  const m = url.match(/(\d+)\/*$/);
  if (!m) return null;
  const n = Number.parseInt(m[1] ?? '', 10);
  return Number.isFinite(n) ? n : null;
}

const shortRef = (ref: string | null): string | null =>
  ref ? ref.slice(0, 8) : null;

const prefixArrow = (ref: string | null): string | null =>
  ref ? `→ ${ref}` : null;

/** Format an instant in Pacific time (the stack stores UTC; the
 *  operator reads PT). "4:01 PM". Empty string on an unparseable
 *  stamp so a bad row degrades rather than throwing. */
export function pacificTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '';
  return d.toLocaleTimeString('en-US', {
    timeZone: 'America/Los_Angeles',
    hour: 'numeric',
    minute: '2-digit',
  });
}

/** The Pacific calendar date of an instant, for date separators.
 *  "Sep 5". Note this is the PT date, which can differ from the UTC
 *  date for late-evening-UTC stamps. */
export function pacificDate(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '';
  return d.toLocaleDateString('en-US', {
    timeZone: 'America/Los_Angeles',
    month: 'short',
    day: 'numeric',
  });
}

/** The boarded cars as a short phrase. Without a name map (the caller
 *  chose not to join the ship-a-change packets) it is a count; with
 *  one it names up to two branches and counts the rest. */
function carSummary(
  ids: readonly string[],
  names: ReadonlyMap<string, string> | undefined,
): string | null {
  const n = ids.length;
  if (n === 0) return null;
  const count = n === 1 ? '1 car' : `${n} cars`;
  if (!names) return count;
  const resolved = ids.map(id => names.get(id) ?? id.slice(0, 8));
  const shown = resolved.slice(0, 2).join(', ');
  const more = resolved.length > 2 ? ` +${resolved.length - 2} more` : '';
  return `${count}: ${shown}${more}`;
}

function boardedIds(train: JobLite): readonly string[] {
  const v = (train.metadata as { boarded_jobs?: unknown } | null)?.boarded_jobs;
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : [];
}

/** The CI action + its label, from the verdict the `ci` step recorded. */
function ciLabel(result: string | null): { action: TimelineAction; label: string } {
  if (result === 'green') return { action: 'ci-green', label: 'CI green' };
  if (result === 'failing') return { action: 'ci-failed', label: 'CI failed' };
  return { action: 'ci-verdict', label: 'CI verdict' };
}

type TrainContext = Readonly<{
  train: JobLite;
  prUrl: string | null;
  prNumber: number | null;
}>;

/** Build one entry for a completed step, or null when the step is
 *  absent, unfinished, or carries no timestamp. */
function entryFor(
  ctx: TrainContext,
  step: StepLite | null,
  action: TimelineAction,
  label: string,
  detail: string | null,
): TimelineEntry | null {
  if (!isCompleted(step)) return null;
  const at = completedAt(step);
  if (at === null) return null;
  const ms = Date.parse(at);
  if (Number.isNaN(ms)) return null;
  return {
    key: `${ctx.train.id}:${action}`,
    trainId: ctx.train.id,
    trainTitle: ctx.train.title,
    prNumber: ctx.prNumber,
    prUrl: ctx.prUrl,
    action,
    label,
    detail,
    ms,
    at,
    timePt: pacificTime(at),
    datePt: pacificDate(at),
  };
}

function trainEntries(
  train: JobLite,
  names: ReadonlyMap<string, string> | undefined,
): TimelineEntry[] {
  const steps = train.steps ?? [];
  const prUrl = metaStr(findStep(steps, PR), 'pr_url');
  const ctx: TrainContext = { train, prUrl, prNumber: prNumberFromUrl(prUrl) };
  const out: TimelineEntry[] = [];

  // boarded — the first completed boarding-family step (pr preferred).
  const boardStep =
    BOARD_CANDIDATES.map(ref => findStep(steps, ref)).find(isCompleted) ?? null;
  const boarded = entryFor(
    ctx,
    boardStep,
    'boarded',
    'boarded',
    carSummary(boardedIds(train), names),
  );
  if (boarded) out.push(boarded);

  // ci
  const ciStep = findStep(steps, CI);
  const ci = ciLabel(metaStr(ciStep, 'result'));
  const ciEntry = entryFor(
    ctx,
    ciStep,
    ci.action,
    ci.label,
    ci.action === 'ci-failed' ? metaStr(ciStep, 'checks') : null,
  );
  if (ciEntry) out.push(ciEntry);

  // merged
  const mergedStep = findStep(steps, MERGED);
  const merged = entryFor(
    ctx,
    mergedStep,
    'merged',
    'merged',
    prefixArrow(shortRef(metaStr(mergedStep, 'merge_ref'))),
  );
  if (merged) out.push(merged);

  // deployed
  const deployed = entryFor(
    ctx,
    findStep(steps, DEPLOYED),
    'deployed',
    'deployed to the playground',
    null,
  );
  if (deployed) out.push(deployed);

  // converged
  const convergedStep = findStep(steps, CONVERGED);
  const converged = entryFor(
    ctx,
    convergedStep,
    'converged',
    'cluster converged',
    prefixArrow(shortRef(metaStr(convergedStep, 'cluster_commit'))),
  );
  if (converged) out.push(converged);

  // cancelled — the alternate terminal. A reason if the step named one.
  const cancelledStep = findStep(steps, CANCELLED);
  const cancelled = entryFor(
    ctx,
    cancelledStep,
    'cancelled',
    'cancelled',
    metaStr(cancelledStep, 'reason') ?? metaStr(cancelledStep, 'note'),
  );
  if (cancelled) out.push(cancelled);

  return out;
}

/**
 * The Conductor's activity across recent trains, newest first.
 *
 * @param trains  recent `pr-train` packets (newest-first from the API;
 *                order here does not matter — the result is re-sorted).
 * @param carNames optional id → branch/title map (from the ship-a-change
 *                packets) so a "boarded" row can name its cars instead
 *                of only counting them.
 */
export function conductorTimeline(
  trains: readonly JobLite[],
  carNames?: ReadonlyMap<string, string>,
): TimelineEntry[] {
  return trains
    .flatMap(t => trainEntries(t, carNames))
    .sort((a, b) => b.ms - a.ms)
    .slice(0, MAX_ENTRIES);
}
