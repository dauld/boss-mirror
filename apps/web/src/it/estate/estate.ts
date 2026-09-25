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
import { sinceText } from '../yard/yard-floor';

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
  /** The host a SELF-SCOPED comparison is about — compare_host stamps
   *  it (estate_compare.rs, the raiser's series key); the cluster and
   *  door comparisons carry none, and neither does a literal built
   *  before this key was read, hence optional. */
  host?: string | null;
  counts: ComparisonCounts;
}>;

// THE HOST COMPARISON (backlog 2d8d983b; page audit 2cff1d6e, GAP 1).
// Until this read the page rendered the cluster's verdict only, while
// every host row carried a finding: measured 2026-09-23, each forge
// row 15:17Z-16:17Z drift 1 (memory declared 30, observed 31), and
// boss-gcp's 10:25Z row disk_tight 1 (13 G free against a 17 G floor)
// plus drift 1. None of it reached the estate surface.
//
// SCOPED, not taken from the unscoped page of 20 the rest of the
// section reads (75027a93): forge compares every 15 minutes and
// boss-gcp once a day, so a mixed page spent by the five-minute series
// held boss-gcp's row about one hour in twenty-four. The reader caps a
// page at 50 (jobs.rs estate_events) and filters by scope only, so even
// scoped a daily host falls off after ~12 h of forge rows (49 forge, 1
// boss-gcp at the 2026-09-23 measure) — which is why the page states
// how far back its page reached whenever it is not the whole series,
// rather than letting an absent host read as silence.
export const HOST_COMPARISONS_READ = '/api/estate/comparisons?scope=host&limit=50';

/** One page of the host series: the host rows, the series' total, and
 *  the oldest instant the page reached (null when it is empty). */
export type HostComparisonPage = Readonly<{
  rows: readonly Comparison[];
  total: number | null;
  oldest: string | null;
}>;

// THE LOOPS (backlog 0d9b2960; page audit 2cff1d6e, GAP 10). The
// estate is kept by loops — the hosts converge themselves, observe
// their units and the other hosts, the forge watches the cluster from
// outside, the runners answer ops-requests — and every run leaves a
// packet. This page showed none of them, so "did the loop run" had no
// answer here while ~2,000 packets of each kind sat in the log. The
// newest TERMINAL of each loop is that answer (outcome and age); an
// open packet is a run in flight, or one that never finished.
//
// PER HOST where the packet names one, and only there. Measured
// 2026-09-24: an ops-request carries `metadata.host`; the forge and
// boss-gcp converges stamp `node_id` on their run step; the watchdog,
// the cluster converge and both observers name no host (observe-host
// runs on BOTH the forge and boss-gcp under one kind, and neither
// packet says which). The page says "not named on the packet" rather
// than guess one from a unit file it cannot read.

/** A loop the page reads by kind. Pinned to infra/platform/workflows by
 *  estate.test.ts, so a renamed kind is a red test rather than a row
 *  that reads "never finished" forever. */
export type EstateLoop = Readonly<{ kind: string; label: string }>;

export const ESTATE_LOOPS: readonly EstateLoop[] = [
  { kind: 'maintenance-forge-converge', label: 'forge converge' },
  { kind: 'maintenance-boss-gcp-converge', label: 'boss-gcp converge' },
  { kind: 'maintenance-cluster-converge', label: 'cluster converge' },
  { kind: 'maintenance-cluster-watchdog', label: 'cluster watchdog' },
  { kind: 'maintenance-estate-observe-host', label: 'observe hosts' },
  { kind: 'maintenance-estate-observe-units', label: 'observe units' },
];

/** The ops-request loop is read once per host that DECLARES it answers
 *  them — the `ops-runner` role (infra/estate/roles.toml: "which hosts
 *  should be answering ... so silence has something to be silence
 *  from"). Its packets carry `metadata.host`, so the split is the
 *  server's filter, not this page's. */
export const OPS_REQUEST_KIND = 'ops-request';
export const OPS_RUNNER_ROLE = 'ops-runner';

/** The success terminals of the loops above; every other terminal they
 *  declare (failed, refused) renders as trouble. Held to the workflow
 *  files by estate.test.ts. */
export const LOOP_OK_OUTCOMES: ReadonlySet<string> = new Set(['completed', 'answered']);

/** One packet of a loop, reduced to what "did it run" needs. */
export type LoopPacket = Readonly<{
  id: string;
  status: string;
  /** `metadata.outcome`, stamped on close; null while open. */
  outcome: string | null;
  /** When it closed (a terminal) or opened (an open packet). */
  at: string | null;
  /** The host the packet names, or null when it names none. */
  host: string | null;
}>;

export type LoopPlan = Readonly<{ kind: string; label: string; host: string | null }>;

export type LoopRow = LoopPlan & Readonly<{
  /** Ready-and-null is a kind with no closed packet at all. */
  latest: Remote<LoopPacket | null>;
  open: Remote<readonly LoopPacket[]>;
}>;

export type EstateState = Readonly<{
  nodes: Remote<readonly EstateNode[]>;
  observations: Remote<readonly Observation[]>;
  comparisons: Remote<readonly Comparison[]>;
  hostComparisons: Remote<HostComparisonPage>;
  loops: readonly LoopRow[];
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
      command: `grep -qsF 'Match host ${host} ' ~/.ssh/config || cloudflared access ssh-config --hostname ${host} --short-lived-cert | sed '/^Add to your/d' >> ~/.ssh/config`,
      why: `it appends a ProxyCommand stanza for ${host}; ssh then reaches it like any other host. The sed drops cloudflared's "Add to your …/.ssh/config:" banner, which ssh cannot parse, and the grep makes a second run a no-op.`,
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
      host: typeof p.host === 'string' ? p.host : null,
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

/** The host series' page as the reader serves it (`{data, total}`),
 *  kept to host rows whatever came back. */
export function parseHostComparisons(raw: unknown): HostComparisonPage {
  const rows = parseComparisons(raw).filter((c) => c.scope === 'host');
  const total = (raw as { total?: unknown } | null)?.total;
  return {
    rows,
    total: typeof total === 'number' ? total : null,
    oldest: rows.at(-1)?.observed_at ?? null,
  };
}

/** Newest comparison per host — rows arrive newest-first, so the first
 *  row naming a host is its latest word — in host order, so a refresh
 *  does not reshuffle the lines. */
export function latestPerHost(rows: readonly Comparison[]): readonly Comparison[] {
  const out = new Map<string, Comparison>();
  for (const r of rows) {
    const key = r.host ?? '';
    if (!out.has(key)) out.set(key, r);
  }
  return [...out.values()].sort((a, b) => (a.host ?? '').localeCompare(b.host ?? ''));
}

/** Zero everywhere-it-matters is the good state and says so; anything
 *  else names what disagrees. A self-scoped (host) comparison counts
 *  no declared total and observes only itself, so its words differ in
 *  those two places and nowhere else. */
export function comparisonVerdict(c: Comparison): { ok: boolean; text: string } {
  const k = c.counts;
  const selfScoped = c.host != null;
  const problems: string[] = [];
  if (k.observed_not_declared > 0) {
    problems.push(`${k.observed_not_declared} ${selfScoped ? 'observed but not declared' : 'in the cluster but undeclared'}`);
  }
  if (k.declared_not_observed > 0) problems.push(`${k.declared_not_observed} declared but not seen`);
  if (k.drift > 0) problems.push(`${k.drift} drifted from declaration`);
  // Headroom, not paperwork: a machine out of room stops the pipeline,
  // so "no drift" must not render beside it (a520737f).
  if ((k.disk_tight ?? 0) > 0) problems.push(`${k.disk_tight} short of disk`);
  if ((k.disk_unmeasured ?? 0) > 0) problems.push(`${k.disk_unmeasured} with no free-space reading`);
  if (problems.length === 0) {
    if (selfScoped) return { ok: true, text: `${k.observed} observed — no drift` };
    return { ok: true, text: `${k.observed} observed, ${k.participating_declared} declared — no drift` };
  }
  return { ok: false, text: problems.join('; ') };
}

const str = (v: unknown): string | null => (typeof v === 'string' ? v : null);

export function parseLoopPackets(raw: unknown): readonly LoopPacket[] {
  return asArray(raw).map((r) => {
    const o = r as Record<string, unknown>;
    if (typeof o.id !== 'string' || typeof o.status !== 'string') {
      throw new Error('jobs row missing id/status');
    }
    const md = (o.metadata ?? {}) as Record<string, unknown>;
    const steps = Array.isArray(o.steps) ? (o.steps as Record<string, unknown>[]) : [];
    const run = steps.find((s) => s.spec_slug === 'run');
    const runMd = (run?.metadata ?? {}) as Record<string, unknown>;
    const open = o.status === 'open';
    return {
      id: o.id,
      status: o.status,
      outcome: open ? null : str(md.outcome),
      at: open ? (str(o.opened_at) ?? str(md.opened_at)) : (str(md.closed_at) ?? str(o.closed_on)),
      host: str(md.host) ?? str(runMd.node_id),
    };
  });
}

/** The rows the page reads: every declared loop, then the ops-request
 *  loop per live host declaring the runner role. With the registry
 *  unreadable the runner row is kept, unfiltered — "did ANY runner
 *  answer" is still a question the log can settle. */
export function loopPlan(nodes: Remote<readonly EstateNode[]>): readonly LoopPlan[] {
  const loops = ESTATE_LOOPS.map((l) => ({ ...l, host: null }));
  if (nodes.kind !== 'ready') return [...loops, { kind: OPS_REQUEST_KIND, label: 'ops-request', host: null }];
  const runners = nodes.data.filter((n) => !n.retired && n.roles.includes(OPS_RUNNER_ROLE));
  return [...loops, ...runners.map((n) => ({ kind: OPS_REQUEST_KIND, label: 'ops-request', host: n.id }))];
}

/** Newest terminal = the first closed row (the listing is newest-opened
 *  first); open = every open packet of the kind, typically none or one. */
export function loopQueries(kind: string, host: string | null): { latest: string; open: string } {
  const narrow = host === null ? '' : `&metadata=${encodeURIComponent(JSON.stringify({ host }))}`;
  return {
    latest: `/api/jobs?kind=${encodeURIComponent(kind)}&status=closed&limit=1${narrow}`,
    open: `/api/jobs?kind=${encodeURIComponent(kind)}&status=open${narrow}`,
  };
}

/** The host a row is about, from the query or the packet — never
 *  guessed (see THE LOOPS above). */
export function loopHost(row: LoopRow): string {
  if (row.host) return row.host;
  const fromLatest = row.latest.kind === 'ready' ? (row.latest.data?.host ?? null) : null;
  const fromOpen = row.open.kind === 'ready' ? (row.open.data.find((p) => p.host)?.host ?? null) : null;
  return fromLatest ?? fromOpen ?? 'not named on the packet';
}

/** How long ago, to the minute — a five-minute loop dated "today"
 *  answers nothing. The board's own reading (yard-floor sinceText). */
export function loopAge(at: string | null, now: Date): string {
  return at ? `${sinceText(at, now.getTime())} ago` : 'undated';
}

async function fetchLoop(plan: LoopPlan): Promise<LoopRow> {
  const q = loopQueries(plan.kind, plan.host);
  const [latest, open] = await Promise.all([
    fetchRemote(q.latest, (raw) => parseLoopPackets(raw)[0] ?? null),
    fetchRemote(q.open, parseLoopPackets),
  ]);
  return { ...plan, latest, open };
}

export async function fetchEstate(): Promise<EstateState> {
  const nodesRead = fetchRemote('/api/estate/nodes', parseNodes);
  const [nodes, observations, comparisons, hostComparisons, loops] = await Promise.all([
    nodesRead,
    fetchRemote('/api/estate/observations?limit=20', parseObservations),
    fetchRemote('/api/estate/comparisons?limit=20', parseComparisons),
    fetchRemote(HOST_COMPARISONS_READ, parseHostComparisons),
    nodesRead.then((n) => Promise.all(loopPlan(n).map(fetchLoop))),
  ]);
  return { nodes, observations, comparisons, hostComparisons, loops };
}
