// A step's authored instructions — the data half of "the surface must
// say what doing this step right looks like" (backlog 3c640ac3).
//
// Workflow steps carry a `procedure` in their metadata_defaults:
// authored prose saying what the step is for, what a legitimate answer
// looks like, and what not to do. It is versioned with the protocol and
// is some of the best-written documentation in the system, and until
// this module existed no surface read it by name — the generic surface
// showed it only incidentally, flattened into one paragraph under a
// lowercase key, and the thirteen plugin bundles did not show it at all.
//
// David, 2026-09-11, after completing `fold` on design-doc dacea60d
// exactly right: "I am not sure that I completed the step right." The
// step's own procedure answers him in its first sentence. The cost of
// an invisible instruction is not a wrong answer; it is a correct
// answer nobody can tell is correct.
//
// Pure so it is testable without a DOM (`bun test` has no Svelte pass);
// the component owns the rendering. The key name lives here once, so
// the panel and the generic surface that must STOP dumping it cannot
// disagree about what it is called (CLAUDE.md §9a).

/// The metadata key the Workflow registry authors a step's procedure
/// under.
export const PROCEDURE_KEY = 'procedure';

/// The step's instructions, or `null` when it authors none.
///
/// Blank is an absence, and so is a non-string: a step with no
/// procedure must render exactly as it did before this existed — no
/// empty heading, no placeholder.
export function procedureFromStep(
  stepMetadata: Record<string, unknown>,
): string | null {
  const raw = stepMetadata[PROCEDURE_KEY];
  if (typeof raw !== 'string') return null;
  return raw.trim().length > 0 ? raw : null;
}
