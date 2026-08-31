import { afterEach, describe, expect, test } from 'bun:test';
import {
  comparisonVerdict,
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
    // than shipping a dead link.
    expect(DEV_SSH_URL).toBe('ssh://root@10.20.0.35');
  });
});
