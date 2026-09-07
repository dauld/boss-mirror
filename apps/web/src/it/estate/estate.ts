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
// registry DECLARES this door — service_instances row `boss-dev-ssh`
// in migration 202608310030-the-dev-session-has-an-ssh-door.sql:
// Dropbear in the boss-dev pod, LoadBalancer 10.20.0.35 port 22,
// key-only, root — but the jobs API serves only
// /api/estate/nodes|observations|comparisons today (boss-jobs
// http/mod.rs); there is no service-instances read endpoint. So the
// row lives twice, and a fact that lives twice gets an equality test:
// estate.test.ts pins the literal below to that migration, so a drift
// is a red test, not a dead link. When a read endpoint lands, these
// constants die and the launch block renders from the registry like
// everything else on the page. Tracked on the estate reader item
// d471a8ce.
export type SshDoor = Readonly<{ user: string; host: string }>;
export const DEV_SSH_DOOR: SshDoor = { user: 'root', host: '10.20.0.35' };
export const DEV_SSH_LABEL = `${DEV_SSH_DOOR.user}@${DEV_SSH_DOOR.host}`;
export const DEV_SSH_URL = `ssh://${DEV_SSH_LABEL}`;

/** A declared node that can carry a jump: the bastion's address is the
 *  one field the route cannot do without, so the type says so. */
export type BastionNode = EstateNode & Readonly<{ address: string }>;

/** The live node the registry declares as the bastion (role=bastion —
 *  boss-gcp, the WireGuard hub, since 202609050510). 10.20.0.35 is a
 *  LAN address; from outside the VPN the only way to it is through
 *  this node. Null when none is declared, retired, or address-less:
 *  the page then renders no route at all, never a broken one. */
export function bastionOf(nodes: readonly EstateNode[]): BastionNode | null {
  const isLiveBastion = (n: EstateNode): n is BastionNode =>
    !n.retired && n.role === 'bastion' && n.address !== null;
  return nodes.find(isLiveBastion) ?? null;
}

export type BastionRoutes = Readonly<{
  /** Open a shell on the bastion. No username: the viewer's ssh config supplies it. */
  shellUrl: string;
  /** The second hop, typed on the bastion. */
  hopCommand: string;
  /** Both hops in one line, from anywhere. */
  jumpCommand: string;
  /** ~/.ssh/config lines that make the primary ssh:// link work from anywhere. */
  sshConfig: string;
}>;

/** The three ways through the bastion to the door, spelled verbatim.
 *  An ssh:// URL cannot express a ProxyJump, so the jump is offered as
 *  a command and as config rather than as a link. `<you>` is left for
 *  the viewer: the bastion account is theirs, not the page's. */
export function bastionRoutes(bastionAddress: string, door: SshDoor = DEV_SSH_DOOR): BastionRoutes {
  const target = `${door.user}@${door.host}`;
  return {
    shellUrl: `ssh://${bastionAddress}`,
    hopCommand: `ssh ${target}`,
    jumpCommand: `ssh -J <you>@${bastionAddress} ${target}`,
    sshConfig: `Host ${door.host}\n  ProxyJump <you>@${bastionAddress}`,
  };
}

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
