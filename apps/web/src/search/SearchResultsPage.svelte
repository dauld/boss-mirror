<script lang="ts">
  // Full results for a global-search query — the surface the chrome
  // dropdown escalates to (Q4).
  //
  // The dropdown is a preview scoped to the app you are in; this is the
  // unscoped view. It sends no `app_kinds`, deliberately: having chosen
  // to leave the dropdown you are no longer asking "in this app", and
  // re-applying the weighting here would quietly hide the thing you
  // came looking for.
  //
  // The layout leads with Subjects carrying their Jobs and events,
  // because that adjacency IS the claim. Three separate lists would
  // render the same data as the federated search this replaced.
  import { onMount } from 'svelte';
  import { formatDate } from '@boss/web-kit/ui/date';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';

  let { q } = $props<{ q: string }>();

  type Row = {
    ref_kind: 'subject' | 'job' | 'event';
    ref_id: string;
    subject_kind: string | null;
    subject_id: string | null;
    title: string;
    body: string;
    occurred_at: string | null;
  };
  type SubjectHit = {
    subject_kind: string;
    subject_id: string;
    title: string;
    jobs: Row[];
    events: Row[];
    event_count: number;
  };
  type Results = {
    query: string;
    subjects: SubjectHit[];
    jobs: Row[];
    events: Row[];
  };

  let results = $state<Results | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  function pathFor(kind: string, id: string): string {
    const direct: Record<string, string> = {
      account: `/ux/accounts/${id}`,
      vendor: `/ux/vendors/${id}`,
      employee: `/ux/people/${id}`,
      product: `/ux/products/${id}`,
      asset: `/ux/assets/${id}`,
    };
    return (
      direct[kind] ??
      `/ux/jobs?subject_kind=${encodeURIComponent(kind)}&subject_id=${encodeURIComponent(id)}`
    );
  }

  async function load(): Promise<void> {
    loading = true;
    error = null;
    if (!q.trim()) {
      results = null;
      loading = false;
      return;
    }
    try {
      const r = await fetch(`/api/search?q=${encodeURIComponent(q)}`);
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      results = (await r.json()) as Results;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  onMount(load);

  let total = $derived(
    results
      ? results.subjects.length + results.jobs.length + results.events.length
      : 0,
  );
</script>

<PageHeader
  title="Search"
  subtitle={q ? `Results for “${q}”` : 'Type in the search box above'}
/>

{#if loading}
  <p class="sr-msg">Searching…</p>
{:else if error}
  <p class="sr-msg sr-err">{error}</p>
{:else if !q.trim()}
  <p class="sr-msg">Nothing to search for yet.</p>
{:else if total === 0}
  <p class="sr-msg">No matches for “{q}”.</p>
{:else if results}
  {#each results.subjects as s (s.subject_kind + s.subject_id)}
    <section class="sr-subject">
      <a class="sr-subject-head" href={pathFor(s.subject_kind, s.subject_id)}>
        <span class="sr-kind">{s.subject_kind}</span>
        <span class="sr-title">{s.title}</span>
        <span class="sr-id">{s.subject_id}</span>
      </a>

      {#if s.jobs.length > 0 || s.event_count > 0}
        <div class="sr-panes">
          <div class="sr-pane">
            <div class="sr-pane-label">
              Work ({s.jobs.length})
            </div>
            {#each s.jobs as j (j.ref_id)}
              <a class="sr-line" href={`/ux/jobs/${j.ref_id}`}>
                <span class="sr-line-title">{j.title}</span>
                <span class="sr-line-sub">{j.body}</span>
              </a>
            {:else}
              <p class="sr-none">No jobs</p>
            {/each}
          </div>

          <div class="sr-pane">
            <div class="sr-pane-label">
              History ({s.events.length} of {s.event_count})
            </div>
            {#each s.events as e (e.ref_id)}
              <div class="sr-line sr-line-static">
                <span class="sr-line-title">{e.title}</span>
                <span class="sr-line-sub">
                  {e.occurred_at ? formatDate(e.occurred_at) : ''}
                </span>
              </div>
            {:else}
              <p class="sr-none">No events</p>
            {/each}
          </div>
        </div>
      {/if}
    </section>
  {/each}

  {#if results.jobs.length > 0}
    <section class="sr-group">
      <h3 class="sr-group-title">Jobs ({results.jobs.length})</h3>
      {#each results.jobs as j (j.ref_id)}
        <a class="sr-line" href={`/ux/jobs/${j.ref_id}`}>
          <span class="sr-line-title">{j.title}</span>
          <span class="sr-line-sub">{j.body}</span>
        </a>
      {/each}
    </section>
  {/if}

  {#if results.events.length > 0}
    <section class="sr-group">
      <h3 class="sr-group-title">Events ({results.events.length})</h3>
      {#each results.events as e (e.ref_id)}
        <div class="sr-line sr-line-static">
          <span class="sr-line-title">{e.title}</span>
          <span class="sr-line-sub">
            {e.subject_kind ?? ''} {e.subject_id ?? ''}
          </span>
        </div>
      {/each}
    </section>
  {/if}
{/if}

<style>
  .sr-msg {
    color: var(--text-dim, #78716c);
    font-size: 14px;
    padding: 16px 0;
  }
  .sr-err {
    color: #b91c1c;
  }
  .sr-subject {
    border: 1px solid var(--border, #e7e5e4);
    border-radius: 8px;
    margin-bottom: 14px;
    background: var(--card, #fff);
    overflow: hidden;
  }
  .sr-subject-head {
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 12px 16px;
    text-decoration: none;
    color: var(--text, #1c1917);
    border-bottom: 1px solid var(--border, #e7e5e4);
  }
  .sr-subject-head:hover {
    background: var(--bg, #f5f5f4);
  }
  .sr-kind {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-dim, #78716c);
  }
  .sr-title {
    font-size: 15px;
    font-weight: 600;
    flex: 1 1 auto;
  }
  .sr-id {
    font-size: 11px;
    color: var(--text-dim, #78716c);
  }
  /* Work and history side by side: the adjacency is the point. */
  .sr-panes {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0;
  }
  @media (max-width: 800px) {
    .sr-panes {
      grid-template-columns: 1fr;
    }
  }
  .sr-pane {
    padding: 10px 16px 14px;
  }
  .sr-pane + .sr-pane {
    border-left: 1px solid var(--border, #e7e5e4);
  }
  @media (max-width: 800px) {
    .sr-pane + .sr-pane {
      border-left: none;
      border-top: 1px solid var(--border, #e7e5e4);
    }
  }
  .sr-pane-label {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-dim, #78716c);
    margin-bottom: 6px;
  }
  .sr-line {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    padding: 5px 6px;
    margin: 0 -6px;
    border-radius: 4px;
    text-decoration: none;
    color: var(--text, #1c1917);
    font-size: 13px;
  }
  .sr-line:not(.sr-line-static):hover {
    background: var(--bg, #f5f5f4);
  }
  .sr-line-title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sr-line-sub {
    color: var(--text-dim, #78716c);
    font-size: 11px;
    white-space: nowrap;
  }
  .sr-none {
    font-size: 12px;
    color: var(--text-dim, #78716c);
    margin: 0;
  }
  .sr-group {
    margin: 18px 0;
  }
  .sr-group-title {
    font-size: 13px;
    margin: 0 0 6px;
  }
</style>
