<script lang="ts">
  // /calendar/me — global-calendar surface for the currently
  // logged-in employee. v1 of the design doc's `Step 5`: shows
  // this week's reservations, one column per day. No drag-to-
  // reschedule (v2). No filter pickers yet (v2 — would be
  // straightforward additions: "show me X's calendar" / "this
  // account's calendar" / "this system's history").

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { session } from '@boss/web-kit/session/session.svelte';
  import { appNow } from '@boss/web-kit/sim-clock';

  type Reservation = {
    id: string;
    // A reservation is on a Subject; reservability is a subject-kind
    // property (calendar_reservable), not a closed type.
    subject: { subject_kind: string; id: string };
    window: { start: string; end: string };
    reason_kind: string;
    reason_ref_id: string;
    strength: 'hard' | 'soft';
    notes: string | null;
    created_by: string;
    created_at: string;
    cancelled_at: string | null;
  };

  let employeeId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : null,
  );

  // Week starts Monday in local time, ends following Sunday.
  function startOfWeek(date: Date): Date {
    const d = new Date(date);
    const day = (d.getDay() + 6) % 7; // Mon=0, Sun=6
    d.setDate(d.getDate() - day);
    d.setHours(0, 0, 0, 0);
    return d;
  }

  function addDays(d: Date, n: number): Date {
    const c = new Date(d);
    c.setDate(c.getDate() + n);
    return c;
  }

  let weekAnchor = $state(startOfWeek(appNow()));
  let weekStart = $derived(weekAnchor);
  let weekEnd = $derived(addDays(weekAnchor, 7));
  let days = $derived(
    Array.from({ length: 7 }, (_, i) => addDays(weekAnchor, i)),
  );

  let reservations = $state<Reservation[]>([]);
  let loading = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    const eid = employeeId;
    const start = weekStart.toISOString();
    const end = weekEnd.toISOString();
    if (!eid) return;
    let cancelled = false;
    loading = true;
    error = null;
    (async () => {
      try {
        const url = `/api/calendar/reservations?resource_kind=employee&resource_id=${encodeURIComponent(
          eid,
        )}&start=${encodeURIComponent(start)}&end=${encodeURIComponent(end)}`;
        const r = await fetch(url);
        if (!r.ok) {
          if (r.status === 503 || r.status === 502) {
            throw new Error(
              'calendar service unavailable — wire calendar_api_url',
            );
          }
          throw new Error(`calendar HTTP ${r.status}`);
        }
        const body = (await r.json()) as Reservation[];
        if (!cancelled) {
          reservations = body;
          loading = false;
        }
      } catch (e) {
        if (!cancelled) {
          error = e instanceof Error ? e.message : String(e);
          reservations = [];
          loading = false;
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function shiftWeek(weeks: number) {
    weekAnchor = addDays(weekAnchor, 7 * weeks);
  }

  function jumpToToday() {
    weekAnchor = startOfWeek(appNow());
  }

  function formatDayHeader(d: Date): string {
    return d.toLocaleDateString('en-US', { weekday: 'short', month: 'short', day: 'numeric' });
  }

  function formatTime(iso: string): string {
    const d = new Date(iso);
    return d.toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit' });
  }

  function reservationsForDay(d: Date): Reservation[] {
    const dayStart = new Date(d);
    dayStart.setHours(0, 0, 0, 0);
    const dayEnd = addDays(dayStart, 1);
    return reservations
      .filter((r) => {
        const start = new Date(r.window.start);
        const end = new Date(r.window.end);
        return start < dayEnd && end > dayStart;
      })
      .sort(
        (a, b) =>
          new Date(a.window.start).getTime() - new Date(b.window.start).getTime(),
      );
  }

  function reasonLabel(kind: string): string {
    switch (kind) {
      case 'job-step':
        return 'Job step';
      case 'preventive-maintenance-visit':
        return 'preventive maintenance visit';
      case 'training':
        return 'Training';
      case 'pto':
        return 'PTO';
      case 'meeting':
        return 'Meeting';
      case 'travel':
        return 'Travel';
      default:
        return kind;
    }
  }

  function reasonClass(kind: string): string {
    return `chip chip-reason-${kind}`;
  }
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Calendar"
    title={employeeId ? `${employeeId} — week of ${formatDayHeader(weekAnchor)}` : 'My Week'}
    subtitle="Reservations from the global calendar primitive"
  />

  <!-- Without an employee there is no week to page through: the body
       says "Sign in", the title has no week in it, and the three
       controls changed nothing on screen. The interaction crawl
       (f2b8a01c) found them answering nothing; disabled says so. -->
  <div class="week-controls">
    <button onclick={() => shiftWeek(-1)} disabled={!employeeId}>← Prev week</button>
    <button onclick={jumpToToday} disabled={!employeeId}>This week</button>
    <button onclick={() => shiftWeek(1)} disabled={!employeeId}>Next week →</button>
  </div>

  {#if !employeeId}
    <p class="empty">Sign in to see your week.</p>
  {:else if loading}
    <p class="empty">Loading reservations…</p>
  {:else if error}
    <p class="empty">Couldn't load calendar: {error}</p>
  {:else}
    <div class="week-grid">
      {#each days as day (day.toISOString())}
        {@const dayRows = reservationsForDay(day)}
        <div class="week-col">
          <div class="week-col-header">{formatDayHeader(day)}</div>
          {#if dayRows.length === 0}
            <div class="week-col-empty">—</div>
          {:else}
            {#each dayRows as r (r.id)}
              <div class="week-cell">
                <div class="week-cell-time">
                  {formatTime(r.window.start)}–{formatTime(r.window.end)}
                </div>
                <div class={reasonClass(r.reason_kind)}>
                  {reasonLabel(r.reason_kind)}
                </div>
                <div class="week-cell-ref mono">{r.reason_ref_id}</div>
                {#if r.notes}
                  <div class="week-cell-notes">{r.notes}</div>
                {/if}
              </div>
            {/each}
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .week-controls {
    display: flex;
    gap: 8px;
    margin-bottom: 16px;
  }
  .week-controls button {
    padding: 6px 12px;
    border: 1px solid var(--hairline);
    background: var(--ink);
    border-radius: 6px;
    cursor: pointer;
  }
  .week-controls button:hover {
    background: var(--ink-raised);
  }
  .week-grid {
    display: grid;
    grid-template-columns: repeat(7, 1fr);
    gap: 8px;
    min-height: 60vh;
  }
  .week-col {
    border: 1px solid var(--hairline);
    border-radius: 6px;
    background: var(--ink);
    padding: 8px;
    min-height: 200px;
  }
  .week-col-header {
    font-weight: 600;
    border-bottom: 1px solid var(--hairline);
    padding-bottom: 4px;
    margin-bottom: 8px;
    color: var(--static);
  }
  .week-col-empty {
    color: var(--static);
    font-size: 12px;
  }
  .week-cell {
    border-left: 3px solid var(--busy);
    background: var(--warn-wash);
    padding: 6px 8px;
    margin-bottom: 6px;
    border-radius: 0 4px 4px 0;
    font-size: 12px;
  }
  .week-cell-time {
    font-weight: 600;
    color: var(--fog);
  }
  .week-cell-ref {
    color: var(--static);
    font-size: 11px;
  }
  .week-cell-notes {
    color: var(--static);
    margin-top: 2px;
    font-style: italic;
  }
  :global(.chip-reason-job-step) {
    background: var(--ok-wash);
    color: var(--ok);
  }
  :global(.chip-reason-pto) {
    background: var(--err-wash);
    color: var(--err);
  }
  :global(.chip-reason-meeting) {
    background: var(--signal-wash);
    color: var(--signal);
  }
  :global(.chip-reason-preventive-maintenance-visit) {
    background: var(--warn-wash);
    color: var(--warn);
  }
  :global(.chip-reason-training) {
    background: var(--signal-wash);
    color: var(--signal);
  }
  :global(.chip-reason-travel) {
    background: var(--ink-raised);
    color: var(--static);
  }
  :global(.chip-reason-custom) {
    background: var(--ink-raised);
    color: var(--static);
  }
</style>
