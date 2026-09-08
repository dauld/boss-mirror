<script lang="ts">
  // Gateway latency dashboard — sub-page of /it/operate/audit.

  import Link from '@boss/web-kit/ui/Link.svelte';
  import { href } from '../../router';

  type EndpointSnapshot = {
    method: string;
    path: string;
    count: number;
    p50_ms: number;
    p95_ms: number;
    p99_ms: number;
    min_ms: number;
    max_ms: number;
    errors: number;
    client_errors?: number;
  };
  type PerfSnapshot = {
    taken_at: string;
    endpoints: ReadonlyArray<EndpointSnapshot>;
  };
  type SortKey =
    | 'p95' | 'p99' | 'p50' | 'count' | 'errors' | 'client_errors' | 'path';
  type State =
    | { kind: 'loading' }
    | { kind: 'ready'; snap: PerfSnapshot }
    | { kind: 'error'; message: string };

  const POLL_MS = 2000;

  let loadState: State = $state<State>({ kind: 'loading' });
  let sortKey = $state<SortKey>('p95');
  let sortDesc = $state(true);
  let paused = $state(false);

  $effect(() => {
    const p = paused;
    let cancelled = false;
    async function fetchOnce(): Promise<void> {
      try {
        const r = await fetch('/api/gateway/perf', {
          headers: { accept: 'application/json' },
          credentials: 'same-origin',
        });
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        const body = (await r.json()) as PerfSnapshot;
        if (!cancelled) loadState = { kind: 'ready', snap: body };
      } catch (e) {
        if (!cancelled) {
          loadState = {
            kind: 'error',
            message: e instanceof Error ? e.message : String(e),
          };
        }
      }
    }
    void fetchOnce();
    if (p) return () => {
      cancelled = true;
    };
    const id = window.setInterval(fetchOnce, POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  });

  async function resetHistograms(): Promise<void> {
    await fetch('/api/gateway/perf/reset', {
      method: 'POST',
      credentials: 'same-origin',
    });
    loadState = { kind: 'loading' };
  }

  function compare(a: EndpointSnapshot, b: EndpointSnapshot, k: SortKey): number {
    switch (k) {
      case 'path': return a.path.localeCompare(b.path);
      case 'count': return a.count - b.count;
      case 'errors': return a.errors - b.errors;
      case 'client_errors': return (a.client_errors ?? 0) - (b.client_errors ?? 0);
      case 'p50': return a.p50_ms - b.p50_ms;
      case 'p95': return a.p95_ms - b.p95_ms;
      case 'p99': return a.p99_ms - b.p99_ms;
    }
  }

  function arrowFor(k: SortKey): string {
    if (sortKey !== k) return '';
    return sortDesc ? ' ▼' : ' ▲';
  }

  function setSort(k: SortKey): void {
    if (k === sortKey) {
      sortDesc = !sortDesc;
    } else {
      sortKey = k;
      sortDesc = true;
    }
  }

  function latencyColor(ms: number): string {
    if (ms < 10) return 'rgba(34, 197, 94, 0.15)';
    if (ms < 50) return 'rgba(132, 204, 22, 0.18)';
    if (ms < 100) return 'rgba(250, 204, 21, 0.25)';
    if (ms < 250) return 'rgba(251, 146, 60, 0.3)';
    if (ms < 1000) return 'rgba(239, 68, 68, 0.35)';
    return 'rgba(153, 27, 27, 0.5)';
  }

  function methodColor(method: string): string {
    const map: Record<string, string> = {
      GET: '#16a34a',
      POST: '#2563eb',
      PUT: '#d97706',
      DELETE: '#dc2626',
      PATCH: '#9333ea',
    };
    return map[method] ?? '#6b7280';
  }

  const thBase = 'padding:8px 12px; font-size:13px; font-weight:600; color:var(--muted); user-select:none';
  const tdBase = 'padding:8px 12px; font-size:14px';

  let rows = $derived.by(() => {
    if (loadState.kind !== 'ready') return [] as EndpointSnapshot[];
    return [...loadState.snap.endpoints].sort((a, b) => {
      const cmp = compare(a, b, sortKey);
      return sortDesc ? -cmp : cmp;
    });
  });
  let totalRequests = $derived(rows.reduce((s, r) => s + r.count, 0));
</script>

<div class="theme-exec" style="padding:32px; max-width:1400px; margin:0 auto">
  <div class="exec-header" style="margin-bottom:24px">
    <div>
      <div class="exec-eyebrow">System Model · Observability</div>
      <h1 class="exec-title">Gateway latency</h1>
      <p style="color:var(--muted); margin:4px 0 0">
        Per-endpoint p50/p95/p99 since gateway start. Refreshes every 2 seconds.
        Buckets collapse path IDs (e.g. <code>/api/people/emp-005</code> →
        <code>/api/people/{'{id}'}</code>).
      </p>
    </div>
    <div style="display:flex; gap:12px; align-items:center">
      <Link to={href('/it/operate/audit')} className="btn">
        ← CTO
      </Link>
      <button type="button" class="btn" onclick={() => (paused = !paused)}>
        {paused ? '▶ Resume' : '⏸ Pause'}
      </button>
      <button type="button" class="btn" onclick={resetHistograms}>Reset</button>
    </div>
  </div>

  {#if loadState.kind === 'loading'}
    <div style="color:var(--muted)">Loading…</div>
  {:else if loadState.kind === 'error'}
    <div style="color:#dc2626">
      Failed to load perf snapshot: {loadState.message}
    </div>
  {:else}
    {@const snap = loadState.snap}
    <div style="color:var(--muted); font-size:13px; margin-bottom:12px">
      Snapshot taken <code>{snap.taken_at}</code> · {rows.length} endpoint buckets ·
      {totalRequests.toLocaleString()} total requests
    </div>
    <table class="perf-table" style="width:100%; border-collapse:collapse">
      <thead>
        <tr style="text-align:left; border-bottom:2px solid var(--border)">
          <th style={thBase}>Method</th>
          <th style={`${thBase}; cursor:pointer`} onclick={() => setSort('path')}>
            Path{arrowFor('path')}
          </th>
          <th style={`${thBase}; text-align:right; cursor:pointer`} onclick={() => setSort('count')}>
            Count{arrowFor('count')}
          </th>
          <th style={`${thBase}; text-align:right; cursor:pointer`} onclick={() => setSort('p50')}>
            p50 ms{arrowFor('p50')}
          </th>
          <th style={`${thBase}; text-align:right; cursor:pointer`} onclick={() => setSort('p95')}>
            p95 ms{arrowFor('p95')}
          </th>
          <th style={`${thBase}; text-align:right; cursor:pointer`} onclick={() => setSort('p99')}>
            p99 ms{arrowFor('p99')}
          </th>
          <th style={`${thBase}; text-align:right`}>max ms</th>
          <th style={`${thBase}; text-align:right; cursor:pointer`} onclick={() => setSort('client_errors')}>
            4xx{arrowFor('client_errors')}
          </th>
          <th style={`${thBase}; text-align:right; cursor:pointer`} onclick={() => setSort('errors')}>
            5xx{arrowFor('errors')}
          </th>
        </tr>
      </thead>
      <tbody>
        {#if rows.length === 0}
          <tr>
            <td colspan="9" style="padding:24px; color:var(--muted)">
              No traffic recorded yet. Exercise the app in another tab.
            </td>
          </tr>
        {/if}
        {#each rows as row (`${row.method}:${row.path}`)}
          {@const clientErrors = row.client_errors ?? 0}
          {@const methodBg = methodColor(row.method)}
          {@const p50bg = latencyColor(row.p50_ms)}
          {@const p95bg = latencyColor(row.p95_ms)}
          {@const p99bg = latencyColor(row.p99_ms)}
          <tr style="border-bottom:1px solid var(--border)">
            <td style={tdBase}>
              <span
                style={`font-size:11px; font-family:monospace; font-weight:600; color:white; background:${methodBg}; padding:2px 6px; border-radius:3px`}
              >
                {row.method}
              </span>
            </td>
            <td style={`${tdBase}; font-family:monospace; font-size:13px`}>{row.path}</td>
            <td style={`${tdBase}; text-align:right; font-variant-numeric:tabular-nums`}>
              {row.count.toLocaleString()}
            </td>
            <td
              style={`${tdBase}; text-align:right; font-variant-numeric:tabular-nums; background:${p50bg}; font-weight:${row.p50_ms >= 250 ? 700 : 500}`}
            >
              {row.p50_ms.toFixed(1)}
            </td>
            <td
              style={`${tdBase}; text-align:right; font-variant-numeric:tabular-nums; background:${p95bg}; font-weight:${row.p95_ms >= 250 ? 700 : 500}`}
            >
              {row.p95_ms.toFixed(1)}
            </td>
            <td
              style={`${tdBase}; text-align:right; font-variant-numeric:tabular-nums; background:${p99bg}; font-weight:${row.p99_ms >= 250 ? 700 : 500}`}
            >
              {row.p99_ms.toFixed(1)}
            </td>
            <td
              style={`${tdBase}; text-align:right; font-variant-numeric:tabular-nums; color:var(--muted)`}
            >
              {row.max_ms.toFixed(1)}
            </td>
            <td
              title="Client (4xx) responses. Persistent values here usually mean contract drift."
              style={`${tdBase}; text-align:right; font-variant-numeric:tabular-nums; color:${clientErrors > 0 ? '#b45309' : 'var(--muted)'}; font-weight:${clientErrors > 0 ? 600 : 400}`}
            >
              {clientErrors}
            </td>
            <td
              style={`${tdBase}; text-align:right; font-variant-numeric:tabular-nums; color:${row.errors > 0 ? '#dc2626' : 'var(--muted)'}; font-weight:${row.errors > 0 ? 600 : 400}`}
            >
              {row.errors}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>
