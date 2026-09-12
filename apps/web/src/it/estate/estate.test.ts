import { afterEach, describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import {
  bastionOf,
  bastionRoutes,
  comparisonVerdict,
  DEV_SSH_DOOR,
  DEV_SSH_LABEL,
  DEV_SSH_URL,
  fetchEstate,
  latestByScope,
  latestComparison,
  parseComparisons,
  parseNodes,
  parseObservations,
  type Comparison,
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

describe('the dev workspace door', () => {
  test('the launch anchor target is the declared ssh door, exactly', () => {
    // The href the page renders. Hardcoded until a service-instances
    // read endpoint exists (see the constant's comment); this pin
    // means a silent change to the door's address fails a test rather
    // than shipping a dead link. The literal is the `boss-dev-ssh`
    // service_instances row of migration 202608310030: LoadBalancer
    // 10.20.0.35, port 22, root. Change that row and this must go red.
    expect(DEV_SSH_URL).toBe('ssh://root@10.20.0.35');
    expect(DEV_SSH_LABEL).toBe('root@10.20.0.35');
    expect(DEV_SSH_DOOR).toEqual({ user: 'root', host: '10.20.0.35' });
  });
});

describe('the bastion route', () => {
  // 10.20.0.35 is a LAN address: the primary link works on the VPN and
  // is dead from outside, where the only way in is THROUGH the bastion
  // (boss-gcp, the WireGuard hub). ssh:// cannot carry a ProxyJump, so
  // the page offers the jump explicitly — sourced from the registry
  // node with role=bastion, never from a second hardcoded address.
  const bastion = node({ id: 'boss-gcp', label: 'boss-gcp', address: '34.45.110.40', role: 'bastion', cpu: 4, memory_gb: 15, disk_gb: 48 });

  test('bastionOf picks the live node declared as the bastion', () => {
    const b = bastionOf(parseNodes([node(), bastion]));
    expect(b?.id).toBe('boss-gcp');
    expect(b?.address).toBe('34.45.110.40');
  });

  test('bastionOf is null when no bastion is declared, when it is retired, or when it has no address', () => {
    // A missing route renders as nothing, never as ssh://null.
    expect(bastionOf(parseNodes([node()]))).toBeNull();
    expect(bastionOf(parseNodes([node(), { ...bastion, retired: true }]))).toBeNull();
    expect(bastionOf(parseNodes([node(), { ...bastion, address: null }]))).toBeNull();
    expect(bastionOf([])).toBeNull();
  });

  test('bastionRoutes spells the three ways through, verbatim', () => {
    const r = bastionRoutes('34.45.110.40', { user: 'root', host: '10.20.0.35' });
    // No username baked in: the viewer's ssh config supplies it.
    expect(r.shellUrl).toBe('ssh://34.45.110.40');
    expect(r.hopCommand).toBe('ssh root@10.20.0.35');
    expect(r.jumpCommand).toBe('ssh -J <you>@34.45.110.40 root@10.20.0.35');
    expect(r.sshConfig).toBe('Host 10.20.0.35\n  ProxyJump <you>@34.45.110.40');
  });

  test('bastionRoutes defaults to the pinned dev door', () => {
    expect(bastionRoutes('34.45.110.40')).toEqual(bastionRoutes('34.45.110.40', DEV_SSH_DOOR));
  });
});

describe('EstatePage renders the bastion route from the registry', () => {
  // Source-level pin, the TriageBoard posture: bun test has no Svelte
  // pass, and the coupling this guards against — a third address typed
  // into the page — would appear in the source, not in a render.
  const source = readFileSync(new URL('./EstatePage.svelte', import.meta.url), 'utf8');
  const code = source
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/(^|[^:])\/\/.*$/gm, '$1');

  test('carries no address of its own — every IP comes from the registry node or the pinned door', () => {
    expect(code).not.toMatch(/\b\d{1,3}(?:\.\d{1,3}){3}\b/);
  });

  test('selects the bastion from the nodes read and gates the block on it', () => {
    expect(code).toMatch(/bastionOf\(/);
    expect(code).toMatch(/bastionRoutes\(/);
    // The block exists only inside an {#if} on the derived bastion —
    // absent bastion, no link, so never a broken one.
    expect(code).toMatch(/\{#if\s+bastion\b[^}]*\}[\s\S]*?bastion\.address[\s\S]*?\{\/if\}/);
  });

  test('the primary door is unchanged and the jump link carries no username', () => {
    expect(code).toMatch(/href=\{DEV_SSH_URL\}/);
    expect(code).toMatch(/href=\{routes\.shellUrl\}/);
    expect(code).toMatch(/routes\.jumpCommand/);
    expect(code).toMatch(/routes\.sshConfig/);
  });
});
