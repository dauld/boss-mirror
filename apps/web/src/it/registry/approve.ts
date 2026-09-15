// The Drift tab's approve — the publish behind an approve (car 3b of
// 8f4e9cc0; David, 2026-09-11: "we might need some sort of doc diff
// view for me to approve").
//
// Car 1 measures and files the drift; car 2 renders it; car 3a is the
// ops verb `publish-workflow` (infra/ops/verbs/publish-workflow.json →
// infra/gcp/publish-workflow.sh on boss-gcp, the one host with both the
// converged tree and the registry). This car is the control: each
// adrift kind gets one, and pressing it FILES AN OPS-REQUEST PACKET —
// the same packet `boss ops boss-gcp publish-workflow <kind>` files
// from a terminal (crates/orchestrators/boss-cli/src/ops_request.rs:
// kind ops-request, subject custom/<host>, metadata host/verb/args, the
// verb's title), through POST /api/jobs with the viewer as the actor.
// No new endpoint: the runner on boss-gcp polls for its host's open
// packets, runs the verb with its own refusals in front of the publish,
// and writes exit code + output onto the `execute` step. The tab then
// shows THAT — the packet link and its answer — so the approver sees
// the effect, not the click.
//
// WHICH CONTROL A KIND GETS is decided by the verb's own last answer
// for that kind, never by a reading of the excerpt. The drift packet
// says the file and the live row disagree; it does NOT say whether the
// live row is one the tree once said (then the tree moved ahead —
// publish) or an operator's edit made live that no revision of the
// tree ever rendered (then publishing ERASES it). That judgement is
// the verb's history walk on the host (publish-workflow.sh, exit 6),
// the only machine-run one there is, and this surface does not guess
// at it: the first approve files the plain request; a refusal-6 comes
// back naming the drift field by field; only then does the tab offer
// `--force-tree`, behind a second confirmation that names the field
// that will be erased. Measured 2026-09-15: ship-a-change's live v31
// description carries "OPERATOR EDIT 2026-08-27, deliberate and
// load-bearing" that the tree never said — the case this guards.
//
// AUTHORITY is the role the ops-request workflow's `execute` step
// names (infra/platform/workflows/ops-request.toml, `authority_role`),
// read off the live row rather than restated here; a viewer without it
// sees the control disabled with the role named — abortAuthority's
// affordance (jobs/abort.ts). The affordance is not the gate: the jobs
// API and policy still decide the POST.

import { fetchRemote, type Remote } from '../../data/remote';
import type { FieldDrift } from './drift';

export const PUBLISH_VERB = 'publish-workflow';
/** The one host the verb serves (infra/ops/verbs/publish-workflow.json). */
export const PUBLISH_HOST = 'boss-gcp';
/** The marker every request this surface files carries — provenance
 *  for the record and the probe, not a filter on what the tab shows. */
export const REQUESTED_FROM = 'drift-tab';
/** The workflow kind whose `execute` step names the approver's role. */
export const OPS_REQUEST_KIND = 'ops-request';

// ---------------------------------------------------------------------
// Kinds — one control per adrift kind, its fields grouped
// ---------------------------------------------------------------------

export type AdriftKind = Readonly<{
  kind: string;
  fields: ReadonlyArray<FieldDrift>;
  /** The active live version the rows were compared against. */
  live_version: number | null;
}>;

/** The packet's per-field rows, grouped by kind in first-seen order. */
export function adriftKinds(fields: ReadonlyArray<FieldDrift>): ReadonlyArray<AdriftKind> {
  return fields.reduce<ReadonlyArray<AdriftKind>>((acc, f) => {
    const i = acc.findIndex((k) => k.kind === f.kind);
    if (i === -1) return [...acc, { kind: f.kind, fields: [f], live_version: f.live_version }];
    const k = acc[i]!;
    return acc.map((x, j) => (j === i ? { ...k, fields: [...k.fields, f], live_version: k.live_version ?? f.live_version } : x));
  }, []);
}

// ---------------------------------------------------------------------
// Requests — publish-workflow ops-requests, as the runner leaves them
// ---------------------------------------------------------------------

/** The verb's second arg: none, `--check` (every refusal, no write —
 *  what a terminal rehearsal files), or `--force-tree`. */
export type PublishMode = 'plain' | 'check' | 'force';

export type PublishRequest = Readonly<{
  id: string;
  title: string;
  workflow_kind: string;
  mode: PublishMode;
  status: 'open' | 'closed';
  opened_at: string | null;
  /** Off the `execute` step: null while the host has not answered. */
  disposition: 'answered' | 'refused' | null;
  exit_code: string | null;
  output: string;
  runner_host: string | null;
  requested_from: string | null;
  /** The instant the answer landed: the `execute` step's server-stamped
   *  `completed_at`; null while open or on a step that predates it. */
  answered_at: string | null;
}>;

const rec = (v: unknown): Record<string, unknown> | null =>
  v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);
const strs = (v: unknown): ReadonlyArray<string> =>
  Array.isArray(v) ? v.flatMap((x) => (typeof x === 'string' ? [x] : [])) : [];

function parseRequest(v: unknown): PublishRequest | null {
  const r = rec(v);
  const id = r ? str(r.id) : null;
  const meta = r ? rec(r.metadata) : null;
  if (!r || !id || !meta || str(meta.verb) !== PUBLISH_VERB) return null;
  const args = strs(meta.args);
  const workflow_kind = args[0] ?? null;
  if (!workflow_kind) return null;
  const execute = (Array.isArray(r.steps) ? r.steps : [])
    .map(rec)
    .find((s) => s !== null && str(s.spec_slug) === 'execute');
  const em = execute ? (rec(execute.metadata) ?? {}) : {};
  const d = str(em.disposition);
  return {
    id,
    title: str(r.title) ?? id,
    workflow_kind,
    mode: args[1] === '--force-tree' ? 'force' : args[1] === '--check' ? 'check' : 'plain',
    status: str(r.status) === 'open' ? 'open' : 'closed',
    opened_at: str(meta.opened_at),
    disposition: d === 'answered' || d === 'refused' ? d : null,
    exit_code: str(em.exit_code),
    output: typeof em.output === 'string' ? em.output : '',
    runner_host: str(em.runner_host),
    requested_from: str(meta.requested_from),
    answered_at: execute ? str(execute.completed_at) : null,
  };
}

/** The jobs-API page for kind=ops-request, reduced to this verb's
 *  requests. `{data, total}` or a bare array; a malformed row is
 *  dropped, never thrown on. */
export function parsePublishRequests(raw: unknown): ReadonlyArray<PublishRequest> {
  const env = rec(raw);
  const items: unknown[] = Array.isArray(raw) ? raw : env && Array.isArray(env.data) ? env.data : [];
  return items.flatMap((j) => parseRequest(j) ?? []);
}

/** The query, spelled once for the page and the probe: `metadata`
 *  containment (`metadata @> {"verb": …}`, http/jobs.rs) — this verb's
 *  requests, not a page of every verb's that may or may not hold them
 *  (ops-requests run at ~40 a day from the disk sweep alone). */
export const PUBLISH_REQUESTS_QUERY = `/api/jobs?kind=ops-request&metadata=${encodeURIComponent(JSON.stringify({ verb: PUBLISH_VERB }))}`;

export function loadPublishRequests(limit = 40): Promise<Exclude<Remote<ReadonlyArray<PublishRequest>>, { kind: 'loading' }>> {
  return fetchRemote(`${PUBLISH_REQUESTS_QUERY}&limit=${limit}`, parsePublishRequests);
}

/** This kind's latest request — by `opened_at`, not API order; an
 *  unstamped one sorts oldest. */
export function latestFor(requests: ReadonlyArray<PublishRequest>, kind: string): PublishRequest | null {
  return requests
    .filter((r) => r.workflow_kind === kind)
    .reduce<PublishRequest | null>((best, r) => (best === null || (r.opened_at ?? '') > (best.opened_at ?? '') ? r : best), null);
}

// ---------------------------------------------------------------------
// Mode — which control the kind gets
// ---------------------------------------------------------------------

/** publish-workflow.sh's exit for "the live row carries what the tree
 *  never said" — the one refusal `--force-tree` overrides. */
export const EXIT_TREE_NEVER_SAID = '6';

export type ApproveMode =
  /** File `publish-workflow <kind>`: the verb's own refusals decide. */
  | { kind: 'plain' }
  /** A request is in flight; a second would be a twin. */
  | { kind: 'pending'; request_id: string }
  /** The verb refused with exit 6: offer `--force-tree` behind the
   *  confirmation that names the field it erases. */
  | { kind: 'force'; request_id: string };

export function modeFor(latest: PublishRequest | null): ApproveMode {
  if (latest === null) return { kind: 'plain' };
  if (latest.status === 'open') return { kind: 'pending', request_id: latest.id };
  if (latest.disposition === 'answered' && latest.exit_code === EXIT_TREE_NEVER_SAID) {
    return { kind: 'force', request_id: latest.id };
  }
  return { kind: 'plain' };
}

/** The second confirmation: the typed text must be the field name(s)
 *  that will be erased, exactly — several joined by a comma and a space
 *  — so what the approver wrote is what the record says was erased. */
export function forceConfirmed(typed: string, fields: ReadonlyArray<string>): boolean {
  return fields.length > 0 && typed.trim() === fields.join(', ');
}

// ---------------------------------------------------------------------
// Superseded — a row the verb has already answered since it was measured
// ---------------------------------------------------------------------
//
// Car 3c (8f4e9cc0 `follow_up_3c`). The rows are the 05:20 packet's and
// stay "adrift" until the next run even after a publish answered exit 0
// beside them; the block showed the answer but nothing marked the row.
// A row reads superseded when the kind's LATEST request WROTE the
// tree's row after the measurement was read: exit 0, answered, closed,
// not a --check (which answers 0 having written nothing, step 5 of
// publish-workflow.sh), and answered after `measured.at`. Anything
// newer than a success — a refusal, exit 78, a request still in flight
// — is the newer answer and un-greys the row. The instant is the
// execute step's `completed_at` (opened_at when a row predates the
// stamp); unstamped both ways the row cannot be judged and stays what
// the packet said. The versions are read off the verb's own
// confirmation line (`<kind> vN -> vM live at`), never computed here.

export type RowState =
  /** As the packet measured it. */
  | { kind: 'adrift' }
  | {
      kind: 'superseded';
      request_id: string;
      /** When the publish answered. */
      at: string;
      /** The live version the verb published over; the measured one
       *  when the output does not name it. */
      from: number | null;
      /** The version the verb confirmed live, or null when unnamed. */
      to: number | null;
    };

const CONFIRMED_LINE = /\bv(\d+) -> v(\d+) live at\b/;
const instant = (iso: string | null): number | null => {
  const t = iso === null ? NaN : Date.parse(iso);
  return Number.isNaN(t) ? null : t;
};

export function rowState(row: AdriftKind, latest: PublishRequest | null, measuredAt: string): RowState {
  if (latest === null || latest.status !== 'closed' || latest.disposition !== 'answered') return { kind: 'adrift' };
  if (latest.exit_code !== '0' || latest.mode === 'check') return { kind: 'adrift' };
  const at = latest.answered_at ?? latest.opened_at;
  const answered = instant(at);
  const measured = instant(measuredAt);
  if (at === null || answered === null || measured === null || answered <= measured) return { kind: 'adrift' };
  const m = CONFIRMED_LINE.exec(latest.output);
  return {
    kind: 'superseded',
    request_id: latest.id,
    at,
    from: m ? Number(m[1]) : row.live_version,
    to: m ? Number(m[2]) : null,
  };
}

/** What the header's adrift count subtracts: the packet counts FIELDS,
 *  so a superseded kind takes all of its fields with it. */
export function supersededFieldCount(kinds: ReadonlyArray<AdriftKind>, states: ReadonlyMap<string, RowState>): number {
  return kinds.reduce((n, k) => (states.get(k.kind)?.kind === 'superseded' ? n + k.fields.length : n), 0);
}

// ---------------------------------------------------------------------
// Body — the packet, as `boss ops` files it
// ---------------------------------------------------------------------

export type ApproveAgainst = Readonly<{
  /** The maintenance-protocol-drift packet the approver read. */
  packet: string;
  /** The tree head that packet measured, or null when it recorded none. */
  head: string | null;
}>;

export type ApproveBody = Readonly<{
  kind: 'ops-request';
  title: string;
  tags: ReadonlyArray<never>;
  subject: Readonly<{ id: string; subject_kind: 'custom' }>;
  owner_id: string;
  status: 'open';
  priority: 'standard';
  metadata: Readonly<Record<string, unknown>>;
}>;

/** `crate::job::envelope("ops-request", title, None, Some(host), owner,
 *  today, {host, verb, args})` with the title `validate` gives it
 *  (`<verb> <args…> on <host>`) — minus `opened_on`, so the server
 *  clocks the packet and stamps `metadata.opened_at` (http/jobs.rs),
 *  the instant the probe compares against; plus the provenance this
 *  surface can add: which drift packet at which head was approved
 *  against, which fields, and the marker. */
export function approveBody(kind: AdriftKind, mode: 'plain' | 'force', against: ApproveAgainst, ownerId: string): ApproveBody {
  const args = mode === 'force' ? [kind.kind, '--force-tree'] : [kind.kind];
  const fields = kind.fields.map((f) => f.field);
  return {
    kind: 'ops-request',
    title: `${PUBLISH_VERB} ${args.join(' ')} on ${PUBLISH_HOST}`,
    tags: [],
    subject: { id: PUBLISH_HOST, subject_kind: 'custom' },
    owner_id: ownerId,
    status: 'open',
    priority: 'standard',
    metadata: {
      host: PUBLISH_HOST,
      verb: PUBLISH_VERB,
      args,
      requested_from: REQUESTED_FROM,
      drift_packet: against.packet,
      drift_head: against.head,
      drift_fields: fields,
      live_version: kind.live_version,
      ...(mode === 'force' ? { force_erases: fields } : {}),
    },
  };
}

/** POST the packet; the created id, or a thrown error naming the
 *  status and body — the caller renders it, never swallows it. */
export async function fileApprove(body: ApproveBody): Promise<string> {
  const r = await fetch('/api/jobs', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!r.ok) throw new Error(`file ops-request: HTTP ${r.status}: ${await r.text()}`);
  const created = rec(await r.json());
  const id = created ? str(created.id) : null;
  if (!id) throw new Error('file ops-request: the create returned no id — refusing to call that filed');
  return id;
}

// ---------------------------------------------------------------------
// Authority — the execute step's role, off the live workflow row
// ---------------------------------------------------------------------

/** `GET /api/workflows/ops-request` → the `execute` step's
 *  `authority_role`, or null when the row names none. */
export function executeAuthorityRole(raw: unknown): string | null {
  const r = rec(raw);
  const steps = r && Array.isArray(r.steps) ? r.steps : [];
  const execute = steps.map(rec).find((s) => s !== null && str(s.title) === 'execute');
  return execute ? str(execute.authority_role) : null;
}

export function loadExecuteAuthorityRole(): Promise<Exclude<Remote<string | null>, { kind: 'loading' }>> {
  return fetchRemote(`/api/workflows/${OPS_REQUEST_KIND}`, executeAuthorityRole);
}

export type ApproveAuthority = { kind: 'admitted' } | { kind: 'refused'; role: string | null; why: string };

/** abortAuthority's shape: whoever the step's role admits — the
 *  ordinary claim rule, no separate approve permission. */
export function approveAuthority(authorityRole: string | null, viewerRole: string | null): ApproveAuthority {
  if (viewerRole === null) {
    return { kind: 'refused', role: authorityRole, why: 'Sign in to approve a publish.' };
  }
  if (authorityRole === null || authorityRole === viewerRole) return { kind: 'admitted' };
  return { kind: 'refused', role: authorityRole, why: `Only ${authorityRole} may approve a publish.` };
}
