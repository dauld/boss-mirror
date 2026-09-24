// Semi-structured rendering for incident packets — the post-mortem an
// incident becomes once it closes. Written for incident-post-mortem,
// folded into `incident` with it on 2026-09-24 (backlog 59d15039).
//
// A post-mortem's findings live in free-form Job metadata, and the
// shape has already drifted between the live packets (incident_at /
// summary / mitigations_shipped / open_questions / evidence on the
// newest; ask / declared_by / incident_date / outcome on the oldest).
// Hardcoding the newest shape would silently drop the older packets'
// content, and dumping raw JSON is the unusable rendering the two
// feedback packets complained about. So the contract is
// SEMI-structured:
//
//   * keys the platform knows get first-class sections in a fixed
//     reading order — when, summary, timeline, root cause, the
//     mitigations* family, open questions, evidence;
//   * every other key renders after them as a labeled prose block, in
//     the order the author wrote them;
//   * nothing is dropped, and nothing renders as raw JSON — non-string
//     values flatten to lines.
//
// A near-copy of `sectionsFor` lives in
// infra/step-plugins/incident-review.js: plugins are standalone JS
// bundles by design (no imports from the SPA), so the ordering is
// deliberately duplicated there. Change one, change both.

import type { Step } from '../../jobs/types';

export type DocSection = Readonly<{ key: string; label: string; body: string }>;

/// Curated labels for the keys the platform knows. Both spellings of
/// "when" map to the same label — the packets drifted before the
/// surface existed.
const KNOWN_LABELS: Readonly<Record<string, string>> = {
  incident_at: 'When it happened',
  incident_date: 'When it happened',
  summary: 'Summary',
  timeline: 'Timeline',
  root_cause: 'Root cause',
  open_questions: 'Open questions',
  evidence: 'Evidence',
};

/// The reading order. `#mitigations` is the slot the whole
/// `mitigations*` key family occupies — the family is matched by
/// prefix so `mitigations_shipped`, `mitigation_next`, etc. are all
/// first-class without a registry of every spelling.
const READING_ORDER: ReadonlyArray<string> = [
  'incident_at',
  'incident_date',
  'summary',
  'timeline',
  'root_cause',
  '#mitigations',
  'open_questions',
  'evidence',
];

/// Sentence-case a metadata key: `declared_by` → "Declared by".
export function humanizeKey(key: string): string {
  const spaced = key.replace(/[_-]+/g, ' ').trim();
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

/// Flatten a metadata value to prose. Strings pass through; scalars
/// stringify; arrays become one line per item; flat objects become
/// labeled lines. Only nested leaves fall back to compact JSON —
/// content is never dropped. `null` means "nothing to render".
function prose(value: unknown): string | null {
  if (value === null || value === undefined) return null;
  if (typeof value === 'string') return value.trim() === '' ? null : value;
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  if (Array.isArray(value)) {
    const lines = value.map((item) =>
      typeof item === 'string' ? item : JSON.stringify(item),
    );
    return lines.length > 0 ? lines.join('\n') : null;
  }
  if (typeof value === 'object') {
    const entries = Object.entries(value as Record<string, unknown>);
    if (entries.length === 0) return null;
    return entries
      .map(([k, v]) => `${humanizeKey(k)}: ${typeof v === 'string' ? v : JSON.stringify(v)}`)
      .join('\n');
  }
  return String(value);
}

/// Order a packet's metadata into renderable sections: well-known keys
/// first in reading order, everything else after them as labeled prose
/// in authored order. `omit` keeps keys the caller renders elsewhere
/// (an outcome badge, a header date) out of the document body.
export function postMortemSections(
  metadata: Record<string, unknown>,
  omit: ReadonlyArray<string> = [],
): ReadonlyArray<DocSection> {
  const skip = new Set(omit);
  const mitigationsSlot = READING_ORDER.indexOf('#mitigations');
  const rank = (key: string): number => {
    const slot = READING_ORDER.indexOf(key);
    if (slot >= 0) return slot;
    if (key.startsWith('mitigation')) return mitigationsSlot;
    return READING_ORDER.length;
  };
  return Object.keys(metadata)
    .filter((key) => !skip.has(key))
    .map((key, authored) => ({ key, authored }))
    // Sort is stable on rank ties via the authored index, so unknown
    // keys (and the mitigations family) keep the author's order.
    .sort((a, b) => rank(a.key) - rank(b.key) || a.authored - b.authored)
    .flatMap(({ key }) => {
      const body = prose(metadata[key]);
      return body === null
        ? []
        : [{ key, label: KNOWN_LABELS[key] ?? humanizeKey(key), body }];
    });
}

/// When the incident happened, whichever spelling the packet carries.
export function incidentAt(metadata: Record<string, unknown>): string | null {
  const v = metadata['incident_at'] ?? metadata['incident_date'];
  return typeof v === 'string' && v.trim() !== '' ? v : null;
}

/// How a closed packet ended: the title of the last completed
/// transition — the terminal that actually fired — falling back to a
/// `metadata.outcome` string for packets fetched without steps.
/// Property-based on purpose: dispatching on a step's kind name is
/// what infra/lint/no-step-kind-match.sh exists to stop.
export function closedOutcome(
  job: Readonly<{ metadata: Record<string, unknown>; steps?: ReadonlyArray<Step> }>,
): string | null {
  const lastCompleted = [...(job.steps ?? [])]
    .filter((s) => s.status === 'completed')
    .sort((a, b) => a.sort_order - b.sort_order)
    .at(-1);
  if (lastCompleted) return lastCompleted.title;
  const m = job.metadata['outcome'];
  return typeof m === 'string' && m.trim() !== '' ? m : null;
}
