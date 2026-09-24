<script lang="ts">
  // /it/design — the design-review station, rendered.
  //
  // This page is a LENS: the packets are the `design-review` station's
  // evaluated queue, and the page's own identity (header, panel set)
  // is the `lens` its registry row declares. It used to define the
  // queue itself — `/api/jobs?kind=design-doc-review&status=open`
  // filtered in the browser — which is two definitions of one queue,
  // drifting silently. See `designLens.ts` for the full reasoning.
  //
  // IT IS NOW ONLY THAT, and the change is a deletion. The page used
  // to render a second thing beside the queue: a table of the markdown
  // files under docs/design/, read from the corpus endpoint on
  // boss-docs-api, with a button that opened a `design-doc-review` Job
  // per file. That service and the corpus index behind it were deleted
  // on 2026-09-10 (backlog f5da586c) — the packet is the doc, so a
  // list of files is not a list of work.
  //
  // What that removed was not only dead code. The corpus table WAS the
  // page; the queue was fetched and then used for nothing a reader
  // could see, because the only consumer was a join on `subject.id` =
  // doc path, and a `design-doc` packet's subject is the literal
  // `boss-platform`. So the station's real packets — the design docs
  // actually waiting on a decision — rendered nowhere, while files
  // nobody had filed anything about rendered as rows with a Start
  // button. The page now shows the queue it was always fetching.
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { navigate } from '../../router';
  import DecidedDesigns from './DecidedDesigns.svelte';
  import {
    pageHeader,
    panelsFor,
    progressLabel,
    queueRows,
    relTime,
    reviewHref,
    REVIEW_STEP_KIND,
    type DesignQueueEnvelope,
    type ReviewPacket,
  } from './designLens';

  let queue = $state<DesignQueueEnvelope | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  const header = $derived(pageHeader(queue?.lens));
  const panels = $derived(panelsFor(queue?.lens));
  const rows = $derived(queueRows(queue?.data ?? [], queue?.steps));

  async function load(): Promise<void> {
    loading = true;
    error = null;
    try {
      // One read, and it is the queue. If it fails the surface has
      // nothing honest to show, so it throws rather than rendering an
      // empty table that reads as "nothing to review".
      const resp = await fetch('/api/stations/design-review/queue');
      if (!resp.ok) throw new Error(`queue: HTTP ${resp.status}`);
      queue = (await resp.json()) as DesignQueueEnvelope;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  /// The `review-design` step of an open packet, resolved on demand.
  ///
  /// Since design-review v2 (with_steps, 2026-09-24) the envelope carries
  /// the step and `enterReview` uses it; this read is the fallback for a
  /// registry still at v1, which serves packets without steps, so the
  /// step id is one read at click time for the ONE packet being opened.
  ///
  /// A failure here is not an error state: `reviewHref` falls back to
  /// the job page, which is a worse door but a real one.
  async function reviewStepId(jobId: string): Promise<string | null> {
    try {
      const r = await fetch(`/api/jobs/${jobId}`);
      if (!r.ok) return null;
      const job = (await r.json()) as { steps?: Array<{ id: string; kind: string }> };
      return job.steps?.find((s) => s.kind === REVIEW_STEP_KIND)?.id ?? null;
    } catch {
      return null;
    }
  }

  async function enterReview(packet: ReviewPacket): Promise<void> {
    navigate(reviewHref(packet.id, packet.reviewStepId ?? (await reviewStepId(packet.id))));
  }

  $effect(() => {
    void load();
  });
</script>

<PageHeader eyebrow={header.eyebrow} title={header.title} subtitle={header.subtitle} />

{#if loading}
  <p class="empty">Loading the review queue…</p>
{:else if error}
  <!-- The shared failure marker (sweep c3e4edcc) draws the failed read's
       rail; this page's own class keeps only the spacing. -->
  <p class="design-error load-failed" role="alert">
    The review queue could not be read: {error}. This is not an empty queue.
  </p>
{:else}
  {#each panels as panel (panel)}
    {#if panel === 'queue'}
      <Section title={`Waiting on a decision (${rows.length})`} wide>
        {#if rows.length === 0}
          <p class="empty">
            Nothing is waiting on a decision. A design doc reaches this
            queue as a <code>design-doc</code> packet carrying its own
            prose and questions — <code>boss design</code> files one.
            Settled material folds into
            <code>docs/architecture-decisions.md</code>, the one
            current-truth record.
          </p>
        {:else}
          <p class="design-lede">
            In the station's order: priority, then age. The first row is
            the one it would hand out next.
          </p>
          <table class="design-table">
            <thead>
              <tr>
                <th>Packet</th>
                <th>Status</th>
                <th>Answers</th>
                <th>Opened</th>
                <th>Review</th>
              </tr>
            </thead>
            <tbody>
              {#each rows as packet (packet.id)}
                <tr>
                  <td><strong>{packet.title}</strong></td>
                  <td class="design-status">{packet.status}</td>
                  <td
                    class="design-progress"
                    class:design-saved={packet.progress.kind === 'saved'}
                  >
                    {progressLabel(packet.progress)}
                  </td>
                  <td class="design-when">{relTime(packet.opened_on)}</td>
                  <td>
                    <button
                      class="wb-btn"
                      type="button"
                      onclick={() => enterReview(packet)}
                    >
                      Review
                    </button>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </Section>
    {:else if panel === 'decided'}
      <DecidedDesigns />
    {/if}
  {/each}
{/if}

<style>
  .design-table {
    width: 100%;
    border-collapse: collapse;
  }
  .design-table th,
  .design-table td {
    text-align: left;
    padding: 8px 12px;
    border-bottom: 1px solid var(--hairline);
    vertical-align: top;
    font-variant-numeric: tabular-nums;
  }
  /* Column labels are instrument text: DM Mono caps in STATIC, not bold
     browser-default headers competing with the rows. Yard-board idiom. */
  .design-table th {
    font-family: var(--font-mono);
    font-size: 11px;
    font-weight: 400;
    letter-spacing: var(--ls-nav);
    text-transform: uppercase;
    color: var(--static);
  }
  .design-table tr:last-child td {
    border-bottom: none;
  }
  .design-status {
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: var(--ls-label);
    text-transform: uppercase;
    color: var(--static);
    white-space: nowrap;
  }
  .design-progress {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--static);
    white-space: nowrap;
  }
  /* A half-made decision is work in hand — it must not read like an
     untouched row (backlog 08372fdb). */
  .design-saved {
    color: var(--warn);
  }
  .design-when {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--static);
    white-space: nowrap;
  }
  /* Inline literals (paths, verbs) in the system mono, pinned to 12px —
     bare <code> falls into the browser's monospace-shrink. */
  code {
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .empty {
    color: var(--static);
    margin: 12px 0;
    line-height: 1.5;
  }
  .design-lede {
    color: var(--static);
    margin: 0 0 12px;
  }
  .design-error {
    margin: 12px 0;
  }
</style>
