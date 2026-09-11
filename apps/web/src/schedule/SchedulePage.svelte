<script lang="ts">
  // Field-service schedule — port of apps/web/src/schedule/SchedulePage.tsx.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { formatDate } from '@boss/web-kit/ui/date';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import { appNow } from '@boss/web-kit/sim-clock';

  type AvailabilityKind =
    | 'available' | 'pto' | 'sick' | 'holiday' | 'training' | 'blocked';
  type AssignmentKind =
    | 'wo' | 'pm' | 'training' | 'diag-call' | 'travel' | 'install';
  type AssignmentStatus =
    | 'tentative' | 'confirmed' | 'completed' | 'cancelled' | 'no-show';

  type AvailabilityBlock = {
    source: 'availability';
    id: string;
    kind: AvailabilityKind;
    starts_at: string;
    ends_at: string;
    notes: string | null;
  };
  type AssignmentBlock = {
    source: 'assignment';
    id: string;
    kind: AssignmentKind;
    status: AssignmentStatus;
    target_job_id: string;
    target_job_title: string | null;
    target_workflow: string | null;
    starts_at: string;
    ends_at: string;
    notes: string | null;
  };
  type WeekGridBlock = AvailabilityBlock | AssignmentBlock;

  type WeekGridRow = {
    employee_id: string;
    blocks: ReadonlyArray<WeekGridBlock>;
  };
  type WeekGridResponse = {
    from: string;
    to: string;
    rows: ReadonlyArray<WeekGridRow>;
  };

  const DAY_LABELS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];

  function startOfWeek(anchor: Date): Date {
    const d = new Date(anchor);
    const dow = d.getUTCDay();
    const daysSinceMon = (dow + 6) % 7;
    d.setUTCDate(d.getUTCDate() - daysSinceMon);
    d.setUTCHours(0, 0, 0, 0);
    return d;
  }
  function addDays(d: Date, n: number): Date {
    const r = new Date(d);
    r.setUTCDate(r.getUTCDate() + n);
    return r;
  }

  const AVAIL_COLOR: Record<AvailabilityKind, { bg: string; fg: string }> = {
    available: { bg: '#dcfce7', fg: '#166534' },
    pto:       { bg: '#fecaca', fg: '#991b1b' },
    sick:      { bg: '#fef3c7', fg: '#92400e' },
    holiday:   { bg: '#e9d5ff', fg: '#6b21a8' },
    training:  { bg: '#dbeafe', fg: '#1e40af' },
    blocked:   { bg: '#e7e5e4', fg: '#44403c' },
  };
  const ASSIGN_COLOR: Record<AssignmentKind, { bg: string; fg: string }> = {
    wo:          { bg: '#fef3c7', fg: '#78350f' },
    pm:          { bg: '#fed7aa', fg: '#9a3412' },
    install:     { bg: '#bae6fd', fg: '#075985' },
    training:    { bg: '#dbeafe', fg: '#1e3a8a' },
    'diag-call': { bg: '#c7d2fe', fg: '#3730a3' },
    travel:      { bg: '#e5e7eb', fg: '#374151' },
  };

  function timeRange(startIso: string, endIso: string): string {
    const s = new Date(startIso);
    const e = new Date(endIso);
    const fmt = (d: Date): string =>
      d.toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit' });
    return `${fmt(s)}-${fmt(e)}`;
  }
  function localHours(startIso: string, endIso: string): string {
    const s = new Date(startIso);
    const e = new Date(endIso);
    const pad = (n: number): string => n.toString().padStart(2, '0');
    return `${pad(s.getHours())}-${pad(e.getHours())}`;
  }
  function truncate(s: string, n: number): string {
    return s.length <= n ? s : `${s.slice(0, n - 1)}…`;
  }

  let weekOffset = $state(0);
  let data = $state<WeekGridResponse | null>(null);
  /// Non-null when the load failed — rendered instead of the empty
  /// state, so an outage never reads as "no techs have availability"
  /// (packet 3fba9c35, the false-empty sweep).
  let loadFailed = $state<string | null>(null);
  let loading = $state(true);
  let empNames = $state<Map<string, string>>(new Map());

  let weekStart = $derived(addDays(startOfWeek(appNow()), weekOffset * 7));
  let weekEnd = $derived(addDays(weekStart, 7));
  let from = $derived(weekStart.toISOString());
  let to = $derived(weekEnd.toISOString());
  let days = $derived(Array.from({ length: 7 }, (_, i) => addDays(weekStart, i)));

  let weekLabel = $derived.by(() => {
    const opts: Intl.DateTimeFormatOptions = { month: 'short', day: 'numeric' };
    return `${weekStart.toLocaleDateString('en-US', opts)} – ${addDays(weekEnd, -1).toLocaleDateString('en-US', opts)}`;
  });

  $effect(() => {
    const f = from;
    const t = to;
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const qs = new URLSearchParams({ from: f, to: t });
        const r = await fetch(`/api/scheduling/week-grid?${qs.toString()}`);
        if (r.ok) {
          const body = (await r.json()) as WeekGridResponse;
          if (!cancelled) {
            data = body;
            loadFailed = null;
          }
        } else {
          if (!cancelled) {
            data = null;
            loadFailed = `HTTP ${r.status}`;
          }
        }
      } catch (e) {
        if (!cancelled) {
          data = null;
          loadFailed = e instanceof Error ? e.message : String(e);
        }
      }
      if (!cancelled) loading = false;
    })();
    return () => {
      cancelled = true;
    };
  });

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/people');
        if (r.ok) {
          const body = (await r.json()) as Array<{ id: string; name: string }>;
          const m = new Map<string, string>();
          for (const e of body) m.set(e.id, e.name);
          if (!cancelled) empNames = m;
        }
      } catch {
        // ignore
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function cellsForRow(row: WeekGridRow): WeekGridBlock[][] {
    const cells: WeekGridBlock[][] = days.map(() => [] as WeekGridBlock[]);
    for (const b of row.blocks) {
      const start = new Date(b.starts_at);
      const end = new Date(b.ends_at);
      for (let i = 0; i < days.length; i++) {
        const dayStart = days[i]!;
        const dayEnd = addDays(dayStart, 1);
        if (start < dayEnd && end > dayStart) cells[i]!.push(b);
      }
    }
    return cells;
  }
</script>

<div class="theme-exec" style="padding:32px">
  <PageHeader
    eyebrow="Work"
    title={`Service schedule — week of ${weekLabel}`}
    subtitle={`${data?.rows.length ?? 0} techs with blocks this week.`}
  />

  <div style="display:flex; gap:8px; margin-bottom:16px; align-items:center">
    <button
      type="button"
      class="step-btn"
      onclick={() => (weekOffset = weekOffset - 1)}
    >
      ← Prev week
    </button>
    <button
      type="button"
      class="step-btn"
      onclick={() => (weekOffset = 0)}
      style={`font-weight:${weekOffset === 0 ? 600 : 400}`}
    >
      This week
    </button>
    <button
      type="button"
      class="step-btn"
      onclick={() => (weekOffset = weekOffset + 1)}
    >
      Next week →
    </button>
    <span style="margin-left:auto; font-size:12px; color:#78716c">
      {formatDate(from)} → {formatDate(addDays(weekEnd, -1).toISOString())}
    </span>
  </div>

  {#if loading && !data}
    <p class="empty">Loading…</p>
  {:else if loadFailed}
    <p class="empty load-failed" role="alert">
      Couldn't load the week grid — {loadFailed}
    </p>
  {:else if !data || data.rows.length === 0}
    <p class="empty">No techs have availability or assignments this week.</p>
  {:else}
    <div style="overflow-x:auto">
      <table class="data-table" style="min-width:900px; border-collapse:collapse">
        <thead>
          <tr>
            <th
              style="min-width:140px; position:sticky; left:0; background:#fafaf9; z-index:1"
            >
              Tech
            </th>
            {#each days as d, i (i)}
              <th style="text-align:left; min-width:130px">
                <div style="font-size:11px; color:#78716c">{DAY_LABELS[i]}</div>
                <div class="mono" style="font-size:11px; color:#a8a29e">
                  {d.toISOString().slice(5, 10)}
                </div>
              </th>
            {/each}
          </tr>
        </thead>
        <tbody>
          {#each data.rows as row (row.employee_id)}
            {@const cells = cellsForRow(row)}
            <tr>
              <td
                style="position:sticky; left:0; background:#fafaf9; z-index:1"
              >
                <EntityLink
                  kind="employee"
                  id={row.employee_id}
                  label={empNames.get(row.employee_id)}
                />
              </td>
              {#each cells as blocks, i (i)}
                <td
                  style="vertical-align:top; padding:4px; border-left:1px solid #f5f5f4"
                >
                  {#if blocks.length === 0}
                    <span style="color:#d6d3d1; font-size:10px">·</span>
                  {:else}
                    <div style="display:flex; flex-direction:column; gap:2px">
                      {#each blocks as b, j (`${b.id}-${j}`)}
                        {#if b.source === 'availability'}
                          {@const c = AVAIL_COLOR[b.kind]}
                          <div
                            style={`background:${c.bg}; color:${c.fg}; font-size:10px; padding:2px 6px; border-radius:3px; text-transform:uppercase; letter-spacing:0.3px`}
                            title={`${b.kind} · ${timeRange(b.starts_at, b.ends_at)}`}
                          >
                            {b.kind === 'available'
                              ? `Avail ${localHours(b.starts_at, b.ends_at)}`
                              : b.kind}
                          </div>
                        {:else}
                          {@const c = ASSIGN_COLOR[b.kind]}
                          <div
                            style={`background:${c.bg}; color:${c.fg}; font-size:10px; padding:2px 6px; border-radius:3px; font-weight:${b.status === 'tentative' ? 400 : 600}; border-left:${b.status === 'tentative' ? `3px dashed ${c.fg}` : `3px solid ${c.fg}`}`}
                            title={`${b.kind} · ${b.status} · ${timeRange(b.starts_at, b.ends_at)} · ${b.target_job_title ?? b.target_job_id}`}
                          >
                            {b.kind}{b.target_job_title ? ` · ${truncate(b.target_job_title, 18)}` : ''}
                          </div>
                        {/if}
                      {/each}
                    </div>
                  {/if}
                </td>
              {/each}
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>
