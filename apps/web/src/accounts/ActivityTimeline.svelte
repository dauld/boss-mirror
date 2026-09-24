<script lang="ts">
  // Activity timeline — client-side merge: 4 parallel fetches,
  // normalize, sort desc. Window defaults to 90d; "Load older"
  // doubles it.

  import Section from '@boss/web-kit/ui/Section.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { loadTimeline } from './api';
  import type { TimelineEntry } from './types';
  import { appNow } from '@boss/web-kit/sim-clock';

  let { accountId } = $props<{ accountId: string }>();

  let entries = $state<TimelineEntry[]>([]);
  /// A failed leg of the merge, rendered instead of the empty state —
  /// a partial or unreachable history must not read as "no activity"
  /// (packet 3fba9c35).
  let loadFailed = $state<string | null>(null);
  let loading = $state(true);
  // Default window must cover seeded-bundle data: brewery seed
  // is a 12-month sim ending up to a year before the install
  // wallclock, so a 90d default (anchored to wallclock today)
  // filters every seeded invoice/job/shipment out of view and
  // leaves the timeline empty. 1825d = 5 years covers the
  // seeded data + any post-install live activity. Same bug
  // shape as the ledger BS endpoint's pre-fix YTD-net-income
  // filter (2026-05-29 finding).
  let windowDays = $state(1825);

  $effect(() => {
    const pid = accountId;
    const days = windowDays;
    let cancelled = false;
    loading = true;
    (async () => {
      const result = await loadTimeline(pid, days);
      if (!cancelled) {
        if (result.kind === 'ready') {
          entries = result.entries;
          loadFailed = null;
        } else {
          entries = [];
          loadFailed = result.error;
        }
        loading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let olderAvailable = $derived(windowDays < 1825);

  function daysAgo(iso: string): string {
    // Source `now` from the sim clock — in sim mode the timeline
    // entries are sim-dated (e.g. 2025-04-15) but Date.now() returns
    // wallclock (e.g. 2026-05-31), so events would render as 13mo
    // ago even when they happened "today" in sim time. Same fix as
    // the v1.0.5 SPA sweep that swapped 35 of 41 new Date() sites
    // to appNow().
    const then = new Date(iso).getTime();
    const now = appNow().getTime();
    const d = Math.floor((now - then) / 86_400_000);
    if (d < 1) return 'today';
    if (d === 1) return '1d';
    if (d < 30) return `${d}d`;
    if (d < 365) return `${Math.floor(d / 30)}mo`;
    return `${Math.floor(d / 365)}y`;
  }
</script>

<Section title={entries.length > 20 ? `Activity timeline (20 of ${entries.length})` : `Activity timeline (${entries.length})`} wide>
    {#if loading && entries.length === 0}
      <p class="empty">Loading timeline…</p>
    {:else if loadFailed}
      <p class="empty load-failed" role="alert">
        Couldn't load the timeline — {loadFailed}
      </p>
    {:else if entries.length === 0}
      <p class="empty">No recent activity for this account.</p>
    {:else}
      <ul class="pp-timeline">
        {#each entries.slice(0, 20) as e (e.id)}
          <li class="pp-timeline-entry">
            <span class="pp-timeline-icon">{e.icon}</span>
            <span class="pp-timeline-date">{daysAgo(e.date)}</span>
            <span class="pp-timeline-body">
              {#if e.link}
                <Link to={e.link}>
                  {e.title}
                </Link>
              {:else}
                {e.title}
              {/if}
              {#if e.detail}<div class="pp-timeline-detail">{e.detail}</div>{/if}
            </span>
          </li>
        {/each}
      </ul>
    {/if}
    {#if olderAvailable}
      <button
        type="button"
        class="btn"
        style="margin-top: 8px"
        onclick={() => (windowDays = windowDays * 2)}
      >
        Load older activity
      </button>
    {/if}
</Section>
