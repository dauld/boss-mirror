import { afterEach, describe, expect, test } from 'bun:test';
import { loadDriftPackets, newestMeasured, parseDriftPackets, type DriftPacket } from './drift';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

// A row as infra/protocol-drift.sh files it: `metadata.measured` plus
// `metadata.drift`, the two top-level keys its `file` verb PATCHes.
// The values are the ones the script's header measured on 2026-09-15
// against the system of record: 85 admitted, 47 compared, 2 adrift.
function measured(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    at: '2026-09-15T05:20:41Z',
    head: '8c2eb5236b3a1e0f4d5c7a9b8e2f1d0c3b4a5968',
    head_at: '2026-09-15T04:13:02Z',
    head_why: null,
    target: 'http://10.20.0.34:7900',
    lint_exit: 2,
    live_admitted: 85,
    authored: 49,
    fields_parsed: 49,
    fields_compared: 47,
    exempt: ['maintenance-protocol-drift'],
    method: { comparator: 'infra/lint/the-live-protocols-are-the-authored-protocols.sh --require-live --report-json' },
    ...over,
  };
}

const FIELDS = [
  {
    kind: 'maintenance-sweep',
    field: 'description',
    live_version: 2,
    at: 41,
    tree_len: 212,
    live_len: 188,
    tree_window: 'sweep runs nightly and files what it reclaimed onto the packet',
    live_window: 'sweep runs nightly and reports what it reclaimed to the journal',
  },
  {
    kind: 'ship-a-change',
    field: 'description',
    live_version: 31,
    at: 0,
    tree_len: 640,
    live_len: 601,
    tree_window: 'A change to the tree, from branch to converged',
    live_window: 'One change to the tree from branch to live',
  },
];

function drift(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    counts: { unauthored: 1, fields: 2, absent: 1, pending: 1 },
    unauthored: ['legacy-hand-seeded'],
    fields: FIELDS,
    absent: [{ kind: 'design-doc', field: 'category' }],
    pending: ['maintenance-protocol-drift'],
    tenants: [{ file: 'examples/brewery/workflows', not_admitted: 0, total: 36 }],
    ...over,
  };
}

function packet(id: string, m: Record<string, unknown> = {}, d: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id,
    kind: 'maintenance-protocol-drift',
    status: 'closed',
    title: `Protocol drift — ${id}`,
    metadata: { chore: 'maintenance-protocol-drift', outcome: 'completed', measured: measured(m), drift: drift(d) },
  };
}

describe('parseDriftPackets', () => {
  test('keeps only packets carrying a measurement, and counts the ones that do not', () => {
    // A failed run: ExecStartPre opened the packet, the script refused
    // (exit 3) and PATCHed nothing — the packet exists with no `measured`.
    const failed = { id: 'f1', title: 'Protocol drift — failed', metadata: { outcome: 'failed' } };
    const r = parseDriftPackets({ data: [packet('p1'), failed], total: 2 });
    expect(r.total).toBe(2);
    expect(r.unmeasured).toBe(1);
    expect(r.packets).toHaveLength(1);
    const p = r.packets[0]!;
    expect(p.id).toBe('p1');
    expect(p.measured.at).toBe('2026-09-15T05:20:41Z');
    expect(p.measured.head).toBe('8c2eb5236b3a1e0f4d5c7a9b8e2f1d0c3b4a5968');
    expect(p.measured.live_admitted).toBe(85);
    expect(p.measured.fields_compared).toBe(47);
    expect(p.measured.lint_exit).toBe(2);
  });

  test('every drift finding the script names survives, by name', () => {
    const p = parseDriftPackets([packet('p1')]).packets[0]!;
    expect(p.drift.counts).toEqual({ unauthored: 1, fields: 2, absent: 1, pending: 1 });
    expect(p.drift.fields).toHaveLength(2);
    const f = p.drift.fields[1]!;
    expect(f.kind).toBe('ship-a-change');
    expect(f.field).toBe('description');
    expect(f.live_version).toBe(31);
    expect(f.at).toBe(0);
    expect(f.tree_len).toBe(640);
    expect(f.live_len).toBe(601);
    expect(f.tree_window).toBe('A change to the tree, from branch to converged');
    expect(f.live_window).toBe('One change to the tree from branch to live');
    expect(p.drift.unauthored).toEqual(['legacy-hand-seeded']);
    expect(p.drift.pending).toEqual(['maintenance-protocol-drift']);
    expect(p.drift.absent).toEqual([{ kind: 'design-doc', field: 'category' }]);
  });

  test('a null head stays null with the row\'s own reason, never an empty sha', () => {
    const p = parseDriftPackets([
      packet('p1', { head: null, head_at: null, head_why: 'git could not read /opt/boss: not a git repository' }),
    ]).packets[0]!;
    expect(p.measured.head).toBeNull();
    expect(p.measured.head_at).toBeNull();
    expect(p.measured.head_why).toBe('git could not read /opt/boss: not a git repository');
  });

  test('a measurement with no drift block is a measurement of nothing adrift, counted as zero, not dropped', () => {
    const raw = packet('p1');
    delete (raw.metadata as Record<string, unknown>).drift;
    const r = parseDriftPackets([raw]);
    expect(r.packets).toHaveLength(1);
    expect(r.packets[0]!.drift.counts).toEqual({ unauthored: 0, fields: 0, absent: 0, pending: 0 });
    expect(r.packets[0]!.drift.fields).toEqual([]);
  });

  test('accepts a bare array and tolerates garbage', () => {
    expect(parseDriftPackets([packet('p1')]).packets).toHaveLength(1);
    expect(parseDriftPackets(null).packets).toHaveLength(0);
    expect(parseDriftPackets(null).total).toBe(0);
    expect(parseDriftPackets({ data: [{ metadata: { measured: 'nope' } }] }).packets).toHaveLength(0);
    // A drift row missing its kind is not a finding; the rest survive.
    const p = parseDriftPackets([packet('p1', {}, { fields: [{ field: 'label' }, FIELDS[0]] })]).packets[0]!;
    expect(p.drift.fields.map((f) => f.kind)).toEqual(['maintenance-sweep']);
  });
});

describe('newestMeasured', () => {
  const parsed = (id: string, at: string): DriftPacket => parseDriftPackets([packet(id, { at })]).packets[0]!;
  test('the newest packet is the one measured last, whatever order the API returned', () => {
    const older = parsed('old', '2026-09-15T05:20:41Z');
    const newer = parsed('new', '2026-09-16T05:20:39Z');
    expect(newestMeasured([older, newer])?.id).toBe('new');
    expect(newestMeasured([newer, older])?.id).toBe('new');
    expect(newestMeasured([])).toBeNull();
  });
});

describe('loadDriftPackets', () => {
  test('reads the kind through the jobs API and parses once at the call site', async () => {
    let url = '';
    globalThis.fetch = (async (u: string | URL | Request) => {
      url = String(u);
      return new Response(JSON.stringify({ data: [packet('p1')], total: 1 }), { status: 200 });
    }) as unknown as typeof fetch;
    const r = await loadDriftPackets(14);
    expect(url).toContain('/api/jobs?kind=maintenance-protocol-drift');
    expect(url).toContain('limit=14');
    expect(r.kind).toBe('ready');
    if (r.kind === 'ready') expect(r.data.packets).toHaveLength(1);
  });

  test('a failed read is a failure, not an empty table', async () => {
    globalThis.fetch = (async () => new Response('nope', { status: 503 })) as unknown as typeof fetch;
    const r = await loadDriftPackets(14);
    expect(r.kind).toBe('failed');
  });
});
