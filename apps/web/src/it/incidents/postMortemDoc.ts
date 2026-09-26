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

import type { Job, Step } from '../../jobs/types';

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

// ---------------------------------------------------------------------
// The active card's facts (backlog 1a242883, gap 2 of page-audit
// 9b9849f5). The card showed a title, a `when` that was blank on every
// live packet, no severity, no age, and "(unassigned)" for the six of
// the incident protocol's ten steps that a ROLE holds. CLAUDE.md
// §Diagnosis: a troubled packet must look troubled.
// ---------------------------------------------------------------------

/// A Job as the list read sends it: `opened_at` is the server's
/// admission instant (backlog 6c2eba00), on the wire and not yet on the
/// shared `Job` type.
export type IncidentJob = Job & { opened_at?: string | null };

const text = (v: unknown): string | null =>
  typeof v === 'string' && v.trim() !== '' ? v : null;

/// The severity the raise recorded (`metadata.severity`), or null — the
/// card shows it beside `priority`, never an invented level.
export function severityOf(job: IncidentJob): string | null {
  return text(job.metadata['severity']);
}

/// When the incident started, in the order the record can answer it:
/// `started_at` (job metadata, else the first step carrying it — the
/// `raised` step declares it as a field), the older packets'
/// incident_at / incident_date, then the opening. Measured on a65ba21e
/// (2026-09-26): no started_at anywhere, so without the opening
/// fall-backs the card's When stays blank on real packets.
export function startedAt(job: IncidentJob): string | null {
  const stepStart = [...(job.steps ?? [])]
    .sort((a, b) => a.sort_order - b.sort_order)
    .map((s) => text(s.metadata?.['started_at']))
    .find((v) => v !== null);
  return (
    text(job.metadata['started_at']) ??
    stepStart ??
    incidentAt(job.metadata) ??
    text(job.opened_at) ??
    text(job.metadata['opened_at']) ??
    text(job.opened_on)
  );
}

/// The instant "time open" counts from: the server stamp, the metadata
/// convention, then `opened_on` at midnight UTC. An unparseable stamp
/// falls through rather than answering NaN.
export function openedAtMs(job: IncidentJob): number | null {
  const candidates = [
    text(job.opened_at),
    text(job.metadata['opened_at']),
    text(job.opened_on) === null ? null : `${job.opened_on}T00:00:00Z`,
  ];
  const ms = candidates
    .map((c) => (c === null ? NaN : Date.parse(c)))
    .find((n) => !Number.isNaN(n));
  return ms ?? null;
}

/// A span as the card reads it: `<1m`, `12m`, `5h 12m`, `3d 4h`.
export function durationText(ms: number): string {
  const min = Math.floor(Math.max(0, ms) / 60_000);
  if (min < 1) return '<1m';
  if (min < 60) return `${min}m`;
  const h = Math.floor(min / 60);
  if (h < 24) return `${h}h ${min % 60}m`;
  return `${Math.floor(h / 24)}d ${h % 24}h`;
}

/// Who a step is waiting on: the assignee, else the role, station or
/// department its audience names (the projected `authority_role` /
/// `station` keys, and the `audience` block itself for a department,
/// which projects no key yet — f5ebd2e1). "unassigned" only when the
/// step declares none of them.
export function holderOf(step: Step): string {
  if (step.assignee_id) return step.assignee_id;
  const m = step.metadata ?? {};
  const role = text(m['authority_role']);
  if (role) return `role ${role}`;
  const station = text(m['station']);
  if (station) return `station ${station}`;
  const audience = m['audience'];
  const dept =
    typeof audience === 'object' && audience !== null
      ? text((audience as Record<string, unknown>)['department'])
      : null;
  return dept ? `department ${dept}` : 'unassigned';
}

/// How long a step has sat at ready/active, from the queue-age lens
/// (`GET /api/jobs/queue-age`, 2a0b034e) — the projection's
/// `became_ready_at`, which the Job read does not carry. `exact: false`
/// means the stamp is an `updated_at` fallback: a LOWER bound.
export type StepWait = Readonly<{ sinceMs: number; exact: boolean }>;
export type StepWaits = Readonly<{ now: number | null; byStep: ReadonlyMap<string, StepWait> }>;

/// Parse the lens's body. A body without a `data` list is a throw, so a
/// wrong-shaped answer renders as unreadable, never as "no waits".
export function parseStepWaits(raw: unknown): StepWaits {
  const body = raw as { data?: unknown; now?: unknown } | null;
  if (typeof body !== 'object' || body === null || !Array.isArray(body.data)) {
    throw new Error('queue-age: the answer carries no data list');
  }
  const byStep = new Map<string, StepWait>(
    (body.data as ReadonlyArray<Record<string, unknown>>).flatMap((r) => {
      const id = text(r['step_id']);
      const sinceMs = Date.parse(text(r['since']) ?? '');
      return id === null || Number.isNaN(sinceMs)
        ? []
        : [[id, { sinceMs, exact: r['exact'] === true }] as const];
    }),
  );
  const now = Date.parse(text(body.now) ?? '');
  return { now: Number.isNaN(now) ? null : now, byStep };
}
