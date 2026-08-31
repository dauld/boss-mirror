<script lang="ts">
  // Stage durations per RUN — the last N Jobs of a pipeline kind with
  // each hop's wall-clock duration (backlog a5096c8f: "even a table
  // of the last N trains with their four stage durations answers
  // 'where does a change wait longest'"). The Bottlenecks page
  // aggregates; this is the departure log those aggregates summarise.
  //
  // Wall clock throughout: durations come from audit_log.created_at
  // server-side, and recency from jobs.created_at — never the
  // sim-calendar opened_on (see FlowPage's note on the two clocks).
  import { onMount } from 'svelte';

  type RunStage = Readonly<{ slug: string; seconds: number | null }>;
  type StageRun = Readonly<{
    job_id: string;
    title: string;
    created_at: string;
    status: string;
    stages: ReadonlyArray<RunStage>;
  }>;
  type AggStage = Readonly<{ slug: string; completed: number; p50_seconds: number }>;

  const KINDS = [
    { id: 'pr-train', label: 'Trains' },
    { id: 'ship-a-change', label: 'Changes' },
  ] as const;

  let kindId = $state<string>('pr-train');
  let runs = $state<ReadonlyArray<StageRun>>([]);
  let agg = $state<ReadonlyArray<AggStage>>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  /// Defensive by contract: the smoke crawl serves `[]` for unknown
  /// endpoints, and a real deploy can race an old views binary — a
  /// non-object body or missing arrays must render as "no runs", not
  /// crash the page.
  function readRuns(v: unknown): ReadonlyArray<StageRun> {
    if (typeof v !== 'object' || v === null || !Array.isArray((v as { runs?: unknown }).runs)) {
      return [];
    }
    return ((v as { runs: unknown[] }).runs).flatMap((r) => {
      if (typeof r !== 'object' || r === null) return [];
      const o = r as Record<string, unknown>;
      if (typeof o.job_id !== 'string' || typeof o.title !== 'string') return [];
      const stages = Array.isArray(o.stages)
        ? (o.stages as unknown[]).flatMap((s) => {
            if (typeof s !== 'object' || s === null) return [];
            const t = s as Record<string, unknown>;
            if (typeof t.slug !== 'string') return [];
            return [{ slug: t.slug, seconds: typeof t.seconds === 'number' ? t.seconds : null }];
          })
        : [];
      return [{
        job_id: o.job_id,
        title: o.title,
        created_at: typeof o.created_at === 'string' ? o.created_at : '',
        status: typeof o.status === 'string' ? o.status : '',
        stages,
      }];
    });
  }

  function readAgg(v: unknown): ReadonlyArray<AggStage> {
    if (typeof v !== 'object' || v === null || !Array.isArray((v as { stages?: unknown }).stages)) {
      return [];
    }
    return ((v as { stages: unknown[] }).stages).flatMap((s) => {
      if (typeof s !== 'object' || s === null) return [];
      const o = s as Record<string, unknown>;
      if (typeof o.slug !== 'string' || typeof o.p50_seconds !== 'number') return [];
      return [{
        slug: o.slug,
        completed: typeof o.completed === 'number' ? o.completed : 0,
        p50_seconds: o.p50_seconds,
      }];
    });
  }

  /// Column order: each run lists its stages in spec order, so a
  /// first-encounter union over runs preserves the pipeline's shape
  /// even when newer runs have not reached the later stages yet.
  let columns = $derived.by(() => {
    const seen = new Set<string>();
    const cols: string[] = [];
    for (const r of runs) {
      for (const s of r.stages) {
        if (!seen.has(s.slug)) {
          seen.add(s.slug);
          cols.push(s.slug);
        }
      }
    }
    return cols;
  });

  function cell(run: StageRun, slug: string): string {
    const s = run.stages.find((x) => x.slug === slug);
    if (!s) return '';
    if (s.seconds === null) return run.status === 'closed' ? '—' : '…';
    return fmt(s.seconds);
  }

  function p50(slug: string): string {
    const a = agg.find((x) => x.slug === slug);
    return a ? fmt(a.p50_seconds) : '—';
  }

  function fmt(seconds: number): string {
    if (seconds < 60) return `${Math.round(seconds)}s`;
    const mins = Math.round(seconds / 60);
    if (mins < 60) return `${mins}m`;
    const h = Math.floor(mins / 60);
    const m = mins % 60;
    if (h < 24) return m ? `${h}h ${m}m` : `${h}h`;
    const d = Math.floor(h / 24);
    return `${d}d ${h % 24}h`;
  }

  function when(iso: string): string {
    const t = Date.parse(iso);
    if (Number.isNaN(t)) return '';
    return new Date(t).toISOString().slice(5, 16).replace('T', ' ');
  }

  async function load(): Promise<void> {
    loading = true;
    try {
      const [runsResp, aggResp] = await Promise.all([
        fetch(`/api/views/stage-runs/${kindId}?limit=10`),
        fetch(`/api/views/stage-durations/${kindId}?days=7`),
      ]);
      if (!runsResp.ok) throw new Error(`stage-runs: HTTP ${runsResp.status}`);
      runs = readRuns(await runsResp.json());
      agg = aggResp.ok ? readAgg(await aggResp.json()) : [];
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  function pick(id: string): void {
    kindId = id;
    void load();
  }

  onMount(load);
</script>

<section class="runs-card">
  <div class="runs-head">
    <h2 class="runs-h">Stage durations — recent runs</h2>
    <div class="runs-tabs">
      {#each KINDS as k (k.id)}
        <button
          type="button"
          class="runs-tab"
          class:runs-tab-on={kindId === k.id}
          onclick={() => pick(k.id)}
        >{k.label}</button>
      {/each}
    </div>
  </div>

  {#if loading}
    <p class="runs-msg">Reading the log…</p>
  {:else if error}
    <p class="runs-msg runs-err">{error}</p>
  {:else if runs.length === 0}
    <p class="runs-msg">No runs of this kind yet.</p>
  {:else}
    <div class="runs-scroll">
      <table class="runs-table">
        <thead>
          <tr>
            <th class="runs-left">run</th>
            {#each columns as c (c)}<th>{c}</th>{/each}
          </tr>
        </thead>
        <tbody>
          {#each runs as r (r.job_id)}
            <tr>
              <td class="runs-left">
                <a class="runs-link" href="/jobs/{r.job_id}">{r.title}</a>
                <span class="runs-when">{when(r.created_at)}</span>
              </td>
              {#each columns as c (c)}<td class="runs-num">{cell(r, c)}</td>{/each}
            </tr>
          {/each}
        </tbody>
        <tfoot>
          <tr>
            <td class="runs-left runs-foot">p50 (7d)</td>
            {#each columns as c (c)}<td class="runs-num runs-foot">{p50(c)}</td>{/each}
          </tr>
        </tfoot>
      </table>
    </div>
    <p class="runs-note">
      A cell is one hop's wall-clock time from ready to done; … is still
      waiting, — never ran. Where a column's numbers dwarf the others,
      that is where a change waits.
    </p>
  {/if}
</section>

<style>
  .runs-card {
    border: 1px solid var(--border, #e7e5e4);
    border-radius: 8px;
    padding: 14px 16px;
    background: var(--card, #fff);
    margin-bottom: 12px;
  }
  .runs-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 10px;
    flex-wrap: wrap;
    margin-bottom: 10px;
  }
  .runs-h {
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--text-dim, #78716c);
    margin: 0;
    font-weight: 600;
  }
  .runs-tabs {
    display: flex;
    gap: 6px;
  }
  .runs-tab {
    font: inherit;
    font-size: 12px;
    padding: 2px 9px;
    border-radius: 4px;
    border: 1px solid var(--border, #e7e5e4);
    background: var(--bg, #f5f5f4);
    color: inherit;
    cursor: pointer;
  }
  .runs-tab-on {
    background: #0f766e;
    border-color: #0f766e;
    color: #fff;
  }
  .runs-scroll {
    overflow-x: auto;
  }
  .runs-table {
    border-collapse: collapse;
    width: 100%;
    font-size: 12px;
  }
  .runs-table th {
    text-align: right;
    font-weight: 600;
    color: var(--text-dim, #78716c);
    padding: 4px 8px;
    border-bottom: 1px solid var(--border, #e7e5e4);
    white-space: nowrap;
  }
  .runs-table td {
    padding: 5px 8px;
    border-bottom: 1px solid var(--border, #f5f5f4);
    white-space: nowrap;
  }
  .runs-left {
    text-align: left !important;
    max-width: 26rem;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .runs-num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .runs-link {
    color: inherit;
    text-decoration: none;
  }
  .runs-link:hover {
    text-decoration: underline;
  }
  .runs-when {
    margin-left: 8px;
    font-size: 11px;
    color: var(--text-dim, #a8a29e);
    font-variant-numeric: tabular-nums;
  }
  .runs-foot {
    color: var(--text-dim, #78716c);
    border-top: 1px solid var(--border, #e7e5e4);
    font-weight: 600;
  }
  .runs-note {
    margin: 8px 0 0;
    font-size: 11px;
    color: var(--text-dim, #a8a29e);
    line-height: 1.5;
  }
  .runs-msg {
    color: var(--text-dim, #78716c);
    font-size: 13px;
  }
  .runs-err {
    color: #b91c1c;
  }
</style>
