// Aborting a job asks for the reason — the pure half of the job page's
// Abort control (design c6f9fb3e, accepted 2026-09-15; backlog 7a98040e).
//
// David, feedback 33324fe9 on /ux/jobs/ac356440: "I need to be able to
// Abort a job in the UI, but make me provide a reason." An abort was
// already a step completion: most workflows declare an abandoned /
// cancelled / declined terminal with `outcome_kind = aborted`, so
// completing it with a reason closes the packet, evented, on the
// record. What did not exist was one control that FINDS that step and
// asks for the reason first — the operator had to know which step was
// the abort and complete it through the step inspector.
//
// Everything here derives from the job's own materialised steps, which
// are the workflow row pinned at admission with `metadata_defaults`
// stamped in. Nothing hardcodes a step name: `abandoned`, `cancelled`,
// `declined`, `refused` and `failed` all read as aborts because their
// rows say `outcome_kind = aborted`, and a workflow that declares no
// such terminal offers no control — the protocol decides whether a
// job can be aborted, not the page.
import type { Step, StepStatus } from './types';

/// One terminal the Abort control can complete.
export type AbortTerminal = Readonly<{
  id: string;
  /// The authored step name (`spec_slug`) — stable across packets,
  /// unlike `title`, which interpolates metadata. Falls back to the id
  /// for steps materialised before the column existed.
  slug: string;
  title: string;
  /// The role the row admits, or null when the row names none (then
  /// the ordinary step-update policy is the only gate).
  authority_role: string | null;
  status: StepStatus;
  /// The step's metadata as materialised — what the completion body
  /// merges over, since a step PUT replaces metadata wholesale.
  metadata: Record<string, unknown>;
}>;

function str(v: unknown): string | null {
  return typeof v === 'string' && v.length > 0 ? v : null;
}

/// The step(s) whose `outcome_kind` is `aborted`, in step order,
/// excluding ones already terminal (a completed abort is a record, not
/// an offer). Empty means the page shows no control; two or more means
/// the control asks which.
export function abortTerminals(steps: ReadonlyArray<Step> | undefined): AbortTerminal[] {
  return (steps ?? [])
    .filter((s) => s.metadata?.['outcome_kind'] === 'aborted')
    .filter((s) => s.status !== 'completed' && s.status !== 'skipped')
    .slice()
    .sort((a, b) => a.sort_order - b.sort_order)
    .map((s) => ({
      id: s.id,
      slug: s.spec_slug ?? s.id,
      title: s.title,
      authority_role: str(s.metadata?.['authority_role']),
      status: s.status,
      metadata: s.metadata ?? {},
    }));
}

/// "At least one sentence" (decision `reason`): free text, not a code
/// from a list, because the sentence is what the next reader needs. A
/// sentence here is three or more words — enough to refuse `dup` and
/// `not needed` without judging grammar, which no check can do.
export function reasonIsSentence(reason: string): boolean {
  return reason.trim().split(/\s+/).filter((w) => w.length > 0).length >= 3;
}

/// The step PUT body that records the abort, or null when the reason is
/// not a sentence (nothing to send — the modal keeps the field open).
///
/// The metadata is MERGED with what the step already carries: a step
/// PUT replaces top-level metadata wholesale, and `outcome_kind` — the
/// very fact that makes this step the abort — lives in that object.
/// No `completed_by`: the server stamps the signing actor at the flip
/// and overwrites anything a human session sends (c17871fe).
export function abortBody(
  terminal: Pick<Step, 'metadata'>,
  reason: string,
): { status: 'completed'; metadata: Record<string, unknown> } | null {
  if (!reasonIsSentence(reason)) return null;
  return {
    status: 'completed',
    metadata: { ...(terminal.metadata ?? {}), reason: reason.trim() },
  };
}

export type AbortAuthority =
  | { kind: 'admitted' }
  | { kind: 'refused'; role: string | null; why: string };

/// Decision `authority`: whoever the step's `authority_role` admits —
/// the ordinary claim rule, no separate abort permission. A viewer
/// without it sees the control disabled with the role named, the way
/// the passkeys panel names why Remove is disabled. This is the
/// affordance, not the gate: the step API and policy still decide.
export function abortAuthority(
  authorityRole: string | null,
  viewerRole: string | null,
): AbortAuthority {
  if (viewerRole === null) {
    return { kind: 'refused', role: authorityRole, why: 'Sign in to abort this job.' };
  }
  if (authorityRole === null || authorityRole === viewerRole) return { kind: 'admitted' };
  return {
    kind: 'refused',
    role: authorityRole,
    why: `Only ${authorityRole} may abort this job.`,
  };
}
