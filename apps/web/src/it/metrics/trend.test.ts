import { afterEach, describe, expect, test } from 'bun:test';
import {
  barGeometry,
  deleteAddPct,
  headGapDays,
  lastDays,
  loadMetricsPackets,
  mergeLandings,
  newestMeasured,
  parseMetricsPackets,
  perDay,
  reading,
  recentWindow,
  registryRatio,
  type Landing,
  type MetricsPacket,
} from './trend';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

// A row as infra/codebase-metrics.sh files it: `metadata.measured` plus
// `metadata.landings`. Only the fields the page reads are filled in.
function measured(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    at: '2026-09-14T15:50:20Z',
    backfill: true,
    since: null,
    ref: 'origin/main',
    head: '5e23287bbf983dcd0feeb3e81d08964d2f238943',
    head_at: '2026-09-13T06:00:11Z',
    counts: { crates: 54, lints: 77, migrations: 136, rust_files: 887, web_files: 429 },
    window: {
      adds: 614635,
      dels: 68780,
      net: 545855,
      landings: 497,
      delete_add_pct: 11,
      by_bucket: { rust_prod: { add: 285281, del: 28645 }, docs: { add: 24806, del: 6035 } },
    },
    totals: {
      lines: 546281,
      prod_lines: 252783,
      test_lines: 172719,
      test_prod_pct: 68,
      by_bucket: { rust_prod: 181126, rust_test: 76416, rust_test_inline: 75510 },
    },
    registry: {
      rows: 246,
      by_registry: { dispatcher_rules: 66, platform_workflows: 47, step_plugins: 12, step_types: 48, tenant_workflows: 73 },
      code_branches_on_kind: 3,
      code_branches_not_counted_why: null,
      code_branches_by_class: { leaked_policy: 3, round_trip: 24 },
      code_branches_sites: {
        leaked_policy: [{ file: 'crates/core/boss-jobs/src/policy_glue.rs', line: 48, scrutinee: 'kind', literals: ['account'] }],
      },
    },
    ...over,
  };
}

function landing(over: Partial<Landing> & { sha: string; at: string }): Landing {
  return { subject: `train ${over.sha}`, add: 0, del: 0, test_add: 0, test_del: 0, ...over };
}

const L = [
  landing({ sha: 'c3', at: '2026-09-13T06:00:11Z', add: 58, del: 193, test_add: 38, test_del: 42 }),
  landing({ sha: 'c2', at: '2026-09-13T05:20:40Z', add: 40, del: 0 }),
  landing({ sha: 'c1', at: '2026-09-12T04:40:11Z', add: 402, del: 30, test_add: 265, test_del: 21 }),
];

function packet(id: string, m: Record<string, unknown> = {}, landings: ReadonlyArray<Landing> = L): Record<string, unknown> {
  return {
    id,
    kind: 'maintenance-codebase-metrics',
    status: 'closed',
    title: `Codebase metrics — ${id}`,
    metadata: { chore: 'maintenance-codebase-metrics', outcome: 'completed', measured: measured(m), landings },
  };
}

describe('parseMetricsPackets', () => {
  test('keeps only packets carrying a measurement, and counts the ones that do not', () => {
    const failed = { id: 'f1', title: 'Codebase metrics — failed', metadata: { outcome: 'failed' } };
    const r = parseMetricsPackets({ data: [packet('p1'), failed], total: 2 });
    expect(r.total).toBe(2);
    expect(r.unmeasured).toBe(1);
    expect(r.packets).toHaveLength(1);
    const p = r.packets[0]!;
    expect(p.id).toBe('p1');
    expect(p.measured.head).toBe('5e23287bbf983dcd0feeb3e81d08964d2f238943');
    expect(p.measured.window.delete_add_pct).toBe(11);
    expect(p.measured.registry?.rows).toBe(246);
    expect(p.measured.registry?.code_branches_on_kind).toBe(3);
    expect(p.measured.registry?.leaked_sites[0]?.file).toBe('crates/core/boss-jobs/src/policy_glue.rs');
    expect(p.landings).toHaveLength(3);
    expect(p.landings[0]?.test_add).toBe(38);
  });

  test('a null delete_add_pct and a null code_branches_on_kind survive as null, never zero', () => {
    const r = parseMetricsPackets({
      data: [
        packet('p1', {
          window: { adds: 0, dels: 0, net: 0, landings: 0, delete_add_pct: null, by_bucket: {} },
          registry: { rows: 5, code_branches_on_kind: null, code_branches_not_counted_why: 'no counter on this machine' },
        }),
      ],
    });
    const m = r.packets[0]!.measured;
    expect(m.window.delete_add_pct).toBeNull();
    expect(m.registry?.code_branches_on_kind).toBeNull();
    expect(m.registry?.code_branches_not_counted_why).toBe('no counter on this machine');
  });

  test('accepts a bare array and tolerates garbage', () => {
    expect(parseMetricsPackets([packet('p1')]).packets).toHaveLength(1);
    expect(parseMetricsPackets(null).packets).toHaveLength(0);
    expect(parseMetricsPackets({ data: [{ metadata: { measured: 'nope' } }] }).packets).toHaveLength(0);
  });
});

describe('newestMeasured / mergeLandings', () => {
  const older = parseMetricsPackets([
    packet('old', { at: '2026-09-13T05:00:00Z', head: 'c1', head_at: '2026-09-12T04:40:11Z' }, [L[2]!]),
  ]).packets[0]!;
  const newer = parseMetricsPackets([packet('new', { since: 'c1', backfill: false }, [L[0]!, L[1]!])]).packets[0]!;

  test('the newest packet is the one measured last, whatever order the API returned', () => {
    expect(newestMeasured([older, newer])?.id).toBe('new');
    expect(newestMeasured([newer, older])?.id).toBe('new');
    expect(newestMeasured([])).toBeNull();
  });

  test('landings are unioned across packets by sha, newest first', () => {
    const dup = parseMetricsPackets([packet('dup', {}, [L[1]!, L[2]!])]).packets[0]!;
    const all = mergeLandings([older, newer, dup]);
    expect(all.map((l) => l.sha)).toEqual(['c3', 'c2', 'c1']);
  });
});

describe('deleteAddPct', () => {
  test('rounds the way the script does, and is null when nothing was added', () => {
    expect(deleteAddPct(614635, 68780)).toBe(11);
    expect(deleteAddPct(58, 193)).toBe(333);
    expect(deleteAddPct(0, 5)).toBeNull();
    expect(deleteAddPct(0, 0)).toBeNull();
  });
});

describe('perDay', () => {
  test('folds landings into UTC days, oldest first, with the day ratio derived from its own sums', () => {
    const rows = perDay(L);
    expect(rows.map((r) => r.day)).toEqual(['2026-09-12', '2026-09-13']);
    const d13 = rows[1]!;
    expect(d13.landings).toBe(2);
    expect(d13.add).toBe(98);
    expect(d13.del).toBe(193);
    expect(d13.net).toBe(-95);
    expect(d13.testAdd).toBe(38);
    expect(d13.testDel).toBe(42);
    expect(d13.deleteAddPct).toBe(197);
    expect(rows[0]!.net).toBe(372);
  });

  test('an empty series is an empty table, not a row of zeros', () => {
    expect(perDay([])).toEqual([]);
  });
});

describe('recentWindow', () => {
  test('sums the landings inside the last N days ending at the newest landing', () => {
    const w = recentWindow(L, 1);
    expect(w.landings).toBe(2);
    expect(w.add).toBe(98);
    expect(w.del).toBe(193);
    expect(w.net).toBe(-95);
    expect(w.deleteAddPct).toBe(197);
    expect(w.from).toBe('2026-09-13');
    expect(w.to).toBe('2026-09-13');
    expect(w.days).toBe(1);
  });

  test('a window wider than the series covers the whole series', () => {
    const w = recentWindow(L, 30);
    expect(w.landings).toBe(3);
    expect(w.net).toBe(500 - 223);
  });

  test('no landings, no window', () => {
    const w = recentWindow([], 7);
    expect(w.landings).toBe(0);
    expect(w.deleteAddPct).toBeNull();
    expect(w.from).toBeNull();
  });
});

describe('registryRatio', () => {
  test('rows per code branch, one decimal; null when the branch count is unknown or zero', () => {
    expect(registryRatio(246, 3)).toBe(82);
    expect(registryRatio(10, 4)).toBe(2.5);
    expect(registryRatio(246, null)).toBeNull();
    expect(registryRatio(246, 0)).toBeNull();
  });
});

describe('headGapDays', () => {
  test('days between the head landing and the measurement, one decimal', () => {
    expect(headGapDays('2026-09-13T06:00:11Z', '2026-09-14T15:50:20Z')).toBe(1.4);
    expect(headGapDays(null, '2026-09-14T15:50:20Z')).toBeNull();
  });
});

describe('reading', () => {
  const [p] = parseMetricsPackets([packet('p1')]).packets;

  test('says what the numbers say — by volume from the recent window, by ratio from the registry row', () => {
    const r = reading(p!, L, 7, [p!]);
    // The three landings net +277 (500 added, 223 deleted): growth.
    expect(r.volume.verdict).toBe('more complex');
    expect(r.volume.text).toContain('+277');
    expect(r.volume.text).toContain('45%');
    // One measurement is a level, not a trend: the ratio has no direction yet.
    expect(r.ratio.verdict).toBe('first measurement');
    expect(r.ratio.text).toContain('246');
    expect(r.ratio.text).toContain('3');
    expect(r.ratio.text).toContain('82');
    expect(r.ratio.text).toContain('no earlier measurement');
  });

  test('the ratio trend compares the newest packet with the earliest one that counted branches', () => {
    const earlier = parseMetricsPackets([
      packet('e', { at: '2026-09-01T05:00:00Z', head: 'c0', registry: { rows: 200, code_branches_on_kind: 4 } }),
    ]).packets[0]!;
    const uncounted = parseMetricsPackets([
      packet('u', { at: '2026-08-20T05:00:00Z', head: 'cx', registry: { rows: 100, code_branches_on_kind: null } }),
    ]).packets[0]!;
    const up = reading(p!, L, 7, [uncounted, p!, earlier]);
    expect(up.ratio.verdict).toBe('simpler');
    expect(up.ratio.text).toContain('up from 50');
    expect(up.ratio.text).toContain('2026-09-01');
    expect(up.ratio.since).toEqual({ at: '2026-09-01T05:00:00Z', rowsPerBranch: 50 });
    const worse = parseMetricsPackets([packet('w', { registry: { rows: 246, code_branches_on_kind: 6 } })]).packets[0]!;
    expect(reading(worse, L, 7, [earlier, worse]).ratio.verdict).toBe('more complex');
    const same = parseMetricsPackets([packet('s', { registry: { rows: 200, code_branches_on_kind: 4 } })]).packets[0]!;
    expect(reading(same, L, 7, [earlier, same]).ratio.verdict).toBe('unchanged');
  });

  test('a shrinking window reads as simpler by volume; a flat one as unchanged', () => {
    const shrink = [landing({ sha: 'a', at: '2026-09-13T00:00:00Z', add: 10, del: 30 })];
    expect(reading(p!, shrink, 7, [p!]).volume.verdict).toBe('simpler');
    const flat = [landing({ sha: 'a', at: '2026-09-13T00:00:00Z', add: 10, del: 10 })];
    expect(reading(p!, flat, 7, [p!]).volume.verdict).toBe('unchanged');
    expect(reading(p!, [], 7, [p!]).volume.verdict).toBe('no landings');
  });

  test('an uncounted branch total is said in the words the row gives, never read as zero', () => {
    const q = parseMetricsPackets([
      packet('q', { registry: { rows: 246, code_branches_on_kind: null, code_branches_not_counted_why: 'no counter on this machine' } }),
    ]).packets[0]!;
    const r = reading(q, L, 7, [q]);
    expect(r.ratio.verdict).toBe('not counted');
    expect(r.ratio.text).toContain('no counter on this machine');
  });

});

describe('barGeometry', () => {
  test('positive nets rise from the baseline, negative ones hang below, scaled to the largest magnitude', () => {
    const g = barGeometry(perDay(L), 200, 100);
    expect(g.bars).toHaveLength(2);
    expect(g.w).toBe(200);
    const [up, down] = g.bars;
    expect(up!.net).toBe(372);
    expect(up!.y + up!.h).toBeCloseTo(g.baseline, 5);
    expect(down!.net).toBe(-95);
    expect(down!.y).toBeCloseTo(g.baseline, 5);
    expect(up!.h).toBeGreaterThan(down!.h);
    expect(g.max).toBe(372);
  });

  test('an empty table is a chart with a baseline and no bars', () => {
    const g = barGeometry([], 200, 100);
    expect(g.bars).toEqual([]);
    expect(g.max).toBe(0);
  });
});

describe('loadMetricsPackets', () => {
  test('reads the kind through the jobs API and parses once at the call site', async () => {
    let url = '';
    globalThis.fetch = (async (u: string | URL | Request) => {
      url = String(u);
      return new Response(JSON.stringify({ data: [packet('p1')], total: 1 }), { status: 200 });
    }) as unknown as typeof fetch;
    const r = await loadMetricsPackets(30);
    expect(url).toContain('/api/jobs?kind=maintenance-codebase-metrics');
    expect(url).toContain('limit=30');
    expect(r.kind).toBe('ready');
    if (r.kind === 'ready') expect(r.data.packets).toHaveLength(1);
  });

  test('a failed read is a failure, not an empty series', async () => {
    globalThis.fetch = (async () => new Response('nope', { status: 503 })) as unknown as typeof fetch;
    const r = await loadMetricsPackets(30);
    expect(r.kind).toBe('failed');
  });
});

describe('lastDays', () => {
  test('keeps the rows inside the last N days ending on the newest row, so the backfill does not flatten the chart', () => {
    const rows = perDay([landing({ sha: 'x', at: '2026-06-18T18:22:10Z', add: 241518 }), ...L]);
    expect(lastDays(rows, 30).map((r) => r.day)).toEqual(['2026-09-12', '2026-09-13']);
    expect(lastDays(rows, 1).map((r) => r.day)).toEqual(['2026-09-13']);
    expect(lastDays(rows, null)).toHaveLength(3);
    expect(lastDays([], 7)).toEqual([]);
  });
});

// Keeps the type in use so a rename is a compile error here too.
const _t: MetricsPacket | null = null;
void _t;
