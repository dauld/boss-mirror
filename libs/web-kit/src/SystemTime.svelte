<script lang="ts">
  // System-time indicator for the standard app shell. Shows the clock
  // the model runs on, and flags "Sim" when that clock is being driven
  // by the simulator (i.e. it has diverged from the wall clock).
  //
  // This component ALSO owns the shared sim-clock store's refresh
  // plumbing (SSE stream + poll fallback) so every page's appNow() /
  // appToday() stays current. It hosts NO controls — pause / resume /
  // reset live in the Simulator app (/simulator). Render it once, in the
  // app shell.
  import type { SimClockState } from './sim-clock-types';
  import { simClock } from './sim-clock.svelte';

  let clock = $derived<SimClockState | null>(simClock.value);
  let wallNow = $state<Date | null>(null);

  async function fallbackPoll(): Promise<void> {
    try {
      const r = await fetch('/api/jobs/live');
      if (!r.ok) return;
      const body = (await r.json()) as { sim_clock?: SimClockState | null };
      simClock.set(body.sim_clock ?? null);
    } catch {
      // Silent — the SSE stream delivers state once it connects.
    }
  }

  // Keep the shared store fresh: SSE stream with a 20s poll fallback.
  $effect(() => {
    void fallbackPoll();
    let es: EventSource | null = null;
    let pollId: number | null = null;
    try {
      es = new EventSource('/api/jobs/sim-clock/stream');
      es.onmessage = (ev) => {
        try {
          simClock.set(JSON.parse(ev.data) as SimClockState);
        } catch {
          // Malformed frame — drop and wait for the next.
        }
      };
      es.onerror = () => {
        if (es && es.readyState === EventSource.CLOSED) {
          es.close();
          es = null;
          if (pollId === null) pollId = window.setInterval(fallbackPoll, 20_000);
        }
      };
    } catch {
      pollId = window.setInterval(fallbackPoll, 20_000);
    }
    return () => {
      es?.close();
      if (pollId !== null) window.clearInterval(pollId);
    };
  });

  // When no sim clock is driving the system, the "system time" IS the
  // wall clock — tick it so the topbar stays live.
  $effect(() => {
    if (clock) {
      wallNow = null;
      return;
    }
    wallNow = new Date();
    const id = window.setInterval(() => {
      wallNow = new Date();
    }, 30_000);
    return () => window.clearInterval(id);
  });

  let isSim = $derived(!!clock?.current_sim_date);

  function fmtSim(c: SimClockState): string {
    const iso = c.now ?? c.current_sim_date;
    if (!iso) return '';
    if (iso.length === 10) return iso; // date-only fallback
    const d = new Date(iso);
    if (isNaN(d.getTime())) return iso;
    const date = d.toISOString().slice(0, 10);
    const hh = String(d.getUTCHours()).padStart(2, '0');
    const mm = String(d.getUTCMinutes()).padStart(2, '0');
    return `${date} ${hh}:${mm}`;
  }

  function fmtWall(d: Date): string {
    return (
      d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' }) +
      ' ' +
      d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
    );
  }

  let display = $derived(clock ? fmtSim(clock) : wallNow ? fmtWall(wallNow) : '');
</script>

{#if display}
  <span class="system-time" title="System time — the clock the model runs on">
    <span class="system-time-label">System time</span>
    <span class="system-time-val">{display}</span>
    {#if isSim}
      <span class="plate" class:plate-busy={!clock?.paused} class:plate-troubled={clock?.paused}>
        {clock?.paused ? 'Sim · paused' : 'Sim'}
      </span>
    {/if}
  </span>
{/if}

<style>
  /* Sits on the chrome bar's enamel band (backlog 7eb59678 car 2): the
     band's white for the reading, its dimmer white for the label. The
     Sim tag is a state, so it is a plate — busy while the model runs,
     troubled when paused (apps/web styles.css, "States are plates"). */
  .system-time {
    display: inline-flex;
    align-items: baseline;
    gap: 8px;
    font-size: 12px;
    color: var(--on-band-dim);
    white-space: nowrap;
  }
  .system-time-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--on-band-dim);
  }
  .system-time-val {
    font-variant-numeric: tabular-nums;
    font-weight: 600;
    color: var(--on-band);
  }
</style>
