// A failed verb answer on a step — the one reader for the note
// jobs.complete_linked_step (v6, backlog f47861a5) writes when the
// ops verb a machine step waits on FAILED: the verb's last FAILED
// line lands on the still-open step as `failed`, with the exit
// (`failed_exit`), the request that ran it (`failed_source`) and the
// urgent backlog-item it filed (`alert`). The step stays `ready`,
// because it is — the work is not done — which is exactly why the
// surfaces must read this: measured 2026-09-19 (backlog 074e1287),
// publish 254177e2's open-pr sat annotated and the step surface and
// the receiving yard drew it like any ready step. CLAUDE.md
// §Diagnosis: a troubled packet must look troubled.
//
// Read here once so the step surface and the yard say the same thing
// for the same note (§9a). The keys mirror the handler's PATCH body
// (jobs_complete_linked_step.rs::annotate_and_alert) and are pinned
// on that side by tests/publish_pr_answer.rs.

export type FailedVerb = Readonly<{
  /** The verb's last FAILED line, verbatim — never paraphrased. */
  line: string;
  /** The exit the runner recorded, as its digits; null when unrecorded. */
  exit: string | null;
  /** The ops-request whose verb failed; null when unrecorded. */
  source: string | null;
  /** The urgent backlog-item filed for it; null when none was. */
  alert: string | null;
}>;

const text = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);

/** The failure a step's metadata carries, or null when it carries
 *  none. Only the line decides — a step with `failed` set is a step
 *  whose verb failed; the other three ride beside it when recorded. */
export function failedVerb(metadata: unknown): FailedVerb | null {
  const md = (metadata ?? {}) as Record<string, unknown>;
  const line = text(md.failed);
  if (line === null) return null;
  const rawExit = md.failed_exit;
  const exit =
    typeof rawExit === 'number' && Number.isFinite(rawExit) ? String(rawExit) : text(rawExit);
  return { line, exit, source: text(md.failed_source), alert: text(md.alert) };
}

/** The one sentence both surfaces print: the exit when one was
 *  recorded, then the line as the verb wrote it. */
export function failedVerbPhrase(f: FailedVerb): string {
  return `FAILED${f.exit === null ? '' : ` (exit ${f.exit})`} · ${f.line}`;
}
