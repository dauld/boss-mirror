// Which design docs are actually asking for something, and which are
// just claiming to.
//
// Origin (David, feedback bedda461): "This page is full of stale info.
// What I expected: the page only includes active jobs and maybe a link
// to the Design Library / Flattened ADR representation of the current
// system."
//
// MEASURED, 38 docs indexed, 2026-08-15: the page's "In review &
// discussion" section held 23 docs. ELEVEN of them had zero open
// questions, zero pending decisions and no open review Job. They were
// there because a hand-written `**Status**:` line still said in-review
// or draft.
//
// The old rule (2026-07-08) was "status says so OR anything actionable
// is attached", and it was right for the corpus it was written
// against. What changed is that the corpus grew a long tail of
// finished discussions — because the status line is the one input to
// that rule nobody ever updates. So status drifts to stale by default,
// in one direction, and the section fills with settled work.
//
// So the rule inverts: a doc is asking for something when something is
// OPEN. Status becomes a label rather than a gate — kept visible,
// because it still says what the author intended, just not trusted to
// decide what is on your plate. The cause is filed separately
// (`0b8ae875`); fixing the page is not fixing the mechanism.

export type DesignDoc = Readonly<{
  path: string;
  title: string;
  status: string;
  open_questions: number;
  word_count: number;
  last_modified: string;
}>;

/// Where a doc sits on this page.
///
/// - `needs-you`  something is open: questions, or a review Job in
///                flight.
/// - `draft`      the author is still writing it and has not asked
///                anything yet. Not settled, and filing it as settled
///                would lose it.
/// - `library`    the settled corpus — living references and finished
///                discussions.
export type DesignGroup = 'needs-you' | 'draft' | 'library';

/// Statuses that mean "being written", as opposed to "being decided".
const DRAFTING: readonly string[] = ['draft'];

export function groupOf(doc: DesignDoc, hasOpenReview: boolean): DesignGroup {
  // Anything open outranks anything the frontmatter claims — including
  // for a draft, since a draft that has started asking questions is
  // exactly what this page is for.
  if (doc.open_questions > 0 || hasOpenReview) {
    return 'needs-you';
  }
  if (DRAFTING.includes(doc.status)) return 'draft';
  return 'library';
}

export type Grouped = Readonly<{
  needsYou: readonly DesignDoc[];
  drafts: readonly DesignDoc[];
  library: readonly DesignDoc[];
}>;

/// Split the corpus. `hasOpenReview` is asked per path rather than
/// passed as a set so the caller keeps owning what "in review" means —
/// today an open `design-doc-review` Job, tomorrow whatever the
/// station says.
export function groupDocs(
  docs: readonly DesignDoc[],
  hasOpenReview: (path: string) => boolean,
): Grouped {
  const needsYou: DesignDoc[] = [];
  const drafts: DesignDoc[] = [];
  const library: DesignDoc[] = [];
  for (const d of docs) {
    const g = groupOf(d, hasOpenReview(d.path));
    if (g === 'needs-you') needsYou.push(d);
    else if (g === 'draft') drafts.push(d);
    else library.push(d);
  }
  // Most open questions first inside "needs you": the page is a queue,
  // and the deepest doc is the one worth an hour rather than a minute.
  // Ties break on path so the order is stable across reloads.
  const byWeight = (a: DesignDoc, b: DesignDoc) =>
    b.open_questions - a.open_questions || a.path.localeCompare(b.path);
  return {
    needsYou: [...needsYou].sort(byWeight),
    drafts: [...drafts].sort((a, b) => a.path.localeCompare(b.path)),
    library: [...library].sort((a, b) => a.path.localeCompare(b.path)),
  };
}

/// How many questions are actually waiting. Rendered in the section
/// header, because "9 docs" and "48 questions" are different sizes of
/// afternoon.
export function openWeight(docs: readonly DesignDoc[]): Readonly<{
  questions: number;
}> {
  return {
    questions: docs.reduce((n, d) => n + d.open_questions, 0),
  };
}
