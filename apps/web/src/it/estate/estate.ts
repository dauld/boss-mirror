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
  /** Every role the node DECLARES (Classes of `node`, 202609120300) —
   *  the set a managed host derives its unit roster from. `role` above
   *  is the primary one the page keys on; this is the full set. Empty
   *  when the node declares nothing. */
  roles: readonly string[];
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
  /** Machines below the disk floor — free under 16 GiB or under 35% of
   *  capacity. Optional because every row recorded before a520737f
   *  predates the key on the cluster scope. */
  disk_tight?: number;
  /** Machines whose free-space reading could not be taken. The kubelet
   *  read is best-effort so the rest of the observation survives losing
   *  it; this is the count that keeps going blind distinguishable from
   *  having room. */
  disk_unmeasured?: number;
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

// THE DEV WORKSPACE DOOR (design 5fc71f03, David 2026-09-18; backlog
// e4cedb46). One hostname, from anywhere, behind a Cloudflare Access
// SSH application that issues a certificate good for one session.
//
// It replaces a MetalLB VIP on the LAN, reached from outside through
// the boss-gcp WireGuard bastion on a key that lived forever — a jump
// this page had to spell out in three forms, because ssh:// cannot
// carry a ProxyJump. None of that is needed now, so none of it is
// here; the VIP still answers and is no longer advertised
// (infra/cluster/manifests/boss-dev.yaml, Service boss-dev-ssh).
//
// HARDCODED, and loudly so, for the same reason the VIP was: the
// hostname is DECLARED — the tunnel route in
// infra/cluster/tunnel-origins.toml and the application in
// infra/cluster/dns/access.toml — but the jobs API serves only
// /api/estate/nodes|observations|comparisons (boss-jobs http/mod.rs),
// so no read reaches either file. A fact that lives twice gets an
// equality test (CLAUDE.md §9a): the Rust test
// the_dev_door_is_an_access_ssh_application.rs holds the literal below
// to the route the connector serves, so a drift is a red test rather
// than a terminal block that opens nothing. When the estate reader
// lands (d471a8ce) this constant dies and the block renders from the
// registry like everything else on the page.
export const DEV_DOOR_HOST = 'dev.algedonic.dev';

/** One line of the terminal setup, with the reason it is there: a
 *  command an operator pastes blind is a command they cannot judge. */
export type DoorStep = Readonly<{ what: string; command: string; why: string }>;

/** The one-time terminal setup for the dev door, in order. Steps 1 and
 *  2 are done once per machine; step 3 is every session — and after
 *  step 2, so is any other ssh to the name (scp, rsync, ProxyJump),
 *  because the stanza teaches ssh itself how to reach it. */
export function devDoorSteps(host: string = DEV_DOOR_HOST): readonly DoorStep[] {
  return [
    {
      what: 'Install cloudflared, once per machine',
      command: 'cloudflared --version',
      why: 'it is the client half of the tunnel: ssh talks to it, it talks to the edge. Not found means not installed — take it from Cloudflare downloads, or your package manager, and run this again.',
    },
    {
      what: 'Teach ssh the route, once per machine',
      command: `cloudflared access ssh-config --hostname ${host} --short-lived-cert >> ~/.ssh/config`,
      why: `it appends a ProxyCommand stanza for ${host}; ssh then reaches it like any other host.`,
    },
    {
      what: 'Open the workspace',
      command: `ssh root@${host}`,
      why: 'the browser asks who you are, Access issues a certificate for the session, and the pod accepts it. Nothing long-lived is stored.',
    },
  ];
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
      roles: Array.isArray(o.roles) ? o.roles.filter((x): x is string => typeof x === 'string') : [],
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
        // Absent from every row recorded before a520737f, and `n`
        // answers 0 for a missing key — a parser that dropped these
        // would render "no drift" over a full build node.
        disk_tight: n('disk_tight'),
        disk_unmeasured: n('disk_unmeasured'),
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
  // Headroom, not paperwork: a machine out of room stops the pipeline,
  // so "no drift" must not render beside it (a520737f).
  if ((k.disk_tight ?? 0) > 0) problems.push(`${k.disk_tight} short of disk`);
  if ((k.disk_unmeasured ?? 0) > 0) problems.push(`${k.disk_unmeasured} with no free-space reading`);
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
