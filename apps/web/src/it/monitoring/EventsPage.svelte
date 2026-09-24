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
  // Filters (source, kind substring, actor, limit, and a since/until
  // window) compose into query params for both modes. Single click on a row toggles the
  // inline JSON payload. Requires operator tier; non-operators
  // get a 403 from the backend which we render inline.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import FileAttachments from '../../content/FileAttachments.svelte';
  import { packetOf, windowBound } from './auditRetro';
  import { appNow, appToday } from '@boss/web-kit/sim-clock';
  import { formatDate } from '@boss/web-kit/ui/date';
  import { actorOf, knownActors as actorsIn } from './auditActor';
  import { readExport } from './auditExport';

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

  // What the live stream is doing, said in one line (backlog 260879f5,
  // page audit 65a273d5). A dead stream used to look exactly like a
  // quiet log: the server swallowed a failed read and held the
  // connection open, and the page acted only on CLOSED, falling to the
  // poll without a word. The server now sends `event: failed` naming the
  // error and ends; each state below paints its own line.
  type LiveState =
    | { kind: 'off' }
    | { kind: 'connecting' }
    | { kind: 'live' }
    | { kind: 'reconnecting' }
    | { kind: 'polling'; reason: string }
    | { kind: 'windowed' };
  let liveState = $state<LiveState>({ kind: 'off' });

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
  // The window a retro or an incident reads (backlog 62a0bbee). The
  // tail is the newest `limit` rows, at most 500 — about ten minutes of
  // log — so without a window nothing older was readable here, only
  // exportable. Both are `datetime-local` values in the zone the rows
  // are painted in; auditRetro.ts turns them into the UTC instants the
  // tail takes (`since` inclusive, `until` exclusive). An Until closes
  // the window, so the live stream is not opened while one is set.
  let sinceInput = $state('');
  let untilInput = $state('');
  let sourceFilter = $state('');
  let kindFilter = $state('');
  // Who acted (backlog 03f79eca) — an EXACT match on the payload's
  // `_actor`, applied by the server in tail, stream and export alike.
  let actorFilter = $state('');
  let limit = $state<(typeof LIMIT_CHOICES)[number]>(100);
  // Provenance filter, applied by the server in tail, stream and export
  // alike (the stream and the export ignored it until 2026-09-24,
  // backlog 34ea2ae0). It defaulted to `real` when the log was 89%
  // simulated (328,255 of 370,033 rows). It defaults to `all` since
  // 34ea2ae0 landed, decided under page audit 65a273d5: the brewery sim
  // is parked and simulated traffic belongs to the playground, so this
  // instance's log is real work and `real` hid nearly nothing while
  // naming a filter. `real` and `sim` stay one click away.
  let provenance = $state<'real' | 'sim' | 'all'>('all');
  let autoRefresh = $state(true);
  let expanded = $state<string | null>(null);
  let lastFetched = $state<Date | null>(null);

  // Download-on-demand state. The audit_log is SIM-dated, so the default
  // window comes from sim time (appNow/appToday), NOT wallclock — a
  // wallclock default lands after every event and exports an empty file.
  // Populated when the panel opens (the sim clock is loaded by then); the
  // operator can override either date. The export inherits the page's
  // source, kind, actor and provenance filters; the page reads the whole
  // body with fetch and saves it as a Blob, taking only the file NAME
  // from Content-Disposition (4630ebc0 — it was a navigation the browser
  // saved by that header).
  function isoDate(d: Date): string {
    return d.toISOString().slice(0, 10);
  }
  let downloadOpen = $state(false);
  let downloadFrom = $state<string>('');
  let downloadTo = $state<string>('');
  // How the last Save .jsonl ended, said in one line under the panel
  // (4630ebc0) — it used to end off the page or nowhere.
  type ExportState =
    | { kind: 'idle' }
    | { kind: 'reading' }
    | { kind: 'saved'; message: string }
    | { kind: 'failed'; message: string };
  let exportState = $state<ExportState>({ kind: 'idle' });

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
    const act = actorFilter.trim();
    if (act) params.set('actor', act);
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
    // the operator narrows the range. It is READ, not navigated to
    // (4630ebc0): a navigation replaced the app with a refusal's raw
    // body, and kept a short file when the stream broke off after its
    // 200. auditExport.ts names every ending; only a whole body saves.
    void saveExport(`/api/events/export?${params.toString()}`);
  }

  async function saveExport(url: string): Promise<void> {
    exportState = { kind: 'reading' };
    const out = await readExport(fetch, url);
    if (out.kind === 'failed') {
      // The panel stays open, holding the window, for a retry.
      exportState = { kind: 'failed', message: out.message };
      return;
    }
    const href = URL.createObjectURL(out.blob);
    const a = document.createElement('a');
    a.href = href;
    a.download = out.filename;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(href), 0);
    exportState = {
      kind: 'saved',
      message: `Saved ${out.filename}: ${out.events.toLocaleString()} ${out.events === 1 ? 'event' : 'events'}.`,
    };
    downloadOpen = false;
  }

  $effect(() => {
    // Re-run whenever filters or live-mode flag change.
    const src = sourceFilter.trim();
    const knd = kindFilter.trim();
    const act = actorFilter.trim();
    const lim = limit;
    const prov = provenance;
    const auto = autoRefresh;
    const since = windowBound(sinceInput);
    const until = windowBound(untilInput);

    let cancelled = false;
    // Frames this run applied before its snapshot answered (697f9f87).
    // The tail read and the stream start together below, so either may
    // answer first, and the server anchors the stream at MAX(id) when it
    // connects — a frame that beats the snapshot may be a row the
    // snapshot never carries. Replacing the rows with the snapshot lost
    // it. So the snapshot MERGES: the early frames not already in it go
    // on top, exactly where they would sit had the snapshot come first.
    // Opening the stream only after the snapshot paints was the other
    // fix, and a worse one: rows landing between the snapshot's query
    // and the stream's anchor would reach neither.
    let early: ReadonlyArray<AuditEntry> = [];
    let snapshotPainted = false;

    async function fetchSnapshot(): Promise<void> {
      const params = new URLSearchParams();
      if (src) params.set('source', src);
      if (knd) params.set('kind', knd);
      if (act) params.set('actor', act);
      if (prov !== 'all') params.set('simulated', prov);
      if (since) params.set('since', since);
      if (until) params.set('until', until);
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
          const inSnapshot = new Set(body.map((r) => r.event_id));
          const rows = [...early.filter((r) => !inSnapshot.has(r.event_id)), ...body];
          loadState = { kind: 'ready', rows: rows.slice(0, LIVE_BUFFER_CAP) };
          early = [];
          snapshotPainted = true;
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

    if (!auto || until) {
      // Snapshot mode — explicit reload only via the user
      // tapping the filter inputs (which retriggers this $effect).
      // No interval timer. An Until puts the page here too: the
      // stream pushes rows as they land, and none that lands now can
      // fall before it (62a0bbee).
      liveState = auto ? { kind: 'windowed' } : { kind: 'off' };
      return () => {
        cancelled = true;
      };
    }

    // Live mode — SSE pushes new rows as they land. Browser's
    // EventSource auto-reconnects when an open stream ends. Two
    // failures stop it for good, and both fall back to a 5s snapshot
    // poll with a line saying so: a refused request (onerror with
    // CLOSED — a non-200, a route 404 on an older deploy) and the
    // server's own `failed` frame, which the page answers by closing
    // the stream so the browser does not re-anchor past the gap.
    const params = new URLSearchParams();
    if (src) params.set('source', src);
    if (knd) params.set('kind', knd);
    if (act) params.set('actor', act);
    // The lens the snapshot applies, or a synthetic row lands in a view
    // the select calls "Real only" (34ea2ae0).
    if (prov !== 'all') params.set('simulated', prov);
    let es: EventSource | null = null;
    let pollFallbackId: number | null = null;
    function fallBackToPoll(reason: string): void {
      es?.close();
      es = null;
      if (cancelled) return;
      liveState = { kind: 'polling', reason };
      if (pollFallbackId === null) {
        pollFallbackId = window.setInterval(fetchSnapshot, SNAPSHOT_RELOAD_MS);
      }
    }
    liveState = { kind: 'connecting' };
    try {
      es = new EventSource(`/api/events/stream?${params.toString()}`);
      es.onopen = () => {
        if (!cancelled) liveState = { kind: 'live' };
      };
      es.addEventListener('failed', (ev) => {
        const data = (ev as MessageEvent<string>).data;
        let reason = 'the server reported a failed read and gave no reason';
        try {
          const body = JSON.parse(data) as { error?: unknown };
          if (typeof body.error === 'string' && body.error) reason = body.error;
        } catch {
          if (data) reason = data;
        }
        fallBackToPoll(reason);
      });
      es.onmessage = (ev) => {
        if (cancelled) return;
        try {
          const entry = JSON.parse(ev.data) as AuditEntry;
          if (!snapshotPainted && !early.some((r) => r.event_id === entry.event_id)) {
            early = [entry, ...early].slice(0, LIVE_BUFFER_CAP);
          }
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
        if (!es) return;
        if (es.readyState === EventSource.CLOSED) {
          // The browser hands an EventSource neither the status nor
          // the body of a refused request, so the line cannot say why.
          fallBackToPoll('the server refused it (the browser does not say why)');
        } else if (!cancelled) {
          liveState = { kind: 'reconnecting' };
        }
      };
    } catch (e) {
      fallBackToPoll(e instanceof Error ? e.message : String(e));
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
  let knownActors = $derived(loadState.kind === 'ready' ? actorsIn(loadState.rows) : []);

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
      <!-- The page's four failure lines — this one, the tail read's, the
           live stream's "down" and a failed export — each carry the shared
           marker (FAILURE_MARKER, tests/mocked/_routes.ts), which draws the
           failed read's rail and is what the outage crawl counts (sweep
           c3e4edcc). -->
      <p class="events-stats-note load-failed" role="alert">Stats unavailable: {statsState.message}</p>
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
          <span>Actor</span>
          <input
            list="events-actors"
            bind:value={actorFilter}
            placeholder="e.g. agent-claude"
          />
          <datalist id="events-actors">
            {#each knownActors as a (a)}
              <option value={a}></option>
            {/each}
          </datalist>
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
        <label class="events-filter" title="Rows at or after this time">
          <span>Since</span>
          <input type="datetime-local" bind:value={sinceInput} max={untilInput || undefined} />
        </label>
        <label class="events-filter" title="Rows before this time — the live stream stops while it is set">
          <span>Until</span>
          <input type="datetime-local" bind:value={untilInput} min={sinceInput || undefined} />
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
            <button
              type="button"
              class="events-download-go"
              onclick={startDownload}
              disabled={exportState.kind === 'reading'}
            >
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
            Exports up to 50,000 events matching the current source, kind, actor and provenance filters
            in the window above as JSON Lines (one event per line — parseable by
            <code>jq</code>, log forwarders, and most analytics tools).
            Narrow the window for large ranges.
          </p>
        </div>
      {/if}
      {#if exportState.kind !== 'idle'}
        <p
          class="events-download-status events-download-{exportState.kind}"
          class:load-failed={exportState.kind === 'failed'}
          role={exportState.kind === 'failed' ? 'alert' : 'status'}
        >
          {exportState.kind === 'reading' ? 'Reading the export…' : exportState.message}
        </p>
      {/if}
  </Section>

  <Section title="Stream" wide>
      {#if liveState.kind !== 'off'}
        <p
          class="events-live events-live-{liveState.kind}"
          class:load-failed={liveState.kind === 'polling'}
          role={liveState.kind === 'polling' ? 'alert' : 'status'}
        >
          {#if liveState.kind === 'connecting'}
            Live stream connecting…
          {:else if liveState.kind === 'live'}
            Live stream connected. New rows appear at the top as they land.
          {:else if liveState.kind === 'reconnecting'}
            Live stream lost; the browser is reconnecting. Rows that land before it is back will not stream — reload to read them.
          {:else if liveState.kind === 'windowed'}
            Live stream off: the Until bound closes the window, so no new row can land in it. Clear Until to follow the log.
          {:else}
            Live stream down: {liveState.reason}. Re-reading the tail every 5 s.
          {/if}
        </p>
      {/if}
      {#if loadState.kind === 'loading'}
        <p class="empty">Loading…</p>
      {:else if loadState.kind === 'error'}
        <p class="empty load-failed" role="alert">Failed to load: {loadState.message}</p>
      {:else if loadState.rows.length === 0}
        <p class="empty">No events match these filters.</p>
      {:else}
        <table class="data-table data-table-striped events-table">
          <thead>
            <tr>
              <th style="width:10ch">Time</th>
              <th style="width:12ch">Source</th>
              <th>Kind</th>
              <th style="width:22ch">Actor</th>
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
                <td class="mono">{actorOf(row.payload) ?? '—'}</td>
              </tr>
              {#if isOpen}
                {@const packet = packetOf(row.kind, row.payload)}
                <tr class="events-payload-row">
                  <td colspan="4">
                    <pre class="events-payload">{JSON.stringify(row.payload, null, 2)}</pre>
                    <div class="events-event-id">event_id: <code>{row.event_id}</code></div>
                    <!-- The packet this row belongs to, one click away
                         rather than an id to copy out of the JSON
                         (62a0bbee). auditRetro.ts says which key. -->
                    {#if packet}
                      <div class="events-packet">packet: <a href="/jobs/{packet}">{packet}</a></div>
                    {/if}
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
    color: var(--static);
    font-weight: 500;
  }
  .events-filter input,
  .events-filter select {
    min-width: 160px;
    padding: 4px 6px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--ink);
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
  .events-live {
    margin: 0 0 8px;
    font-size: 12px;
    color: var(--static);
  }
  /* The down line (polling) is the shared marker's since c3e4edcc. */
  .events-live-reconnecting {
    color: inherit;
    font-weight: 500;
  }
  .events-freshness {
    font-size: 12px;
    color: var(--static);
    margin-left: auto;
  }
  .events-download-btn {
    padding: 6px 12px;
    font-size: 12px;
    font-weight: 500;
    background: var(--ink);
    color: inherit;
    border: 1px solid var(--border);
    border-radius: 4px;
    cursor: pointer;
  }
  .events-download-btn:hover {
    background: var(--signal-wash);
  }
  .events-download-panel {
    margin-top: 12px;
    padding: 10px 12px;
    background: var(--signal-wash);
    border: 1px solid var(--border);
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
    background: var(--signal);
    color: var(--on-band);
    border: 1px solid var(--signal);
    border-radius: 4px;
    cursor: pointer;
  }
  .events-download-go:hover {
    background: var(--signal);
  }
  .events-download-cancel {
    padding: 6px 10px;
    font-size: 12px;
    background: transparent;
    color: var(--static);
    border: none;
    cursor: pointer;
  }
  .events-download-hint {
    margin: 8px 0 0;
    font-size: 11px;
    color: var(--static);
    line-height: 1.5;
  }
  .events-download-hint code {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    background: var(--wash);
    padding: 1px 4px;
    border-radius: 2px;
  }
  .events-download-status {
    margin: 8px 0 0;
    font-size: 12px;
    color: var(--static);
  }
  .events-table tbody tr.events-row {
    cursor: pointer;
  }
  .events-table tbody tr.events-row:hover {
    background: var(--signal-wash);
  }
  .events-table tbody tr.events-row-open {
    background: var(--signal-wash);
    font-weight: 500;
  }
  .events-table .mono {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 12px;
  }
  .events-payload-row td {
    background: var(--ink-raised);
    color: var(--fog);
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
  .events-event-id,
  .events-packet {
    padding: 4px 16px 10px;
    font-size: 11px;
    color: var(--static);
  }
  .events-packet a {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
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
