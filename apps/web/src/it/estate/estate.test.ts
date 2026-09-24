import { afterEach, describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import {
  comparisonVerdict,
  DEV_DOOR_HOST,
  devDoorSteps,
  ESTATE_LOOPS,
  fetchEstate,
  latestByScope,
  latestComparison,
  loopAge,
  loopHost,
  loopPlan,
  loopQueries,
  LOOP_OK_OUTCOMES,
  OPS_REQUEST_KIND,
  OPS_RUNNER_ROLE,
  parseComparisons,
  parseLoopPackets,
  parseNodes,
  parseObservations,
  type Comparison,
  type LoopRow,
} from './estate';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

function node(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: 'w-1', label: 'Worker 1', address: '10.20.0.14', role: 'talos-worker',
    cpu: 32, memory_gb: 63, disk_gb: 929, notes: 'the build node', retired: false,
    ...over,
  };
}

function obsEvent(scope: string, observed_at: string, nodes: unknown[] = [{ id: 'x' }]): unknown {
  return { payload: { scope, observed_at, observer: 'boss-estate-observe', nodes } };
}

describe('parseNodes', () => {
  test('accepts both bare arrays and {data: []} envelopes, and carries declared capacity through', () => {
    const rows = parseNodes({ data: [node()] });
    expect(rows).toHaveLength(1);
    expect(rows[0]?.cpu).toBe(32);
    expect(rows[0]?.memory_gb).toBe(63);
    expect(parseNodes([node()])[0]?.id).toBe('w-1');
  });

  test('a row without an id is a parse failure, not a silently dropped machine', () => {
    // A machine vanishing from the render because a field went missing
    // is exactly the estate's failure story (w-1 invisible for five
    // days) — refuse loudly instead.
    expect(() => parseNodes([{ role: 'talos-worker' }])).toThrow();
  });
  test('declared roles ride along as a set; a node declaring none reads empty', () => {
    const [gcp, w1] = parseNodes([
      { ...node(), id: 'boss-gcp', role: 'bastion', roles: ['legacy-stack', 'wireguard-bastion'] },
      node(),
    ]);
    expect(gcp?.roles).toEqual(['legacy-stack', 'wireguard-bastion']);
    expect(w1?.roles).toEqual([]);
  });
});

describe('observations and comparisons', () => {
  test('latestByScope keeps the newest-first row per scope', () => {
    const rows = parseObservations([
      obsEvent('kubernetes-nodes', '2026-08-31T10:20:00Z', [{ id: 'a' }, { id: 'b' }]),
      obsEvent('host', '2026-08-31T10:25:00Z'),
      obsEvent('kubernetes-nodes', '2026-08-30T10:20:00Z'),
    ]);
    const byScope = latestByScope(rows);
    expect(byScope.get('kubernetes-nodes')?.nodes).toHaveLength(2);
    expect(byScope.get('host')?.observed_at).toBe('2026-08-31T10:25:00Z');
  });

  test('zero drift renders as the good state, with the counts said plainly', () => {
    const rows = parseComparisons([
      { payload: { scope: 'kubernetes-nodes', observed_at: '2026-08-31T10:20:01Z', counts: {
        observed: 5, participating_declared: 5,
        observed_not_declared: 0, declared_not_observed: 0, drift: 0,
      } } },
    ]);
    const c = latestComparison(rows, 'kubernetes-nodes');
    expect(c).not.toBeNull();
    const v = comparisonVerdict(c as Comparison);
    expect(v.ok).toBe(true);
    expect(v.text).toBe('5 observed, 5 declared — no drift');
  });

  test('a machine nobody declared is named, not averaged away', () => {
    const v = comparisonVerdict({
      observed_at: '', scope: 'kubernetes-nodes',
      counts: { observed: 6, participating_declared: 5, observed_not_declared: 1, declared_not_observed: 0, drift: 0 },
    });
    expect(v.ok).toBe(false);
    expect(v.text).toContain('1 in the cluster but undeclared');
  });

  test('a machine short of disk is not "no drift"', () => {
    // a520737f: the cluster comparison can now carry disk_tight, and a
    // page that renders "5 observed, 5 declared — no drift" beside a
    // build node at 90% full is the troubled-packet class — the state
    // has crossed its own alarm threshold and the surface must say so.
    const v = comparisonVerdict({
      observed_at: '', scope: 'kubernetes-nodes',
      counts: {
        observed: 5, participating_declared: 5, observed_not_declared: 0,
        declared_not_observed: 0, drift: 0, disk_tight: 1,
      },
    });
    expect(v.ok).toBe(false);
    expect(v.text).toContain('1 short of disk');
  });

  test('a node whose free space went unread is named, not counted as roomy', () => {
    // The kubelet read is best-effort, so going blind must look
    // different from having room — otherwise the instrument can fail
    // back into exactly the silence this packet reported.
    const v = comparisonVerdict({
      observed_at: '', scope: 'kubernetes-nodes',
      counts: {
        observed: 5, participating_declared: 5, observed_not_declared: 0,
        declared_not_observed: 0, drift: 0, disk_unmeasured: 2,
      },
    });
    expect(v.ok).toBe(false);
    expect(v.text).toContain('2 with no free-space reading');
  });

  test('the parser carries the disk counts through to the verdict', () => {
    // The verdict can only report what the parser keeps, and the parser
    // builds counts key by key — so the pair is tested end to end.
    const rows = parseComparisons([
      { payload: { scope: 'kubernetes-nodes', observed_at: '2026-09-10T13:45:00Z', counts: {
        observed: 5, participating_declared: 5, observed_not_declared: 0,
        declared_not_observed: 0, drift: 0, disk_tight: 1, disk_unmeasured: 1,
      } } },
    ]);
    const v = comparisonVerdict(rows[0] as Comparison);
    expect(v.ok).toBe(false);
    expect(v.text).toContain('1 short of disk');
    expect(v.text).toContain('1 with no free-space reading');
  });

  test('a comparison recorded before the disk counts existed still reads clean', () => {
    // Every row already in the series predates both keys.
    const v = comparisonVerdict({
      observed_at: '', scope: 'kubernetes-nodes',
      counts: {
        observed: 5, participating_declared: 5, observed_not_declared: 0,
        declared_not_observed: 0, drift: 0,
      },
    });
    expect(v.ok).toBe(true);
  });
});

describe('fetchEstate', () => {
  test('an unreachable registry lands as failed, never as an empty estate', async () => {
    globalThis.fetch = (async () => {
      throw new Error('connect ECONNREFUSED');
    }) as unknown as typeof fetch;
    const s = await fetchEstate();
    expect(s.nodes.kind).toBe('failed');
    expect(s.observations.kind).toBe('failed');
    expect(s.comparisons.kind).toBe('failed');
  });

  test('good reads land ready with parsed rows', async () => {
    globalThis.fetch = (async (url: RequestInfo | URL) => {
      const u = String(url);
      const body = u.includes('/nodes')
        ? [node()]
        : u.includes('/observations')
          ? [obsEvent('host', '2026-08-31T10:25:00Z')]
          : [];
      return new Response(JSON.stringify(body), { status: 200 });
    }) as unknown as typeof fetch;
    const s = await fetchEstate();
    expect(s.nodes.kind).toBe('ready');
    if (s.nodes.kind === 'ready') expect(s.nodes.data[0]?.id).toBe('w-1');
    expect(s.observations.kind).toBe('ready');
  });
});

// THE LOOPS (backlog 0d9b2960; page audit 2cff1d6e, GAP 10). The page
// showed none of the estate's own working or finished packets, so "did
// the loop run" had no answer on it. Each loop's newest terminal is that
// answer; an open packet is the run in flight (or stuck).

/** A packet as GET /api/jobs lists it — only the keys the page reads. */
function jobRow(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: 'aaaa1111-0000-0000-0000-000000000000',
    kind: 'maintenance-cluster-watchdog',
    status: 'closed',
    opened_at: '2026-09-24T11:14:28Z',
    metadata: { outcome: 'completed', closed_at: '2026-09-24T11:14:34Z', opened_at: '2026-09-24T11:14:28Z' },
    steps: [{ spec_slug: 'run', metadata: { result: 'ok' } }],
    ...over,
  };
}

describe('parseLoopPackets', () => {
  test('a terminal carries its outcome and the instant it closed', () => {
    const [p] = parseLoopPackets({ data: [jobRow()], total: 1 });
    expect(p).toEqual({
      id: 'aaaa1111-0000-0000-0000-000000000000',
      status: 'closed',
      outcome: 'completed',
      at: '2026-09-24T11:14:34Z',
      host: null,
    });
  });

  test('an open packet is dated from when it opened, and has no outcome yet', () => {
    const [p] = parseLoopPackets([jobRow({ status: 'open', opened_at: '2026-09-24T11:20:00Z', metadata: {} })]);
    expect(p?.status).toBe('open');
    expect(p?.outcome).toBeNull();
    expect(p?.at).toBe('2026-09-24T11:20:00Z');
  });

  test('the host is the one the packet names: an ops-request on its metadata, a converge on its run step', () => {
    // Measured 2026-09-24: ops-request metadata.host = "forge"; the
    // forge and boss-gcp converges stamp node_id on the run step; the
    // watchdog and the observers name no host at all.
    const [ops] = parseLoopPackets([jobRow({ kind: 'ops-request', metadata: { host: 'boss-gcp', outcome: 'answered' } })]);
    expect(ops?.host).toBe('boss-gcp');
    const [conv] = parseLoopPackets([jobRow({ steps: [{ spec_slug: 'run', metadata: { node_id: 'forge', result: 'ok' } }] })]);
    expect(conv?.host).toBe('forge');
    const [none] = parseLoopPackets([jobRow()]);
    expect(none?.host).toBeNull();
  });

  test('a row with no id is refused, not rendered as a link to nowhere', () => {
    expect(() => parseLoopPackets([{ status: 'closed' }])).toThrow();
  });
});

describe('loopPlan', () => {
  const nodes = [
    parseNodes([node({ id: 'forge', role: 'forge', roles: ['cluster-operator', OPS_RUNNER_ROLE] })])[0]!,
    parseNodes([node({ id: 'boss-gcp', role: 'bastion', roles: [OPS_RUNNER_ROLE] })])[0]!,
    parseNodes([node({ id: 'old-box', role: 'forge', roles: [OPS_RUNNER_ROLE], retired: true })])[0]!,
    parseNodes([node({ id: 'w-1' })])[0]!,
  ];

  test('every declared loop, then one ops-request row per live host that declares the runner role', () => {
    const plan = loopPlan({ kind: 'ready', data: nodes });
    expect(plan.map((p) => [p.kind, p.host])).toEqual([
      ...ESTATE_LOOPS.map((l) => [l.kind, null]),
      [OPS_REQUEST_KIND, 'forge'],
      [OPS_REQUEST_KIND, 'boss-gcp'],
    ]);
  });

  test('an unreadable registry still asks whether ANY runner answered, rather than dropping the row', () => {
    const plan = loopPlan({ kind: 'failed', error: 'down' });
    expect(plan.filter((p) => p.kind === OPS_REQUEST_KIND)).toEqual([
      { kind: OPS_REQUEST_KIND, label: 'ops-request', host: null },
    ]);
  });
});

describe('loopQueries', () => {
  test('the newest terminal is one closed row; the open read is every open packet of the kind', () => {
    expect(loopQueries('maintenance-cluster-watchdog', null)).toEqual({
      latest: '/api/jobs?kind=maintenance-cluster-watchdog&status=closed&limit=1',
      open: '/api/jobs?kind=maintenance-cluster-watchdog&status=open',
    });
  });

  test('a host row is narrowed by metadata containment, the filter the server applies', () => {
    const q = loopQueries(OPS_REQUEST_KIND, 'forge');
    const filter = `&metadata=${encodeURIComponent(JSON.stringify({ host: 'forge' }))}`;
    expect(q.latest).toBe(`/api/jobs?kind=ops-request&status=closed&limit=1${filter}`);
    expect(q.open).toBe(`/api/jobs?kind=ops-request&status=open${filter}`);
  });
});

describe('loopHost', () => {
  const row = (over: Partial<LoopRow>): LoopRow => ({
    kind: 'k', label: 'k', host: null,
    latest: { kind: 'ready', data: null }, open: { kind: 'ready', data: [] },
    ...over,
  });

  test('a host row is its host; otherwise the host the newest packet names; otherwise it says so', () => {
    expect(loopHost(row({ host: 'forge' }))).toBe('forge');
    const packet = { id: 'x', status: 'closed', outcome: 'completed', at: null, host: 'boss-gcp' };
    expect(loopHost(row({ latest: { kind: 'ready', data: packet } }))).toBe('boss-gcp');
    expect(loopHost(row({}))).toBe('not named on the packet');
  });
});

describe('loopAge', () => {
  // A five-minute loop dated "today" answers nothing; the age is read
  // to the minute (the board's sinceText), and a missing stamp says so.
  const now = new Date('2026-09-24T12:00:00Z');
  test('minutes, then hours, from the instant the packet names', () => {
    expect(loopAge('2026-09-24T11:55:00Z', now)).toBe('5m ago');
    expect(loopAge('2026-09-24T09:00:00Z', now)).toBe('3h ago');
  });
  test('no stamp is undated, never a zero age', () => {
    expect(loopAge(null, now)).toBe('undated');
  });
});

describe('fetchEstate reads the loops', () => {
  test('an unreachable jobs API lands every loop row as failed — never as a loop that did not run', async () => {
    globalThis.fetch = (async (url: RequestInfo | URL) => {
      const u = String(url);
      if (u.startsWith('/api/jobs')) return new Response('', { status: 502 });
      const body = u.includes('/nodes') ? [node({ id: 'forge', roles: [OPS_RUNNER_ROLE] })] : [];
      return new Response(JSON.stringify(body), { status: 200 });
    }) as unknown as typeof fetch;
    const s = await fetchEstate();
    expect(s.loops.length).toBe(ESTATE_LOOPS.length + 1);
    expect(s.loops.every((l) => l.latest.kind === 'failed' && l.open.kind === 'failed')).toBe(true);
  });

  test('a kind with no closed packet reads ready-and-null, which the page renders as never finished', async () => {
    globalThis.fetch = (async (url: RequestInfo | URL) => {
      const u = String(url);
      if (u.includes('/nodes')) {
        return new Response(JSON.stringify([node({ id: 'forge', roles: [OPS_RUNNER_ROLE] })]), { status: 200 });
      }
      if (u.includes('kind=maintenance-cluster-watchdog&status=closed')) {
        return new Response(JSON.stringify({ data: [jobRow()], total: 1 }), { status: 200 });
      }
      return new Response(JSON.stringify({ data: [], total: 0 }), { status: 200 });
    }) as unknown as typeof fetch;
    const s = await fetchEstate();
    const wd = s.loops.find((l) => l.kind === 'maintenance-cluster-watchdog');
    expect(wd?.latest).toEqual({ kind: 'ready', data: parseLoopPackets([jobRow()])[0]! });
    const other = s.loops.find((l) => l.kind === 'maintenance-forge-converge');
    expect(other?.latest).toEqual({ kind: 'ready', data: null });
    expect(s.loops.find((l) => l.kind === OPS_REQUEST_KIND)?.host).toBe('forge');
  });
});

describe('the loops the page reads are in the registry', () => {
  // A kind renamed in the registry would otherwise render "never
  // finished" forever, a confident wrong answer (CLAUDE.md §9a: the
  // list lives here AND in infra/platform/workflows, so it is pinned).
  const workflow = (kind: string): string =>
    readFileSync(new URL(`../../../../../infra/platform/workflows/${kind}.toml`, import.meta.url), 'utf8');
  const kinds = [...ESTATE_LOOPS.map((l) => l.kind), OPS_REQUEST_KIND];

  test('every kind the page reads is a workflow in the tree', () => {
    for (const k of kinds) expect(workflow(k)).toContain(`kind = "${k}"`);
  });

  test('every terminal those workflows declare is one the page has decided the colour of', () => {
    // The ok set is the success terminals; everything else these
    // workflows can end in (failed, refused) must render as trouble. A
    // new terminal outcome lands here as a red test, not as a silent ok.
    const declared = new Set(
      kinds.flatMap((k) => [...workflow(k).matchAll(/terminal = \{ outcome = "([^"]+)" \}/g)].map((m) => m[1]!)),
    );
    expect([...declared].sort()).toEqual(['answered', 'completed', 'failed', 'refused']);
    expect([...LOOP_OK_OUTCOMES].sort()).toEqual(['answered', 'completed']);
  });
});

describe('the dev workspace door', () => {
  // The door moved from a LAN VIP behind a WireGuard bastion to one
  // public hostname behind a Cloudflare Access SSH application (design
  // 5fc71f03; backlog e4cedb46). The hostname is pinned to the tunnel
  // route that serves it by the Rust test
  // the_dev_door_is_an_access_ssh_application.rs — this file pins the
  // WORDS an operator pastes, which nothing else reads.
  test('the hostname is the declared one', () => {
    expect(DEV_DOOR_HOST).toBe('dev.algedonic.dev');
  });

  test('the block is the three-line setup, in order, each with its reason', () => {
    const steps = devDoorSteps();
    expect(steps.map((s) => s.command)).toEqual([
      'cloudflared --version',
      "grep -qsF 'Match host dev.algedonic.dev ' ~/.ssh/config || cloudflared access ssh-config --hostname dev.algedonic.dev --short-lived-cert | sed '/^Add to your/d' >> ~/.ssh/config",
      'ssh root@dev.algedonic.dev',
    ]);
    // A command pasted blind is a command nobody can judge.
    expect(steps.every((s) => s.why.length > 0 && s.what.length > 0)).toBe(true);
  });

  test('the route step appends only the stanza, and only once', () => {
    // cloudflared prints "Add to your <home>/.ssh/config:" above the
    // stanza. It is not a # comment, so appending it raw leaves a line
    // ssh refuses to parse, and the operator deleted it by hand every
    // time. A second run appended a second stanza, so the step is
    // guarded on the one it writes.
    const route = devDoorSteps()[1]?.command ?? '';
    expect(route).toContain("sed '/^Add to your/d'");
    expect(route.startsWith("grep -qsF 'Match host dev.algedonic.dev ' ~/.ssh/config || ")).toBe(true);
  });

  test('every step names the host it was given, so one constant moves them all', () => {
    const steps = devDoorSteps('dev.example.test');
    expect(steps[1]?.command).toContain('dev.example.test');
    expect(steps[2]?.command).toBe('ssh root@dev.example.test');
    // The root login is what the certificate's principal must match:
    // sshd with no AuthorizedPrincipalsFile requires cert principal ==
    // login name (infra/cluster/manifests/boss-dev.yaml).
    expect(steps[2]?.command.startsWith('ssh root@')).toBe(true);
  });
});

describe('EstatePage renders the door from the module', () => {
  // Source-level pin, the TriageBoard posture: bun test has no Svelte
  // pass, and the coupling this guards against — an address or a
  // command typed into the page — would appear in the source, not in a
  // render.
  const source = readFileSync(new URL('./EstatePage.svelte', import.meta.url), 'utf8');
  const code = source
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/(^|[^:])\/\/.*$/gm, '$1');

  test('carries no address of its own — every IP came from the registry node, and none is left', () => {
    expect(code).not.toMatch(/\b\d{1,3}(?:\.\d{1,3}){3}\b/);
  });

  test('the setup block is rendered from devDoorSteps, not retyped', () => {
    expect(code).toMatch(/devDoorSteps\(\)/);
    expect(code).toMatch(/\{#each\s+doorSteps\b/);
    expect(code).toMatch(/\{step\.command\}/);
    expect(code).not.toMatch(/cloudflared access ssh-config/);
  });

  test('the bastion route is gone with the door it served', () => {
    expect(code).not.toMatch(/bastion/i);
    expect(code).not.toMatch(/ProxyJump/);
  });
});
