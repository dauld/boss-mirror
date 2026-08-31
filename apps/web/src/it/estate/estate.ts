// The estate page's data — the hardware registry rendered instead of
// prose (59ef456a; on 2026-08-30 three separate hand-written accounts
// of the machines were wrong in the same direction, because none was
// connected to the machines — this lens is).
//
// Three reads, all guest-safe:
//   GET /api/estate/nodes         — what we MEANT to have (declared)
//   GET /api/estate/observations  — what a look FOUND (events)
//   GET /api/estate/comparisons   — the difference, computed on event
//
// Every fetch lands in a Remote<T>: this page's whole subject is
// absence lying, so an outage must render as failure, never as an
// empty estate (the false-empty family).

import { fetchRemote, type Remote } from '../../data/remote';

export type EstateNode = Readonly<{
  id: string;
  label: string;
  address: string | null;
  role: string;
  cpu: number | null;
  memory_gb: number | null;
  disk_gb: number | null;
  notes: string | null;
  retired: boolean;
}>;

export type ObservedNode = Readonly<{
  id: string;
  address?: string | null;
  cpu?: number | null;
  memory_gb?: number | null;
  disk_gb?: number | null;
  disk_free_gb?: number | null;
  ready?: boolean;
}>;

export type Observation = Readonly<{
  observed_at: string;
  observer: string;
  scope: string;
  nodes: readonly ObservedNode[];
}>;

export type ComparisonCounts = Readonly<{
  observed: number;
  participating_declared: number;
  observed_not_declared: number;
  declared_not_observed: number;
  drift: number;
}>;

export type Comparison = Readonly<{
  observed_at: string;
  scope: string;
  counts: ComparisonCounts;
}>;

export type EstateState = Readonly<{
  nodes: Remote<readonly EstateNode[]>;
  observations: Remote<readonly Observation[]>;
  comparisons: Remote<readonly Comparison[]>;
}>;

// The dev session door. HARDCODED FALLBACK, and loudly so: the estate
// registry DECLARES this door (service_instances row `boss-dev-ssh`,
// port 22, migration 202608310030) but the jobs API serves only
// /api/estate/nodes|observations|comparisons today — there is no
// service-instances read endpoint yet. When one lands, this constant
// dies and the launch block renders from the registry like everything
// else on the page. Tracked on the estate reader item d471a8ce.
export const DEV_SSH_URL = 'ssh://root@10.20.0.35';
export const DEV_SSH_LABEL = 'root@10.20.0.35';

function asArray(raw: unknown): readonly unknown[] {
  if (Array.isArray(raw)) return raw;
  if (raw && typeof raw === 'object' && Array.isArray((raw as { data?: unknown }).data)) {
    return (raw as { data: unknown[] }).data;
  }
  throw new Error('expected an array or {data: [...]}');
}

export function parseNodes(raw: unknown): readonly EstateNode[] {
  return asArray(raw).map((r) => {
    const o = r as Record<string, unknown>;
    if (typeof o.id !== 'string' || typeof o.role !== 'string') {
      throw new Error('estate node row missing id/role');
    }
    return {
      id: o.id,
      label: typeof o.label === 'string' ? o.label : o.id,
      address: typeof o.address === 'string' ? o.address : null,
      role: o.role,
      cpu: typeof o.cpu === 'number' ? o.cpu : null,
      memory_gb: typeof o.memory_gb === 'number' ? o.memory_gb : null,
      disk_gb: typeof o.disk_gb === 'number' ? o.disk_gb : null,
      notes: typeof o.notes === 'string' ? o.notes : null,
      retired: o.retired === true,
    };
  });
}

/** Event rows arrive as {payload: {...}} envelopes from the reader;
 *  the payload is the observation the observer POSTed, verbatim. */
export function parseObservations(raw: unknown): readonly Observation[] {
  return asArray(raw).flatMap((r) => {
    const p = (r as { payload?: unknown }).payload as Record<string, unknown> | undefined;
    if (!p || typeof p.scope !== 'string' || typeof p.observed_at !== 'string') return [];
    const nodes = Array.isArray(p.nodes) ? (p.nodes as ObservedNode[]) : [];
    return [{
      observed_at: p.observed_at,
      observer: typeof p.observer === 'string' ? p.observer : '?',
      scope: p.scope,
      nodes,
    }];
  });
}

export function parseComparisons(raw: unknown): readonly Comparison[] {
  return asArray(raw).flatMap((r) => {
    const p = (r as { payload?: unknown }).payload as Record<string, unknown> | undefined;
    if (!p || typeof p.scope !== 'string') return [];
    const c = (p.counts ?? {}) as Record<string, unknown>;
    const n = (k: string): number => (typeof c[k] === 'number' ? (c[k] as number) : 0);
    return [{
      observed_at: typeof p.observed_at === 'string' ? p.observed_at : '',
      scope: p.scope,
      counts: {
        observed: n('observed'),
        participating_declared: n('participating_declared'),
        observed_not_declared: n('observed_not_declared'),
        declared_not_observed: n('declared_not_observed'),
        drift: n('drift'),
      },
    }];
  });
}

/** Newest observation per scope — the reader serves newest-first, so
 *  the first row of each scope wins. */
export function latestByScope(rows: readonly Observation[]): ReadonlyMap<string, Observation> {
  const out = new Map<string, Observation>();
  for (const r of rows) if (!out.has(r.scope)) out.set(r.scope, r);
  return out;
}

export function latestComparison(rows: readonly Comparison[], scope: string): Comparison | null {
  return rows.find((r) => r.scope === scope) ?? null;
}

/** Zero everywhere-it-matters is the good state and says so; anything
 *  else names what disagrees. */
export function comparisonVerdict(c: Comparison): { ok: boolean; text: string } {
  const k = c.counts;
  const problems: string[] = [];
  if (k.observed_not_declared > 0) problems.push(`${k.observed_not_declared} in the cluster but undeclared`);
  if (k.declared_not_observed > 0) problems.push(`${k.declared_not_observed} declared but not seen`);
  if (k.drift > 0) problems.push(`${k.drift} drifted from declaration`);
  if (problems.length === 0) {
    return { ok: true, text: `${k.observed} observed, ${k.participating_declared} declared — no drift` };
  }
  return { ok: false, text: problems.join('; ') };
}

export async function fetchEstate(): Promise<EstateState> {
  const [nodes, observations, comparisons] = await Promise.all([
    fetchRemote('/api/estate/nodes', parseNodes),
    fetchRemote('/api/estate/observations?limit=20', parseObservations),
    fetchRemote('/api/estate/comparisons?limit=20', parseComparisons),
  ]);
  return { nodes, observations, comparisons };
}
