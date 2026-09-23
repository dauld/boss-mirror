// Where a Job currently sits on its Workflow's map — the one
// definition shared by every surface that groups Jobs by step
// (TriageFlow's queues, FleetPage's node lists). Extracted so the
// two cannot drift (CLAUDE.md §9a): the key must match how the
// fleet aggregate groups server-side — spec_slug with the title as
// the pre-migration-100 fallback — or the client's item lists and
// the server's depth badges disagree about which node a Job is on.
import type { Job, Step } from './types';

/// A Job's current in-flight step (first `ready`/`active` by the
/// server's sort order), if any.
export function currentStep(j: Job): Step | undefined {
  const steps = Array.isArray(j.steps) ? j.steps : [];
  return steps.find((s) => s.status === 'ready' || s.status === 'active');
}

/// The phrase for a step a packet is STANDING AT: its name with its
/// own status beside it, because step titles are perfect-tense by house
/// convention and `DEPARTED — merged into main` alone was read as done
/// while main had not moved (train 47391bfc, 2026-09-23; 648a68a9). The
/// server spells it as `boss_jobs::yard::standing_at` and hands it as-is
/// where it can (the yard status's `at_step`); a raw step row carries no
/// such field, so the web spells it here once, held equal to the
/// server's format string by position.test.ts (3102fe7a).
export function standingAt(name: string, status: string): string {
  return `${name} (${standingNote(status)})`;
}

/// The part of [`standingAt`] beside the name, for a surface that
/// already shows the name in its own place (the journey's stops).
export function standingNote(status: string): string {
  return `${status}, not yet done`;
}

/// The fleet-node key the Job sits at: `spec_slug`, else title.
export function positionOf(j: Job): string | null {
  const current = currentStep(j);
  if (!current) return null;
  const slug = (current as Step & { spec_slug?: string | null }).spec_slug;
  return slug && slug !== '' ? slug : (current.title ?? null);
}

/// Where the Job stands, as a person reads it: its position with the
/// current step's status beside it (`promoted (ready, not yet done)`),
/// or `null` when no step is in flight.
export function standingOf(j: Job): string | null {
  const current = currentStep(j);
  const pos = positionOf(j);
  return current && pos ? standingAt(pos, current.status) : null;
}

const PRIORITY_ORDER: Record<string, number> = {
  emergency: 0,
  urgent: 1,
  standard: 2,
  scheduled: 3,
};

/// Group Jobs by their position, each list priority-first then
/// oldest-first — the queue order a triager works in.
export function groupByPosition(jobs: ReadonlyArray<Job>): Map<string, Job[]> {
  const out = new Map<string, Job[]>();
  for (const j of jobs) {
    const pos = positionOf(j);
    if (!pos) continue;
    (out.get(pos) ?? out.set(pos, []).get(pos)!).push(j);
  }
  for (const list of out.values()) {
    list.sort(
      (a, b) =>
        (PRIORITY_ORDER[a.priority ?? 'standard'] ?? 2) -
          (PRIORITY_ORDER[b.priority ?? 'standard'] ?? 2) ||
        String(a.opened_on ?? '').localeCompare(String(b.opened_on ?? '')),
    );
  }
  return out;
}
