// An active step keeps the holder that claimed it until it is released
// (backlogs 650ebd0c, 0f42efa0). The jobs API refuses a step PUT that
// replaces the holder of an ACTIVE step, or clears it without moving
// the step out of active; an active step changes hands by release
// (`boss step release`, the abandoned-step reclaim, or the Release
// control these surfaces now carry) and then a claim. These answers are
// what the step surfaces read so a write carries only what its gesture
// changed — never a field copied from the snapshot the page was drawn
// from (backlog 6ef4a36b).

type Held = Readonly<{ status: string; assignee_id: string | null }>;

/// Is the step's holder fixed? True for an active step whose holder
/// names someone — `null`, `""` and a blank name nobody, as on the
/// server.
export function holderLocked(step: Held): boolean {
  return step.status === 'active' && (step.assignee_id ?? '').trim() !== '';
}

/// The `assignee_id` a surface's write carries, as a spreadable part:
/// the operator's pick when it differs from the stored holder, and
/// nothing otherwise. Nothing on a locked step, whatever the picker
/// holds (it can be stale — backlog 848477c3): omitting the key keeps
/// the stored holder. And nothing when the picker is untouched — the
/// snapshot is not a change, and sending it back is how a Save after an
/// agent's claim came to carry `assignee_id: null` (6ef4a36b).
export function assigneeChange(
  step: Held,
  picked: string,
): { assignee_id?: string | null } {
  if (holderLocked(step)) return {};
  const next = picked.trim() || null;
  const stored = (step.assignee_id ?? '').trim() || null;
  return next === stored ? {} : { assignee_id: next };
}

/// A write's `status` and `assignee_id`: the status only when the
/// gesture moves the step (Start, Complete), never the snapshot's own.
/// The surfaces sent `status: overrides.status ?? step.status`, so a
/// page drawn while the step was ready, saved after an agent claimed
/// it, sent `{status: ready, assignee_id: null}` — the release body —
/// and the server released the claim, as it must (backlog 6ef4a36b).
export function gestureFields(
  step: Held,
  picked: string,
  status?: string,
): { status?: string; assignee_id?: string | null } {
  return { ...(status ? { status } : {}), ...assigneeChange(step, picked) };
}

/// The line a locked picker shows beside itself.
export const HOLDER_LOCKED_NOTE =
  'held — an active step changes hands by release, then claim';

/// The release: the one body that takes an active step off its holder,
/// and the one `boss step release` and the abandoned-step reclaim send.
/// It names nobody — handing the step to someone in the same write is
/// the one-write reassignment the server refuses.
export const RELEASE = { status: 'ready', assignee_id: null } as const;

/// The step's run edge, `boss_jobs::agent_runs::EDGE_KEY` — pinned
/// equal to it by holder.test.ts (CLAUDE.md §9a).
export const AGENT_RUN_EDGE = 'agent_run';

/// Where a hand-release's evidence lands on the freed step, the key
/// `boss step release` writes — `boss-cli` `steps::RELEASED_KEY`,
/// pinned equal to it by holder.test.ts (CLAUDE.md §9a).
export const RELEASED_KEY = 'released';

/// What a release writes through the merge door before the status
/// write — `release_patch` in boss-cli/src/steps.rs, key for key:
///
/// - the run edge, cleared ALWAYS. A released step completed before
///   anyone claims it would otherwise deliver onto the run that let it
///   go (b91a2103). This used to clear the edge only when the page's
///   snapshot held one, and the snapshot can predate the dispatcher's
///   write of it — a Ready step still naming a live run (the review of
///   car 675f1858, #1). The merge door deletes a missing key quietly,
///   so the unconditional null costs nothing. The PUT cannot clear a
///   key; only the merge door's `null` can.
/// - `released` — `{why, by, at, from_run}`: a hand-release is
///   testimony, and the reason is the whole artifact it leaves (#2).
///   `from_run` is the run the snapshot named, or null; the null above
///   clears whatever the server holds either way.
export function releaseMetadata(
  metadata: Readonly<Record<string, unknown>>,
  why: string,
  by: string | null,
  at: string,
): Record<string, unknown> {
  return {
    [AGENT_RUN_EDGE]: null,
    [RELEASED_KEY]: { why: why.trim(), by, at, from_run: metadata[AGENT_RUN_EDGE] ?? null },
  };
}

/// The question a page's Release asks before it writes anything — the
/// page's `--why`. `null` when the operator cancels, which releases
/// nothing; a blank answer is refused by `releaseStep`, as the CLI
/// refuses a blank `--why`.
export const RELEASE_QUESTION =
  'Why are you releasing this step? The reason is recorded on it for the next holder.';
export function askReleaseReason(): string | null {
  return window.prompt(RELEASE_QUESTION);
}

type ReadBack = Readonly<{
  status: string;
  assignee_id: string | null;
  metadata: Readonly<Record<string, unknown>>;
}>;

/// A 2xx is a claim; the read-back is the fact — `confirm_released` in
/// boss-cli/src/steps.rs, the four ways a release half-happens, each
/// named. `null` when the step reads back released; otherwise the line
/// saying what it reads back instead.
export function releaseUnconfirmed(step: ReadBack, why: string): string | null {
  if (step.status !== RELEASE.status) {
    return `the step reads back ${step.status} — it was not released`;
  }
  if (step.assignee_id !== null) {
    return `the step reads back ${RELEASE.status} but is still assigned to ${step.assignee_id}`;
  }
  const run = step.metadata[AGENT_RUN_EDGE];
  if (typeof run === 'string') {
    return `the step still names ${AGENT_RUN_EDGE} ${run} — the next completion would deliver onto that run (b91a2103)`;
  }
  const stamp = step.metadata[RELEASED_KEY];
  const recorded =
    stamp && typeof stamp === 'object' ? (stamp as Record<string, unknown>)['why'] : undefined;
  if (recorded !== why.trim()) {
    return 'the step does not read back the reason sent, so the record would not say why it came free';
  }
  return null;
}
