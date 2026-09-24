<script lang="ts">
  // Launch calendar — port of apps/web/src/calendar/CalendarPage.tsx.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import { appNow, appToday } from '@boss/web-kit/sim-clock';
  import { okRead, type ReadState } from '../data/readState';
  import { loadOwnerNames, ownerIdsOf } from './ownerNames';

  type LaunchCalendarRow = {
    job_id: string;
    title: string;
    owner_id: string | null;
    subject_id: string | null;
    status: string;
    current_tier: number | null;
    launch_date: string | null;
    launch_channel: string | null;
  };

  type WindowPreset = '30d' | '90d' | '180d';
  const WINDOW_DAYS: Record<WindowPreset, number> = {
    '30d': 30,
    '90d': 90,
    '180d': 180,
  };

  function shiftDays(anchor: Date, n: number): string {
    const d = new Date(anchor);
    d.setDate(d.getDate() + n);
    return d.toISOString().slice(0, 10);
  }

  function todayIso(): string {
    return appToday();
  }

  function formatLongDate(iso: string): string {
    const d = new Date(`${iso}T12:00:00Z`);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleDateString('en-US', {
      weekday: 'short',
      month: 'short',
      day: 'numeric',
      year: 'numeric',
    });
  }

  let windowPreset = $state<WindowPreset>('90d');
  let data = $state<LaunchCalendarRow[]>([]);
  /// Non-null when the load failed — rendered instead of the empty
  /// state, so an outage never reads as "no motions in this window"
  /// (packet 3fba9c35, the false-empty sweep).
  let loadFailed = $state<string | null>(null);
  let loading = $state(true);
  let empNames = $state<ReadonlyMap<string, string>>(new Map());
  let peopleRead = $state<ReadState>(okRead);

  let from = $derived(todayIso());
  let to = $derived(shiftDays(appNow(), WINDOW_DAYS[windowPreset]));

  $effect(() => {
    const f = from;
    const t = to;
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const qs = new URLSearchParams({ from: f, to: t });
        const r = await fetch(`/api/jobs/launch-calendar?${qs.toString()}`);
        if (r.ok) {
          const body = (await r.json()) as { data?: LaunchCalendarRow[] };
          if (!cancelled) {
            data = Array.isArray(body?.data) ? body.data : [];
            loadFailed = null;
          }
        } else {
          if (!cancelled) {
            data = [];
            loadFailed = `HTTP ${r.status}`;
          }
        }
      } catch (e) {
        if (!cancelled) {
          data = [];
          loadFailed = e instanceof Error ? e.message : String(e);
        }
      }
      if (!cancelled) loading = false;
    })();
    return () => {
      cancelled = true;
    };
  });

  // Names only the owners shown, one row each, and says so when a name
  // cannot load. Until backlog 0268a829 this read the whole roster, and
  // a refusal or a network error was dropped, so the owners silently
  // became ids (see ./ownerNames.ts).
  let ownerIds = $derived(ownerIdsOf(data));
  $effect(() => {
    const ids = ownerIds;
    let cancelled = false;
    (async () => {
      const out = await loadOwnerNames(ids);
      if (!cancelled) {
        empNames = out.names;
        peopleRead = out.read;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let grouped = $derived.by(() => {
    const map = new Map<string, LaunchCalendarRow[]>();
    for (const row of data) {
      const key = row.launch_date ?? 'unscheduled';
      const list = map.get(key) ?? [];
      list.push(row);
      map.set(key, list);
    }
    const keys = Array.from(map.keys()).sort((a, b) => {
      if (a === 'unscheduled') return -1;
      if (b === 'unscheduled') return 1;
      return a.localeCompare(b);
    });
    return keys.map((k) => ({ date: k, rows: map.get(k)! }));
  });

  const WINDOW_KEYS: ReadonlyArray<WindowPreset> = ['30d', '90d', '180d'];
</script>

<div class="theme-exec" style="padding:32px">
  <PageHeader
    eyebrow="Know"
    title={`Launch calendar (${data.length}${loading ? '…' : ''})`}
    subtitle="Every in-flight marketing motion, anchored to its launch date."
  />

  <div style="display:flex; gap:8px; margin-bottom:16px">
    {#each WINDOW_KEYS as k (k)}
      <button
        type="button"
        onclick={() => (windowPreset = k)}
        class="step-btn"
        style={`font-weight:${windowPreset === k ? 600 : 400}; background:${windowPreset === k ? 'var(--ink-raised)' : ''}`}
      >
        Next {WINDOW_DAYS[k]} days
      </button>
    {/each}
    <span style="margin-left:auto; font-size:12px; color:var(--static)">
      {from} → {to}
    </span>
  </div>

  {#if loading && data.length === 0}
    <p class="empty">Loading…</p>
  {:else if loadFailed}
    <p class="empty load-failed" role="alert">
      Couldn't load the launch calendar — {loadFailed}
    </p>
  {:else if grouped.length === 0}
    <p class="empty">No marketing motions in this window.</p>
  {:else}
    {#if peopleRead.kind === 'failed'}
      <p class="load-failed" role="alert" style="font-size:12px; margin-bottom:12px">
        Couldn't load owner names — {peopleRead.error}. Owners show as ids.
      </p>
    {/if}
    <div style="display:flex; flex-direction:column; gap:20px">
      {#each grouped as block (block.date)}
        {@const label = block.date === 'unscheduled' ? 'Unscheduled' : formatLongDate(block.date)}
        <section>
          <h3
            style="font-size:13px; color:var(--static); text-transform:uppercase; letter-spacing:0.4px; margin-bottom:8px; padding-bottom:4px; border-bottom:1px solid var(--hairline)"
          >
            {label}
            <span style="color:var(--static); font-weight:400">· {block.rows.length}</span>
          </h3>
          <ul style="list-style:none; padding:0; margin:0">
            {#each block.rows as r (r.job_id)}
              <li style="display:flex; gap:12px; padding:6px 0; border-bottom:1px solid var(--hairline); font-size:13px">
                <div style="flex:1">
                  <EntityLink kind="job" id={r.job_id} label={r.title} />
                  {#if r.launch_channel}
                    <span
                      style="margin-left:8px; padding:1px 6px; font-size:11px; background:var(--ink-raised); border-radius:3px; color:var(--static)"
                    >
                      {r.launch_channel}
                    </span>
                  {/if}
                </div>
                <div style="color:var(--static); font-size:12px">
                  {#if r.owner_id}
                    <EntityLink
                      kind="employee"
                      id={r.owner_id}
                      label={empNames.get(r.owner_id)}
                    />
                  {:else}
                    —
                  {/if}
                </div>
                <div
                  style="color:var(--static); font-size:11px; text-transform:uppercase; letter-spacing:0.3px; min-width:96px; text-align:right"
                >
                  tier {r.current_tier ?? '—'} · {r.status}
                </div>
              </li>
            {/each}
          </ul>
        </section>
      {/each}
    </div>
  {/if}
</div>
