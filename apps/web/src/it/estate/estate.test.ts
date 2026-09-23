import { afterEach, describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import {
  comparisonVerdict,
  DEV_DOOR_HOST,
  devDoorSteps,
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
      'cloudflared access ssh-config --hostname dev.algedonic.dev --short-lived-cert >> ~/.ssh/config',
      'ssh root@dev.algedonic.dev',
    ]);
    // A command pasted blind is a command nobody can judge.
    expect(steps.every((s) => s.why.length > 0 && s.what.length > 0)).toBe(true);
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
