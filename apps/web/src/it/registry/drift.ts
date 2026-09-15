// The protocol drift's read model — a projection of the daily
// `maintenance-protocol-drift` packets, nothing else.
//
// David, 2026-09-11 (8f4e9cc0): "we might need some sort of doc diff
// view for me to approve." Car 1 (19dec171, train #376) is the
// measuring half: infra/protocol-drift.sh runs the gate's own
// comparator on boss-gcp at 05:20 UTC and PATCHes `metadata.measured`
// and `metadata.drift` onto the packet its unit opened. The FIELDS,
// their direction (`unauthored` is what is live that the tree does not
// say, `pending` what the tree says that is not live, `fields` both
// existing and disagreeing) and the 90-character windows are documented
// at the top of that script and in the lint it runs; this module does
// not restate them. This is the reading half (4ae9969e, car 2): one
// tab on the Registry family that renders the newest measurement. The
// approve is car 3, not here.
//
// EVERY VALUE HERE IS A FIELD OF THE PACKET. The page adds nothing the
// record does not hold: a null head stays null with the row's own
// `head_why`; a measurement with no drift block is zero findings, not
// a dropped row; a packet with no measurement (a run the script
// refused, exit 3) is COUNTED and named, never read as "0 adrift". A
// failed read is a failure, never an empty table.

import { fetchRemote, type Remote } from '../../data/remote';

// ---------------------------------------------------------------------
// Types — the packet fields the page reads
// ---------------------------------------------------------------------

export type Measured = Readonly<{
  at: string;
  /** The checkout the bundle was read from; null when git could not
   *  read it, with `head_why` saying so — the comparison still ran. */
  head: string | null;
  head_at: string | null;
  head_why: string | null;
  /** The registry the live rows were read from. */
  target: string | null;
  /** The lint's own verdict: 0 agreement, 1 an unauthored live kind, 2
   *  field drift under --require-live. */
  lint_exit: number | null;
  live_admitted: number;
  authored: number;
  fields_parsed: number;
  fields_compared: number;
  exempt: ReadonlyArray<string>;
}>;

/** One compared field that disagrees: the file in the tree at `head`
 *  against the ACTIVE live row of the kind. */
export type FieldDrift = Readonly<{
  kind: string;
  field: string;
  live_version: number | null;
  /** Offset of the first differing character. */
  at: number | null;
  tree_len: number | null;
  live_len: number | null;
  tree_window: string;
  live_window: string;
}>;

export type AbsentField = Readonly<{ kind: string; field: string }>;

export type Tenant = Readonly<{ file: string; not_admitted: number | null; total: number | null }>;

export type Counts = Readonly<{ unauthored: number; fields: number; absent: number; pending: number }>;

export type Drift = Readonly<{
  counts: Counts;
  /** Live kinds no file authors — what is live that the tree does not say. */
  unauthored: ReadonlyArray<string>;
  fields: ReadonlyArray<FieldDrift>;
  /** Fields the file makes no claim about. */
  absent: ReadonlyArray<AbsentField>;
  /** Bundle kinds with no live row — what the tree says that is not live. */
  pending: ReadonlyArray<string>;
  tenants: ReadonlyArray<Tenant>;
}>;

export type DriftPacket = Readonly<{
  id: string;
  title: string;
  measured: Measured;
  drift: Drift;
}>;

export type DriftPage = Readonly<{
  packets: ReadonlyArray<DriftPacket>;
  /** Packets of the kind that carry no measurement (a refused run). */
  unmeasured: number;
  total: number;
}>;

// ---------------------------------------------------------------------
// Parsing — once, at the fetch call site
// ---------------------------------------------------------------------

const rec = (v: unknown): Record<string, unknown> | null =>
  v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);
const num = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null);
const int = (v: unknown): number => num(v) ?? 0;
const strs = (v: unknown): ReadonlyArray<string> =>
  Array.isArray(v) ? v.flatMap((x) => (typeof x === 'string' ? [x] : [])) : [];

function parseMeasured(v: unknown): Measured | null {
  const r = rec(v);
  const at = r ? str(r.at) : null;
  if (!r || !at) return null;
  return {
    at,
    head: str(r.head),
    head_at: str(r.head_at),
    head_why: str(r.head_why),
    target: str(r.target),
    lint_exit: num(r.lint_exit),
    live_admitted: int(r.live_admitted),
    authored: int(r.authored),
    fields_parsed: int(r.fields_parsed),
    fields_compared: int(r.fields_compared),
    exempt: strs(r.exempt),
  };
}

function parseField(v: unknown): FieldDrift | null {
  const r = rec(v);
  const kind = r ? str(r.kind) : null;
  const field = r ? str(r.field) : null;
  if (!r || !kind || !field) return null;
  return {
    kind,
    field,
    live_version: num(r.live_version),
    at: num(r.at),
    tree_len: num(r.tree_len),
    live_len: num(r.live_len),
    tree_window: str(r.tree_window) ?? '',
    live_window: str(r.live_window) ?? '',
  };
}

function parseAbsent(v: unknown): AbsentField | null {
  const r = rec(v);
  const kind = r ? str(r.kind) : null;
  const field = r ? str(r.field) : null;
  return r && kind && field ? { kind, field } : null;
}

function parseTenant(v: unknown): Tenant | null {
  const r = rec(v);
  const file = r ? str(r.file) : null;
  return r && file ? { file, not_admitted: num(r.not_admitted), total: num(r.total) } : null;
}

const list = <T>(v: unknown, parse: (x: unknown) => T | null): ReadonlyArray<T> =>
  Array.isArray(v) ? v.flatMap((x) => parse(x) ?? []) : [];

/** The `drift` block. Absent entirely (the row predates it, or a run
 *  measured and found the registry in agreement) it is zero findings —
 *  the counts are then derived from the lists, which are empty, so the
 *  two cannot disagree. */
function parseDrift(v: unknown): Drift {
  const r = rec(v) ?? {};
  const fields = list(r.fields, parseField);
  const unauthored = strs(r.unauthored);
  const absent = list(r.absent, parseAbsent);
  const pending = strs(r.pending);
  const c = rec(r.counts);
  return {
    counts: {
      unauthored: c ? (num(c.unauthored) ?? unauthored.length) : unauthored.length,
      fields: c ? (num(c.fields) ?? fields.length) : fields.length,
      absent: c ? (num(c.absent) ?? absent.length) : absent.length,
      pending: c ? (num(c.pending) ?? pending.length) : pending.length,
    },
    unauthored,
    fields,
    absent,
    pending,
    tenants: list(r.tenants, parseTenant),
  };
}

/** The jobs-API page for the kind: `{data, total}` or a bare array. */
export function parseDriftPackets(raw: unknown): DriftPage {
  const env = rec(raw);
  const items: unknown[] = Array.isArray(raw) ? raw : env && Array.isArray(env.data) ? env.data : [];
  const total = env ? (num(env.total) ?? items.length) : items.length;
  const packets = items.flatMap((j): DriftPacket[] => {
    const r = rec(j);
    const id = r ? str(r.id) : null;
    const meta = r ? rec(r.metadata) : null;
    const measured = meta ? parseMeasured(meta.measured) : null;
    if (!r || !id || !measured) return [];
    return [{ id, title: str(r.title) ?? id, measured, drift: parseDrift(meta?.drift) }];
  });
  return { packets, unmeasured: items.length - packets.length, total };
}

/** The whole kind, newest first. One packet a day, so `limit` covers
 *  `limit` days; the page reports `total` past what it read. */
export function loadDriftPackets(limit: number): Promise<Exclude<Remote<DriftPage>, { kind: 'loading' }>> {
  return fetchRemote(`/api/jobs?kind=maintenance-protocol-drift&limit=${limit}`, parseDriftPackets);
}

/** The packet measured last — by `measured.at`, not by API order. */
export function newestMeasured(packets: ReadonlyArray<DriftPacket>): DriftPacket | null {
  return packets.reduce<DriftPacket | null>((best, p) => (best === null || p.measured.at > best.measured.at ? p : best), null);
}
