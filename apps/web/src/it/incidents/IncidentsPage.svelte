<script lang="ts">
  // /it/incidents — the IT incidents surface.
  //
  // David: "Do we have a good surface for IT to view post mortems more
  // durably? I think we probably need a new 'Incidents' page that is
  // both where we respond to active incidents and document post
  // mortems for posterity." Two panels answer the two halves:
  //
  //   * Active incidents — every open incident-post-mortem packet,
  //     with a compact strip of its step states, who holds the current
  //     step, and the link into the packet where the response work
  //     actually happens. This panel is a lens over the queue, not a
  //     second place to work — the packet is.
  //   * Post-mortem archive — every closed packet rendered as a
  //     readable document, newest first, each with the terminal it
  //     ended on. This is the "for posterity" half: the packet's
  //     metadata IS the post-mortem, and the renderer is
  //     semi-structured (postMortemDoc.ts) because the live packets
  //     already carry two different shapes.
  //
  // Failure renders as failure (packet 3fba9c35, the false-empty
  // sweep): an incidents page that reports calm during an outage is
  // the worst possible incidents page.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { href } from '../../router';
  import type { Job, Step } from '../../jobs/types';
  import { closedOutcome, incidentAt, postMortemSections } from './postMortemDoc';

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'failed'; message: string }
    | { kind: 'ready'; jobs: ReadonlyArray<Job> };

  let load = $state<LoadState>({ kind: 'loading' });

  async function fetchIncidents(): Promise<void> {
    load = { kind: 'loading' };
    try {
      const res = await fetch('/api/jobs?kind=incident-post-mortem&limit=200');
      if (!res.ok) throw new Error(`incident-post-mortem jobs: HTTP ${res.status}`);
      const body = await res.json();
      const jobs = (Array.isArray(body) ? body : (body.data ?? [])) as Job[];
      load = { kind: 'ready', jobs };
    } catch (e) {
      load = { kind: 'failed', message: e instanceof Error ? e.message : String(e) };
    }
  }
  onMount(fetchIncidents);

  const jobs = $derived(load.kind === 'ready' ? load.jobs : []);

  /// Open packets, newest declaration first — the response queue.
  const active = $derived(
    [...jobs.filter((j) => j.status !== 'closed' && j.status !== 'cancelled')].sort(
      (a, b) => (b.opened_on ?? '').localeCompare(a.opened_on ?? ''),
    ),
  );

  /// Closed packets, newest closure first — the durable record.
  const archive = $derived(
    [...jobs.filter((j) => j.status === 'closed' || j.status === 'cancelled')].sort(
      (a, b) => (b.closed_on ?? '').localeCompare(a.closed_on ?? ''),
    ),
  );

  const stepsOf = (j: Job): ReadonlyArray<Step> =>
    [...(j.steps ?? [])].sort((a, b) => a.sort_order - b.sort_order);

  /// The steps a responder can act on right now, with their holders.
  const workable = (j: Job): ReadonlyArray<Step> =>
    stepsOf(j).filter((s) => s.status === 'ready' || s.status === 'active');

  const doneCount = (j: Job): number =>
    stepsOf(j).filter((s) => s.status === 'completed' || s.status === 'skipped').length;

  /// Header keys the document body must not repeat.
  const HEADER_KEYS = ['incident_at', 'incident_date', 'outcome'] as const;
</script>

<PageHeader
  title="Incidents"
  subtitle="Respond to active incidents, and read the post-mortems for posterity. Every incident is an incident-post-mortem packet — this page is the lens over that queue."
/>

{#if load.kind === 'loading'}
  <p class="inc-msg">Loading incidents…</p>
{:else if load.kind === 'failed'}
  <div class="inc-failed" role="alert">
    <p class="inc-failed-text load-failed">
      Could not load the incident queue — {load.message}. This page will not guess:
      an unreadable queue is not an empty one.
    </p>
    <button class="inc-btn" type="button" onclick={fetchIncidents}>Retry</button>
  </div>
{:else}
  <section class="inc-active" aria-label="Active incidents">
    <h2 class="inc-h2">Active incidents <span class="inc-count">{active.length}</span></h2>
    {#if active.length === 0}
      <p class="inc-empty">No active incidents — nothing needs a response right now.</p>
    {:else}
      {#each active as j (j.id)}
        <article class="inc-card">
          <header class="inc-card-head">
            <h3 class="inc-card-title">{j.title}</h3>
            {#if incidentAt(j.metadata)}
              <span class="inc-when">{incidentAt(j.metadata)}</span>
            {/if}
            <a class="inc-open" href={href(`/jobs/${j.id}`)}>Open packet →</a>
          </header>

          <!-- The compact step-state strip: one segment per step, in
               workflow order, coloured by its status. Hover a segment
               for the step's title, state and holder. -->
          <div class="inc-strip" role="img" aria-label="{doneCount(j)} of {stepsOf(j).length} steps completed">
            {#each stepsOf(j) as s (s.id)}
              <span
                class="inc-strip-step inc-strip-{s.status}"
                title="{s.title} — {s.status}{s.assignee_id ? ` · ${s.assignee_id}` : ''}"
              ></span>
            {/each}
            <span class="inc-strip-count">{doneCount(j)}/{stepsOf(j).length} steps done</span>
          </div>

          {#if workable(j).length > 0}
            <p class="inc-now">
              Now:
              {#each workable(j) as s, i (s.id)}
                {i > 0 ? ' · ' : ''}<strong>{s.title}</strong>
                {s.assignee_id ? `(${s.assignee_id})` : '(unassigned)'}
              {/each}
            </p>
          {/if}
        </article>
      {/each}
    {/if}
  </section>

  <section class="inc-archive" aria-label="Post-mortem archive">
    <h2 class="inc-h2">Post-mortem archive <span class="inc-count">{archive.length}</span></h2>
    {#if archive.length === 0}
      <p class="inc-empty">
        No post-mortems archived yet. Closed incident packets land here as durable
        documents.
      </p>
    {:else}
      {#each archive as j (j.id)}
        <article class="inc-doc">
          <header class="inc-doc-head">
            <h3 class="inc-doc-title">{j.title}</h3>
            <div class="inc-doc-meta">
              {#if incidentAt(j.metadata)}
                <span class="inc-when">{incidentAt(j.metadata)}</span>
              {/if}
              <span class="inc-closed">closed {j.closed_on ?? '—'}</span>
              {#if closedOutcome(j)}
                <span class="inc-outcome">{closedOutcome(j)}</span>
              {/if}
              <a class="inc-open" href={href(`/jobs/${j.id}`)}>packet →</a>
            </div>
          </header>
          {#each postMortemSections(j.metadata, HEADER_KEYS) as section (section.key)}
            <section class="inc-section">
              <h4 class="inc-section-label">{section.label}</h4>
              <p class="inc-section-body">{section.body}</p>
            </section>
          {:else}
            <p class="inc-empty">This packet closed without recorded findings.</p>
          {/each}
        </article>
      {/each}
    {/if}
  </section>
{/if}

<style>
  .inc-h2 {
    font-size: 15px;
    margin: var(--s5) 0 var(--s3);
    display: flex;
    align-items: baseline;
    gap: var(--s2);
  }
  .inc-count {
    font: 500 12px var(--font-mono);
    color: var(--static);
  }
  .inc-msg {
    color: var(--text-dim);
    font-size: 14px;
  }
  .inc-empty {
    color: var(--text-dim);
    font-size: 13px;
    font-style: italic;
    margin: 0 0 var(--s3);
  }

  /* Failure is a first-class state, visually distinct from empty. */
  .inc-failed {
    border: 1px solid var(--err);
    border-left: 3px solid var(--err);
    padding: var(--s3) var(--s4);
    display: flex;
    align-items: center;
    gap: var(--s4);
    flex-wrap: wrap;
  }
  .inc-failed-text {
    margin: 0;
    font-size: 13px;
    color: var(--err);
    flex: 1 1 24ch;
  }
  .inc-btn {
    font: inherit;
    font-size: 12px;
    padding: 4px 14px;
    border: 1px solid var(--border);
    background: transparent;
    color: inherit;
    cursor: pointer;
  }
  .inc-btn:hover {
    border-color: var(--signal);
    color: var(--signal);
  }

  /* --- Active cards ------------------------------------------------- */
  .inc-card {
    background: var(--card);
    border: 1px solid var(--border);
    border-left: 3px solid var(--warn);
    padding: var(--s3) var(--s4);
    margin-bottom: var(--s3);
    display: flex;
    flex-direction: column;
    gap: var(--s2);
  }
  .inc-card-head {
    display: flex;
    align-items: baseline;
    gap: var(--s3);
    flex-wrap: wrap;
  }
  .inc-card-title {
    margin: 0;
    font-size: 14px;
    flex: 1 1 auto;
  }
  .inc-when {
    font: 400 11px var(--font-mono);
    color: var(--static);
  }
  .inc-open {
    font-size: 12px;
    white-space: nowrap;
  }

  .inc-strip {
    display: flex;
    align-items: center;
    gap: 3px;
  }
  .inc-strip-step {
    width: 18px;
    height: 8px;
    border: 1px solid var(--border);
    background: transparent;
  }
  .inc-strip-completed {
    background: var(--ok);
    border-color: var(--ok);
  }
  .inc-strip-skipped {
    background: var(--hairline);
    border-color: var(--hairline);
  }
  .inc-strip-active {
    background: var(--warn);
    border-color: var(--warn);
  }
  .inc-strip-ready {
    border-color: var(--signal);
  }
  .inc-strip-count {
    margin-left: var(--s2);
    font: 400 11px var(--font-mono);
    color: var(--static);
  }
  .inc-now {
    margin: 0;
    font-size: 12px;
    color: var(--text-dim);
  }
  .inc-now strong {
    color: var(--text);
    font-weight: 600;
  }

  /* --- Archive documents -------------------------------------------- */
  .inc-doc {
    background: var(--card);
    border: 1px solid var(--border);
    padding: var(--s4) var(--s5);
    margin-bottom: var(--s4);
    max-width: 78ch;
  }
  .inc-doc-head {
    border-bottom: 1px solid var(--border);
    padding-bottom: var(--s2);
    margin-bottom: var(--s3);
  }
  .inc-doc-title {
    margin: 0 0 var(--s1);
    font-size: 14px;
  }
  .inc-doc-meta {
    display: flex;
    align-items: baseline;
    gap: var(--s3);
    flex-wrap: wrap;
    font-size: 11px;
    color: var(--text-dim);
  }
  .inc-closed {
    font: 400 11px var(--font-mono);
  }
  .inc-outcome {
    font: 500 11px var(--font-mono);
    color: var(--ok);
    border: 1px solid var(--ok);
    padding: 0 6px;
  }
  .inc-section {
    margin: 0 0 var(--s3);
  }
  .inc-section-label {
    margin: 0 0 var(--s1);
    font: 400 11px var(--font-mono);
    letter-spacing: var(--ls-label);
    text-transform: uppercase;
    color: var(--static);
  }
  .inc-section-body {
    margin: 0;
    font-size: 13px;
    line-height: 1.6;
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
