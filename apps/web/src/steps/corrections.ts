// A step's corrections, as the job GET hands them (design 4105b020,
// backlog 56727f95).
//
// A completed step is a fact and is never rewritten; a correction sits
// BESIDE it. Until the corrections door, the correction sat in a job
// metadata key of its author's invention (25 of them for ~132
// corrections, measured 2026-09-23) and a reader of the step saw the
// damaged sentence with no signal that anything corrected it —
// f3e091f0's triage evidence still reads "Ordering trap confirmed:  is
// required of every rule". The server now attaches the entries that
// target a step as `step.corrections`, each with its `index` in the
// job's append-only list, so a reader renders a list it was GIVEN
// rather than searching job metadata for one.
//
// Read here once, for the one component that draws them
// (StepCorrections.svelte), so every surface says the same thing about
// the same entry (§9a). The keys mirror the server's entry shape,
// crates/core/boss-jobs/src/corrections.rs (`entry_for`, `for_step`).

/** One correction of a field: what the field reads now, and what it
 *  should read. Both verbatim — never merged into one text. */
export type FieldCorrection = Readonly<{
  kind: 'correction';
  index: number;
  field: string;
  reads: string;
  shouldRead: string;
  why: string;
  by: string;
  at: string;
  /** The index of the later entry that withdrew this one, or null. */
  withdrawnBy: number | null;
}>;

/** An appended entry withdrawing an earlier correction (Q4: a
 *  correction is corrected only by appending). */
export type CorrectionWithdrawal = Readonly<{
  kind: 'withdrawal';
  index: number;
  field: string;
  withdraws: number;
  why: string;
  by: string;
  at: string;
}>;

export type StepCorrection = FieldCorrection | CorrectionWithdrawal;

const str = (v: unknown): string => (typeof v === 'string' ? v : '');
const int = (v: unknown): number | null =>
  typeof v === 'number' && Number.isInteger(v) && v >= 0 ? v : null;

/** The corrections a step carries, in the list's order. An entry
 *  without an index or a field is not one the server wrote, and is
 *  skipped rather than drawn as a guess. */
export function stepCorrections(step: unknown): readonly StepCorrection[] {
  const raw = (step as { corrections?: unknown } | null | undefined)?.corrections;
  if (!Array.isArray(raw)) return [];
  const entries = raw.filter(
    (e): e is Record<string, unknown> => typeof e === 'object' && e !== null,
  );
  const withdrawnBy = new Map<number, number>(
    entries.flatMap((e) => {
      const target = int(e.withdraws);
      const index = int(e.index);
      return target !== null && index !== null ? [[target, index] as const] : [];
    }),
  );
  return entries.flatMap((e): StepCorrection[] => {
    const index = int(e.index);
    const field = str(e.field);
    if (index === null || field === '') return [];
    const common = { index, field, why: str(e.why), by: str(e.by), at: str(e.at) };
    const withdraws = int(e.withdraws);
    if (withdraws !== null) return [{ kind: 'withdrawal', withdraws, ...common }];
    if (typeof e.reads !== 'string' || typeof e.should_read !== 'string') return [];
    return [
      {
        kind: 'correction',
        ...common,
        reads: e.reads,
        shouldRead: e.should_read,
        withdrawnBy: withdrawnBy.get(index) ?? null,
      },
    ];
  });
}
