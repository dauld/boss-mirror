<script lang="ts">
  // Audit-log tail — CTO surface.
  //
  // Two modes:
  // - **Live (auto-refresh on)** subscribes to /api/events/stream
  //   (SSE). Server pushes each new audit_log row as it lands;
  //   no ordering loss vs. the prior poll-loop variant. Per the
  //   SSE policy doc (docs/design/sse-policy.md) this is "every
  //   event matters" → SSE-push.
  // - **Snapshot (auto-refresh off)** falls back to the explicit
  //   GET /api/events/tail with the current filters. Useful for
  //   pinning a window to inspect / share.
  //
  // Filters (source, kind substring, limit) compose into query
  // params for both modes. Single click on a row toggles the
  // inline JSON payload. Requires operator tier; non-operators
  // get a 403 from the backend which we render inline.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import FileAttachments from '../../content/FileAttachments.svelte';
  import { appNow, appToday } from '@boss/web-kit/sim-clock';
  import { formatDate } from '@boss/web-kit/ui/date';

  type AuditEntry = {
    event_id: string;
    timestamp: string;
    source: string;
    kind: string;
    payload: unknown;
  };
  type State =
    | { kind: 'loading' }
    | { kind: 'ready'; rows: ReadonlyArray<AuditEntry> }
    | { kind: 'error'; message: string };

  // Snapshot-mode poll; live mode uses SSE which arrives push-driven.
  const SNAPSHOT_RELOAD_MS = 5000;
  const LIMIT_CHOICES = [50, 100, 200, 500] as const;
  // Cap how many rows we keep in memory in live mode. New ones
  // arrive at the top; trim from the bottom past the cap.
  const LIVE_BUFFER_CAP = 500;

  let loadState: State = $state<State>({ kind: 'loading' });

  // Size and growth (168b3f25). David, 2026-09-02: "We need size and
  // growth stats on audit log to make sure it isn't growing
  // unsustainably." Read once on mount from /api/events/stats and
  // refreshed on a slow cadence — the numbers move by the hour, not
  // the second, and the SSE tail below already carries the live feel.
  type AuditStats = {
    total_rows: number;
    table_bytes: number;
    oldest_at: string | null;
    newest_at: string | null;
    rows_last_24h: number;
    rows_last_7d: number;
    per_day: ReadonlyArray<{ day: string; rows: number }>;
    top_kinds: ReadonlyArray<{ kind: string; rows: number }>;
  };
  type StatsState =
    | { kind: 'loading' }
    | { kind: 'ready'; stats: AuditStats }
    | { kind: 'error'; message: string };
  const STATS_RELOAD_MS = 60_000;
  let statsState = $state<StatsState>({ kind: 'loading' });

  async function loadStats(): Promise<void> {
    try {
      const r = await fetch('/api/events/stats', { headers: { Accept: 'application/json' } });
      if (!r.ok) {
        statsState = { kind: 'error', message: `HTTP ${r.status}` };
        return;
      }
      statsState = { kind: 'ready', stats: (await r.json()) as AuditStats };
    } catch (e) {
      statsState = { kind: 'error', message: e instanceof Error ? e.message : String(e) };
    }
  }

  function humanBytes(n: number): string {
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let v = n;
    let i = 0;
    while (v >= 1024 && i < units.length - 1) {
      v /= 1024;
      i += 1;
    }
    return `${v < 10 && i > 0 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
  }

  function perDayMax(days: ReadonlyArray<{ rows: number }>): number {
    return days.reduce((m, d) => (d.rows > m ? d.rows : m), 1);
  }

  $effect(() => {
    void loadStats();
    const t = setInterval(() => void loadStats(), STATS_RELOAD_MS);
    return () => clearInterval(t);
  });
  let sourceFilter = $state('');
  let kindFilter = $state('');
  let limit = $state<(typeof LIMIT_CHOICES)[number]>(100);
  // Provenance filter. Defaults to `real`: the audit log was 89%
  // simulated when this landed (328,255 of 370,033 rows), so an
  // unfiltered page is nine-tenths brewery traffic and the operator
  // events it buries are the reason anyone opens this page. `all`
  // stays one click away — the default is a lens, not a lie.
  let provenance = $state<'real' | 'sim' | 'all'>('real');
  let autoRefresh = $state(true);
  let expanded = $state<string | null>(null);
  let lastFetched = $state<Date | null>(null);

  // Download-on-demand state. The audit_log is SIM-dated, so the default
  // window comes from sim time (appNow/appToday), NOT wallclock — a
  // wallclock default lands after every event and exports an empty file.
  // Populated when the panel opens (the sim clock is loaded by then); the
  // operator can override either date. The export inherits the page's
  // source + kind filters; the browser saves via Content-Disposition.
  function isoDate(d: Date): string {
    return d.toISOString().slice(0, 10);
  }
  let downloadOpen = $state(false);
  let downloadFrom = $state<string>('');
  let downloadTo = $state<string>('');

  function toggleDownload(): void {
    if (!downloadOpen) {
      // Default window: 7 sim-days back to the sim's "today".
      downloadFrom = isoDate(new Date(appNow().getTime() - 7 * 86400_000));
      downloadTo = appToday();
    }
    downloadOpen = !downloadOpen;
  }

  function startDownload(): void {
    const params = new URLSearchParams();
    const src = sourceFilter.trim();
    const knd = kindFilter.trim();
    if (src) params.set('source', src);
    if (knd) params.set('kind', knd);
    // The export honours the same lens as the view. A download that
    // silently disagreed with the table above it would be worse than
    // no export.
    if (provenance !== 'all') params.set('simulated', provenance);
    // Convert dates to half-open RFC-3339 window. `until` is
    // exclusive on the backend, so we pass to-date+1day to include
    // the entire to-date.
    if (downloadFrom) {
      params.set('since', `${downloadFrom}T00:00:00Z`);
    }
    if (downloadTo) {
      // until is exclusive; bump by 1 day to make the from..to
      // range inclusive on the to side.
      const t = new Date(`${downloadTo}T00:00:00Z`);
      t.setUTCDate(t.getUTCDate() + 1);
      params.set('until', t.toISOString());
    }
    // The export endpoint streams up to 50k rows; for large windows
    // the operator narrows the range. Direct navigation triggers the
    // browser download dialog via the response's Content-Disposition.
    window.location.href = `/api/events/export?${params.toString()}`;
    downloadOpen = false;
  }

  $effect(() => {
    // Re-run whenever filters or live-mode flag change.
    const src = sourceFilter.trim();
    const knd = kindFilter.trim();
    const lim = limit;
    const prov = provenance;
    const auto = autoRefresh;

    let cancelled = false;

    async function fetchSnapshot(): Promise<void> {
      const params = new URLSearchParams();
      if (src) params.set('source', src);
      if (knd) params.set('kind', knd);
      if (prov !== 'all') params.set('simulated', prov);
      params.set('limit', String(lim));
      try {
        const r = await fetch(`/api/events/tail?${params.toString()}`, {
          credentials: 'same-origin',
          headers: { accept: 'application/json' },
        });
        if (!r.ok) {
          const msg = await r.text();
          throw new Error(`HTTP ${r.status}${msg ? `: ${msg}` : ''}`);
        }
        const body = (await r.json()) as ReadonlyArray<AuditEntry>;
        if (!cancelled) {
          loadState = { kind: 'ready', rows: body };
          lastFetched = new Date();
        }
      } catch (e) {
        if (!cancelled) {
          loadState = {
            kind: 'error',
            message: e instanceof Error ? e.message : String(e),
          };
        }
      }
    }

    // Always start with one snapshot so the page renders the
    // recent window immediately, regardless of mode.
    void fetchSnapshot();

    if (!auto) {
      // Snapshot mode — explicit reload only via the user
      // tapping the filter inputs (which retriggers this $effect).
      // No interval timer.
      return () => {
        cancelled = true;
      };
    }

    // Live mode — SSE pushes new rows as they land. Browser's
    // EventSource auto-reconnects on transient blips. On a hard
    // failure (route 404 on older deploys) onerror fires with
    // CLOSED state; fall back to a 5s snapshot poll.
    const params = new URLSearchParams();
    if (src) params.set('source', src);
    if (knd) params.set('kind', knd);
    let es: EventSource | null = null;
    let pollFallbackId: number | null = null;
    try {
      es = new EventSource(`/api/events/stream?${params.toString()}`);
      es.onmessage = (ev) => {
        if (cancelled) return;
        try {
          const entry = JSON.parse(ev.data) as AuditEntry;
          // Prepend new row, dedupe, cap.
          if (loadState.kind === 'ready') {
            const existing = loadState.rows;
            if (!existing.some((r) => r.event_id === entry.event_id)) {
              const next = [entry, ...existing].slice(0, LIVE_BUFFER_CAP);
              loadState = { kind: 'ready', rows: next };
            }
          } else {
            loadState = { kind: 'ready', rows: [entry] };
          }
          lastFetched = new Date();
        } catch {
          // Drop malformed frame.
        }
      };
      es.onerror = () => {
        if (es && es.readyState === EventSource.CLOSED) {
          es.close();
          es = null;
          if (pollFallbackId === null) {
            pollFallbackId = window.setInterval(fetchSnapshot, SNAPSHOT_RELOAD_MS);
          }
        }
      };
    } catch {
      pollFallbackId = window.setInterval(fetchSnapshot, SNAPSHOT_RELOAD_MS);
    }

    return () => {
      cancelled = true;
      es?.close();
      if (pollFallbackId !== null) window.clearInterval(pollFallbackId);
    };
  });

  // Distinct sources present in the current batch — drives a quick
  // dropdown without an extra API.
  let knownSources = $derived.by(() => {
    if (loadState.kind !== 'ready') return [] as string[];
    const set = new Set<string>();
    for (const r of loadState.rows) set.add(r.source);
    return [...set].sort();
  });

  function formatTimestamp(iso: string): string {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    // HH:MM:SS.mmm on one row, full date hover via title
    const hh = String(d.getHours()).padStart(2, '0');
    const mm = String(d.getMinutes()).padStart(2, '0');
    const ss = String(d.getSeconds()).padStart(2, '0');
    const ms = String(d.getMilliseconds()).padStart(3, '0');
    return `${hh}:${mm}:${ss}.${ms}`;
  }

  function formatFullTimestamp(iso: string): string {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toISOString();
  }

  function toggleRow(id: string): void {
    expanded = expanded === id ? null : id;
  }
</script>

<div class="events">
  <PageHeader
    eyebrow="IT · Event stream"
    title="Audit Log"
    subtitle="Live tail of every domain event. Operator tier only."
  />

  <Section title="Size and growth" wide>
    {#if statsState.kind === 'loading'}
      <p class="events-stats-note">Measuring the log…</p>
    {:else if statsState.kind === 'error'}
      <p class="events-stats-note">Stats unavailable: {statsState.message}</p>
    {:else}
      {@const st = statsState.stats}
      <div class="events-stats">
        <div class="events-stat">
          <span class="events-stat-label">Rows</span>
          <span class="events-stat-value">{st.total_rows.toLocaleString()}</span>
        </div>
        <div class="events-stat">
          <span class="events-stat-label">On disk</span>
          <span class="events-stat-value">{humanBytes(st.table_bytes)}</span>
        </div>
        <div class="events-stat">
          <span class="events-stat-label">Last 24h</span>
          <span class="events-stat-value">{st.rows_last_24h.toLocaleString()}</span>
        </div>
        <div class="events-stat">
          <span class="events-stat-label">Per day, 7d avg</span>
          <span class="events-stat-value">{Math.round(st.rows_last_7d / 7).toLocaleString()}</span>
        </div>
        <div class="events-stat">
          <span class="events-stat-label">Since</span>
          <span class="events-stat-value">{st.oldest_at ? formatDate(st.oldest_at) : '—'}</span>
        </div>
      </div>
      <div class="events-growth">
        <div class="events-per-day">
          <h4>Rows per day, last 30 days</h4>
          {#each st.per_day as d (d.day)}
            <div class="events-day">
              <span class="events-day-label">{d.day.slice(5)}</span>
              <span class="events-day-bar" style="width: {Math.max(2, Math.round((100 * d.rows) / perDayMax(st.per_day)))}%"></span>
              <span class="events-day-rows">{d.rows.toLocaleString()}</span>
            </div>
          {/each}
        </div>
        <div class="events-top-kinds">
          <h4>Busiest kinds, last 30 days</h4>
          <table>
            <tbody>
              {#each st.top_kinds as k (k.kind)}
                <tr><td class="events-kind">{k.kind}</td><td class="events-kind-rows">{k.rows.toLocaleString()}</td></tr>
              {/each}
            </tbody>
          </table>
        </div>
      </div>
    {/if}
  </Section>

  <Section title="Filters" wide>
      <div class="events-filters">
        <label class="events-filter">
          <span>Source</span>
          <input
            list="events-sources"
            bind:value={sourceFilter}
            placeholder="e.g. jobs, assets"
          />
          <datalist id="events-sources">
            {#each knownSources as s (s)}
              <option value={s}></option>
            {/each}
          </datalist>
        </label>
        <label class="events-filter">
          <span>Kind contains</span>
          <input
            bind:value={kindFilter}
            placeholder="e.g. step, invoice"
          />
        </label>
        <label class="events-filter">
          <span>Provenance</span>
          <select bind:value={provenance}>
            <option value="real">Real only</option>
            <option value="sim">Simulated only</option>
            <option value="all">All</option>
          </select>
        </label>
        <label class="events-filter">
          <span>Limit</span>
          <select bind:value={limit}>
            {#each LIMIT_CHOICES as n (n)}
              <option value={n}>{n}</option>
            {/each}
          </select>
        </label>
        <label class="events-filter events-auto">
          <input type="checkbox" bind:checked={autoRefresh} />
          <span>Live (SSE)</span>
        </label>
        <button
          type="button"
          class="events-download-btn"
          onclick={toggleDownload}
          title="Export matching events as a JSON Lines file"
        >
          Download ⤓
        </button>
        {#if lastFetched}
          <span class="events-freshness">
            Last: {formatTimestamp(lastFetched.toISOString())}
          </span>
        {/if}
      </div>

      {#if downloadOpen}
        <div class="events-download-panel">
          <div class="events-download-row">
            <label class="events-filter">
              <span>From</span>
              <input type="date" bind:value={downloadFrom} max={downloadTo} />
            </label>
            <label class="events-filter">
              <span>To</span>
              <input type="date" bind:value={downloadTo} min={downloadFrom} />
            </label>
            <button type="button" class="events-download-go" onclick={startDownload}>
              Save .jsonl
            </button>
            <button
              type="button"
              class="events-download-cancel"
              onclick={() => (downloadOpen = false)}
            >
              Cancel
            </button>
          </div>
          <p class="events-download-hint">
            Exports up to 50,000 events matching the current source + kind filters
            in the window above as JSON Lines (one event per line — parseable by
            <code>jq</code>, log forwarders, and most analytics tools).
            Narrow the window for large ranges.
          </p>
        </div>
      {/if}
  </Section>

  <Section title="Stream" wide>
      {#if loadState.kind === 'loading'}
        <p class="empty">Loading…</p>
      {:else if loadState.kind === 'error'}
        <p class="empty">Failed to load: {loadState.message}</p>
      {:else if loadState.rows.length === 0}
        <p class="empty">No events match these filters.</p>
      {:else}
        <table class="data-table data-table-striped events-table">
          <thead>
            <tr>
              <th style="width:10ch">Time</th>
              <th style="width:12ch">Source</th>
              <th>Kind</th>
            </tr>
          </thead>
          <tbody>
            {#each loadState.rows as row (row.event_id)}
              {@const isOpen = expanded === row.event_id}
              <tr
                class="events-row{isOpen ? ' events-row-open' : ''}"
                onclick={() => toggleRow(row.event_id)}
              >
                <td
                  class="mono"
                  title={formatFullTimestamp(row.timestamp)}
                >{formatTimestamp(row.timestamp)}</td>
                <td class="mono">{row.source}</td>
                <td class="mono">{row.kind}</td>
              </tr>
              {#if isOpen}
                <tr class="events-payload-row">
                  <td colspan="3">
                    <pre class="events-payload">{JSON.stringify(row.payload, null, 2)}</pre>
                    <div class="events-event-id">event_id: <code>{row.event_id}</code></div>
                    <!--
                      Event-attached files render inline next to the
                      event that produced them. Per design Q6 events
                      get attachments — a vendor-invoice PDF tied to
                      a `bill.received` event, a signed scan tied to
                      an `acknowledgment` event. Empty by default
                      until someone uploads.
                    -->
                    <div class="events-attachments">
                      <FileAttachments targetKind="event" targetId={row.event_id} canEdit={false} />
                    </div>
                  </td>
                </tr>
              {/if}
            {/each}
          </tbody>
        </table>
      {/if}
  </Section>
</div>

<style>
  .events-filters {
    display: flex;
    flex-wrap: wrap;
    gap: 16px;
    align-items: flex-end;
  }
  .events-filter {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
  }
  .events-filter span {
    color: var(--muted, #64748b);
    font-weight: 500;
  }
  .events-filter input,
  .events-filter select {
    min-width: 160px;
    padding: 4px 6px;
    border: 1px solid var(--border, #d1d5db);
    border-radius: 4px;
    background: white;
  }
  .events-auto {
    flex-direction: row;
    align-items: center;
    gap: 6px;
  }
  .events-auto span {
    font-size: 13px;
    color: inherit;
  }
  .events-freshness {
    font-size: 12px;
    color: var(--muted, #64748b);
    margin-left: auto;
  }
  .events-download-btn {
    padding: 6px 12px;
    font-size: 12px;
    font-weight: 500;
    background: white;
    color: inherit;
    border: 1px solid var(--border, #d1d5db);
    border-radius: 4px;
    cursor: pointer;
  }
  .events-download-btn:hover {
    background: var(--accent-bg, #eff6ff);
  }
  .events-download-panel {
    margin-top: 12px;
    padding: 10px 12px;
    background: var(--accent-bg, #eff6ff);
    border: 1px solid var(--border, #d1d5db);
    border-radius: 4px;
  }
  .events-download-row {
    display: flex;
    gap: 12px;
    align-items: flex-end;
    flex-wrap: wrap;
  }
  .events-download-go {
    padding: 6px 12px;
    font-size: 12px;
    font-weight: 600;
    background: #1d4ed8;
    color: white;
    border: 1px solid #1e40af;
    border-radius: 4px;
    cursor: pointer;
  }
  .events-download-go:hover {
    background: #1e40af;
  }
  .events-download-cancel {
    padding: 6px 10px;
    font-size: 12px;
    background: transparent;
    color: var(--muted, #64748b);
    border: none;
    cursor: pointer;
  }
  .events-download-hint {
    margin: 8px 0 0;
    font-size: 11px;
    color: var(--muted, #64748b);
    line-height: 1.5;
  }
  .events-download-hint code {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    background: rgba(15, 23, 42, 0.08);
    padding: 1px 4px;
    border-radius: 2px;
  }
  .events-table tbody tr.events-row {
    cursor: pointer;
  }
  .events-table tbody tr.events-row:hover {
    background: var(--accent-bg, #eff6ff);
  }
  .events-table tbody tr.events-row-open {
    background: var(--accent-bg, #eff6ff);
    font-weight: 500;
  }
  .events-table .mono {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 12px;
  }
  .events-payload-row td {
    background: #0b1020;
    color: #e2e8f0;
    padding: 0;
  }
  .events-payload {
    margin: 0;
    padding: 12px 16px;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 12px;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 420px;
    overflow: auto;
  }
  .events-attachments {
    margin-top: 12px;
    padding-top: 12px;
    border-top: 1px dashed var(--border);
  }
  .events-event-id {
    padding: 4px 16px 10px;
    font-size: 11px;
    color: #94a3b8;
  }

  .events-stats-note { margin: 0; opacity: 0.8; }
  .events-stats { display: flex; flex-wrap: wrap; gap: 20px; margin-bottom: 12px; }
  .events-stat { display: flex; flex-direction: column; min-width: 110px; }
  .events-stat-label { font-size: 12px; opacity: 0.7; }
  .events-stat-value { font-size: 20px; font-variant-numeric: tabular-nums; }
  .events-growth { display: grid; grid-template-columns: minmax(0, 2fr) minmax(0, 1fr); gap: 20px; }
  @media (max-width: 800px) { .events-growth { grid-template-columns: minmax(0, 1fr); } }
  .events-growth h4 { margin: 0 0 6px; font-size: 13px; opacity: 0.8; }
  .events-day { display: grid; grid-template-columns: 44px minmax(0, 1fr) 64px; align-items: center; gap: 8px; font-size: 12px; line-height: 1.5; }
  .events-day-label { opacity: 0.7; font-variant-numeric: tabular-nums; }
  .events-day-bar { display: block; height: 8px; border-radius: 2px; background: currentColor; opacity: 0.45; }
  .events-day-rows { text-align: right; font-variant-numeric: tabular-nums; }
  .events-top-kinds table { width: 100%; border-collapse: collapse; font-size: 12px; }
  .events-top-kinds td { padding: 2px 0; }
  .events-kind { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 0; }
  .events-kind-rows { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; padding-left: 8px; }
</style>
