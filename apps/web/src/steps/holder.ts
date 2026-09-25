// An active step keeps the holder that claimed it until it is released
// (backlogs 650ebd0c, 0f42efa0). The jobs API refuses a step PUT that
// replaces the holder of an ACTIVE step, or clears it without moving
// the step out of active; an active step changes hands by release
// (`boss step release`, or the abandoned-step reclaim) and then a
// claim. These two answers are what the step surfaces read so their
// assignee picker never offers that write.

type Held = Readonly<{ status: string; assignee_id: string | null }>;

/// Is the step's holder fixed? True for an active step whose holder
/// names someone — `null`, `""` and a blank name nobody, as on the
/// server.
export function holderLocked(step: Held): boolean {
  return step.status === 'active' && (step.assignee_id ?? '').trim() !== '';
}

/// The `assignee_id` a surface's write carries: the stored holder on a
/// locked step, whatever the picker holds (it can be stale — backlog
/// 848477c3); otherwise the picker, with empty meaning nobody.
export function assigneeToSend(step: Held, picked: string): string | null {
  if (holderLocked(step)) return step.assignee_id;
  return picked || null;
}

/// The line a locked picker shows beside itself.
export const HOLDER_LOCKED_NOTE =
  'held — an active step changes hands by release, then claim';
