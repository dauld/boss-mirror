// The decision-context resolution chain — the data half of "a step
// must present the packet's case to whoever acts on it" (19db52de).
//
// David, 2026-08-18, on his publish sign-off: "There is just a sign
// and complete button, which doesn't seem like much of a choice." And
// generalising: "the current state of the packet isn't present in a
// useful manner to me as the decision-maker." The evidence always
// existed — in another step's metadata, or the job body — but the
// surface holding the button never showed it.
//
// The chain mirrors the review plugin's both-bags fallback, which this
// codebase already learned the hard way (a reviewer answered four
// questions blind because the prose sat in the OTHER metadata bag):
//
//   1. step.metadata.context_md   — the author addressed THIS step;
//   2. job.metadata.context_md    — the packet-level briefing;
//   3. job.metadata.message       — the filed text itself (every
//      user-feedback packet's case lives here, so all of them become
//      self-presenting without a single data write).
//
// Pure so it is testable without a DOM; the component owns the fetch.

export type DecisionContextSource =
  | 'step'
  | 'job-context'
  | 'job-message'
  | 'job-body'
  | 'prior-steps';

export type DecisionContext = {
  text: string;
  source: DecisionContextSource;
};

function nonEmptyString(v: unknown): string | null {
  return typeof v === 'string' && v.trim().length > 0 ? v : null;
}

export function contextFromStep(
  stepMetadata: Record<string, unknown>,
): DecisionContext | null {
  const text = nonEmptyString(stepMetadata['context_md']);
  return text ? { text, source: 'step' } : null;
}

export function contextFromJob(
  jobMetadata: Record<string, unknown>,
): DecisionContext | null {
  const ctx = nonEmptyString(jobMetadata['context_md']);
  if (ctx) return { text: ctx, source: 'job-context' };
  const msg = nonEmptyString(jobMetadata['message']);
  if (msg) return { text: msg, source: 'job-message' };
  // `body` is where a backlog-item states its case, the way a
  // user-feedback packet uses `message`. Missing it meant EVERY
  // backlog-item reached its decision step with an empty panel —
  // 4e0e42b2 carries 5,622 characters of case in `body` and showed
  // none of it. One kind's field name was covered and the other's
  // was not, which is why the gap survived being fixed once.
  const body = nonEmptyString(jobMetadata['body']);
  if (body) return { text: body, source: 'job-body' };
  return null;
}

/// The fourth source: what the packet's earlier steps recorded.
///
/// The three sources above cover a packet whose case is stated ONCE —
/// a user-feedback packet's `message`, or a briefing someone wrote by
/// hand. They do not cover a packet whose case ACCUMULATES, which is
/// what a multi-step protocol produces: a protocol-retro's findings sit
/// in `collect`, `analyze`, `gaps` and `report`; a rotate-a-credential's
/// case sits in `scope`. Both reached a human decision with the panel
/// empty on 2026-08-28, and both were described the same way David
/// described the original defect — the job was unworkable.
///
/// That is the same failure this file was written for, one shape along.
/// Its own header records the review plugin learning it once already:
/// "a reviewer answered four questions blind because the prose sat in
/// the OTHER metadata bag."
///
/// Gathers long-form prose from steps that are DONE, in order, labelled
/// by the step that recorded it. Short values are skipped: a receipt, a
/// disposition or a sha is a field, not a case, and pasting them under a
/// decision would bury the prose that matters.
export type PriorStep = {
  title?: string;
  status?: string;
  metadata?: Record<string, unknown> | null;
};

/// Values shorter than this are fields, not prose.
const PROSE_MIN = 120;

/// Keys that are never the case for a decision, however long they run.
const NOT_PROSE = new Set(['context_md', 'receipt', 'pr_url', 'branch', 'sha']);

export function contextFromPriorSteps(steps: PriorStep[]): DecisionContext | null {
  const sections: string[] = [];
  for (const step of steps) {
    if (step.status !== 'completed') continue;
    const md = step.metadata ?? {};
    const parts: string[] = [];
    for (const [key, value] of Object.entries(md)) {
      if (NOT_PROSE.has(key)) continue;
      const text = nonEmptyString(value);
      if (!text || text.length < PROSE_MIN) continue;
      parts.push(`**${key}**\n\n${text}`);
    }
    if (parts.length) {
      sections.push(`### ${step.title ?? 'Earlier step'}\n\n${parts.join('\n\n')}`);
    }
  }
  if (!sections.length) return null;
  return { text: sections.join('\n\n'), source: 'prior-steps' };
}
