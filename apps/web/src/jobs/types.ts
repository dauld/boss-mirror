// Job types — shape matches the JSON wire from /api/jobs.
//
// The Subject type is an open-kind shape, not a discriminated
// union (docs/architecture-decisions.md §Primitives & information
// architecture): kind-specific fields are optional on one shape
// where kind-specific fields are optional. This matches the Rust
// side (boss-core::primitives::Subject is a trait with kind() + id()
// methods; any new kind is just a new string, not a type change).
// New Subject kinds on the backend now land without a TS build
// break.
//
// The trade-off: TS no longer catches a typo in `subject_kind ===
// 'practise'`. SUBJECT_KINDS below is the runtime mirror of the
// canonical list in boss-core; reference it from new code instead
// of inlining string literals.

/// Canonical Subject kind strings, mirrored from
/// boss-core::primitives::SUBJECT_KINDS. Keep in sync when Wave 7
/// introduces new per-kind Subject impls on the Rust side.
export const SUBJECT_KINDS = [
  'asset',
  'account',
  'purchase_order',
  'campaign',
  'employee',
  'vendor',
  'custom',
] as const;

export type SubjectKind = (typeof SUBJECT_KINDS)[number] | (string & {});

/// A Subject is an identity-bearing thing a Job points at.
///
/// Wire shape mirrors Rust `boss_core::job::Subject`:
/// `{ subject_kind, id }`. `subject_kind` stays an open string so a
/// Subject kind the TS code has never heard of still renders.
export type Subject = {
  subject_kind: SubjectKind;
  id: string;
};

export type JobStatus =
  | 'draft' | 'open' | 'blocked' | 'pending-sign-off' | 'closed' | 'cancelled';

/**
 * Five-state predicate-driven lifecycle (mirrors `boss_core::job::StepStatus`).
 * Use the `isPending` / `isTerminal` / `isInFlight` helpers
 * to gate UI rather than comparing against literal strings.
 */
export type StepStatus =
  // Stage 1 — pre-execution
  | 'pending'
  | 'ready'
  // Stage 2 — in-flight
  | 'active'
  // Stage 3 — terminal
  | 'completed'
  | 'skipped';

/** True if the step hasn't yet started (Stage 1). */
export function isPending(status: StepStatus): boolean {
  return status === 'pending'
    || status === 'ready';
}

/** True if the step is terminal (Stage 3). */
export function isTerminal(status: StepStatus): boolean {
  return status === 'completed'
    || status === 'skipped';
}

/** True if the step is in Stage 2 (in-flight). */
export function isInFlight(status: StepStatus): boolean {
  return status === 'active';
}

/// One field of a step's completion contract (mirrors
/// `boss_core::job::StepField`).
///
/// `field_type` is either a scalar name (`string`, `date-time`) or a
/// pipe-shaped enum domain (`reproduce|design|build`). The enum form
/// is what makes a step a FORK: the Workflow's viability lint proves
/// every value has a successor gated on it, and a triage surface can
/// read the domain to know which routes exist without hardcoding them.
export type StepField = {
  name: string;
  field_type: string;
  required?: boolean;
};

export type Step = {
  id: string;
  job_id: string;
  kind: string;
  title: string;
  /// The step's stable identity within its Workflow — the authored
  /// step name, not the rendered `title` (which interpolates packet
  /// metadata and therefore differs per packet). Anything keying off
  /// "which step is this" wants this field; `title` is for humans.
  /// Present on the wire and previously undeclared here, so callers
  /// that needed it type-errored despite the data being there.
  spec_slug?: string;
  assignee_id: string | null;
  status: StepStatus;
  sort_order: number;
  blocked_by: string[];
  /// Step-authored fields, validated in union with the kind bundle's.
  /// Present on the wire; optional here because older callers may not
  /// request enriched steps.
  fields?: StepField[];
  sign_offs_required?: string[];
  sign_offs?: {
    authority_id: string;
    role: string;
    stamped_at: string;
    shape_hash: string;
  }[];
  completed_on: string | null;
  metadata: Record<string, unknown>;
  notes?: string | null;
  /// Pointer to a child Job when this Step's work decomposes
  /// further. Structural column on the `steps` table; traversal
  /// code can discover embedded Jobs without parsing metadata.
  embedded_job?: string | null;
};

export type Job = {
  id: string;
  kind: string;
  subject: Subject;
  title: string;
  owner_id: string;
  status: JobStatus;
  priority: 'emergency' | 'urgent' | 'standard' | 'scheduled';
  opened_on: string;
  due_on: string | null;
  closed_on: string | null;
  metadata: Record<string, unknown>;
  tags: string[];
  steps?: Step[];
};

/// Pick the human-readable identifier for a Subject — the value
/// most useful in a table row or a hero header.
export function subjectLabel(s: Subject): string {
  return s.id || '(unknown subject)';
}

/// Pick the canonical SPA path for a Subject — where clicking the
/// subject in a list should navigate to.
///
/// Same dispatch shape as `subjectLabel`; unknown kinds return `'#'`
/// so existing click handlers don't throw.
export function subjectPath(s: Subject): string {
  switch (s.subject_kind) {
    case 'asset':
      return `/assets/${encodeURIComponent(s.id)}`;
    case 'account':
      return `/accounts/${s.id ?? ''}`;
    case 'purchase_order':
      return `/purchase-orders/${s.id ?? ''}`;
    // A campaign has no page of its own, so it lands on its own
    // packets. It used to land on `kind=marketing-motion`, a
    // tenant workflow hardcoded into shared frontend
    // (backlog 423a531d, 2026-09-22): an instance running another
    // tenant does not publish that kind, so the click reached an
    // empty list. `subject_id` is the filter the /jobs route already
    // parses, and it asks the honest question — this campaign's work,
    // whatever protocol it runs under.
    case 'campaign':
      return `/jobs?subject_id=${encodeURIComponent(s.id ?? '')}`;
    case 'employee':
      return `/people/${s.id ?? ''}`;
    case 'vendor':
      return `/vendors/${s.id ?? ''}`;
    case 'custom':
      return '#';
    default:
      return '#';
  }
}
