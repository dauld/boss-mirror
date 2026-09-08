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
  // The doc corpus and the indexer's rejections stay their own reads:
  // they describe docs that have NO packet yet, which is exactly the
  // set you need in order to START a review, and they are
  // boss-docs-api's to serve. The station registry's business is the
  // queue and how it is framed.
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { href, navigate } from '../../router';
  import { groupDocs, openWeight } from './designGroups';
  import {
    pageHeader,
    panelsFor,
    reviewHref,
    reviewsByDocPath,
    REVIEW_STEP_KIND,
    type DesignQueueEnvelope,
    type ReviewPacket,
  } from './designLens';

  type DesignDoc = {
    path: string;
    title: string;
    status: string;
    /// Questions currently parsed from the doc's ## Open questions.
    open_questions: number;
    /// Decisions recorded in review but not yet flushed to git.
    pending_count: number;
    word_count: number;
    last_modified: string;
  };

  type Rejection = {
    path: string;
    reason: string;
    first_seen_at: string;
    last_seen_at: string;
  };

  type StaleStatus = {
    path: string;
    title: string;
    status: string;
    reason: string;
  };

  let docs = $state<ReadonlyArray<DesignDoc>>([]);
  // Docs on disk that are NOT in the list below, and why. Empty is the
  // healthy state. Without this the panel silently showed a partial
  // corpus: a rejected doc has no design_docs row, so its absence read
  // as "nobody wrote it" — which is how transactional-audit-log.md
  // stayed invisible for six days.
  let rejections = $state<ReadonlyArray<Rejection>>([]);
  // The quieter sibling of a rejection. A rejected doc is missing; a
  // doc whose status drifted is present and lying — it says "in
  // review" with nothing left to review. Eleven of the twenty docs
  // claiming to be live were in that state on 2026-08-15, every one
  // wrong in the same direction, and nothing surfaced it (0b8ae875).
  let staleStatuses = $state<ReadonlyArray<StaleStatus>>([]);
  let queue = $state<DesignQueueEnvelope | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  const header = $derived(pageHeader(queue?.lens));
  const panels = $derived(panelsFor(queue?.lens));
  const openReviewsByPath = $derived(reviewsByDocPath(queue?.data ?? []));

  // System actor for opening review Jobs — same shape inventory-api
  // uses for its system-initiated Job opens.
  const SYSTEM_USER = JSON.stringify({
    id: 'system',
    role: 'platform-admin',
    access_tier: 'operator',
    territory_account_ids: [],
    direct_report_ids: [],
    department: null,
  });

  /// Whole days a doc has been out of the tracker. The age is what
  /// makes a rejection actionable — "failed" invites a shrug, "absent
  /// for 6 days" does not.
  function daysSince(iso: string): number {
    const then = new Date(iso).getTime();
    if (Number.isNaN(then)) return 0;
    return Math.max(0, Math.floor((Date.now() - then) / 86_400_000));
  }

  async function load(): Promise<void> {
    loading = true;
    error = null;
    try {
      // The queue IS the page — if this read fails the surface has
      // nothing honest to show, so it is the one that throws.
      const queueResp = await fetch('/api/stations/design-review/queue');
      if (!queueResp.ok) throw new Error(`queue: HTTP ${queueResp.status}`);
      queue = (await queueResp.json()) as DesignQueueEnvelope;

      const docsResp = await fetch('/api/design/docs');
      if (!docsResp.ok) throw new Error(`docs: HTTP ${docsResp.status}`);
      docs = (await docsResp.json()) as DesignDoc[];

      // Rejections are supplementary — they name docs the indexer
      // could not parse. If that call fails, the page still has
      // everything an operator came for, so degrade to an empty list
      // rather than replacing the whole surface with an error. (It
      // did throw here once, which blanked the page whenever the
      // route was unavailable.)
      rejections = await fetch('/api/design/rejections')
        .then((r) => (r.ok ? (r.json() as Promise<Rejection[]>) : []))
        .catch(() => []);
      // Same degrade-to-empty contract as rejections above: a report
      // about the corpus must never be able to blank the corpus.
      staleStatuses = await fetch('/api/design/stale-statuses')
        .then((r) => (r.ok ? (r.json() as Promise<StaleStatus[]>) : []))
        .catch(() => []);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  /// The `review-design` step of an open packet, resolved on demand.
  ///
  /// The station queue serves packets without steps (it fetches them
  /// only when the predicate reads step state, and this station's
  /// predicate is a kind match), so the step id is one read at click
  /// time for the ONE packet being opened. The page used to enrich
  /// every packet with its steps on load to find the same id.
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
    navigate(reviewHref(packet.id, await reviewStepId(packet.id)));
  }

  /// One action, one destination: the review surface. Whether a review
  /// Job already exists is an implementation detail, and surfacing it
  /// as the difference between a link and a button made the Review
  /// column read as a status field that sometimes happened to be
  /// clickable (David, 2026-08-14: "that link should just consistently
  /// launch the review UX"). Creating the Job when there isn't one is
  /// a step on the way, not a different outcome.
  async function openReview(doc: DesignDoc): Promise<void> {
    // Already in the station's queue — go straight in. Posting again
    // would open a second packet for the same doc.
    const existing = openReviewsByPath[doc.path];
    if (existing) {
      await enterReview(existing);
      return;
    }
    const body = {
      kind: 'design-doc-review',
      // Identity-first Subject: the doc path IS the subject id. The
      // pre-2026-06-13 {custom_kind, ref_id} shape 422s ("missing
      // field `id`") — this page shipped before that migration and
      // the button was dead until 2026-07-06.
      subject: {
        subject_kind: 'custom',
        id: doc.path,
      },
      title: `Review: ${doc.title}`,
      owner_id: 'system',
      priority: 'standard',
      status: 'open',
      metadata: {
        doc_path: doc.path,
        doc_title: doc.title,
      },
      tags: ['design-review'],
    };
    try {
      const resp = await fetch('/api/jobs', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'x-boss-user': SYSTEM_USER,
        },
        body: JSON.stringify(body),
      });
      if (!resp.ok) throw new Error(`HTTP ${resp.status}: ${await resp.text()}`);
      const created: { id?: string } = await resp.json().catch(() => ({}));
      // doc_path is stamped at materialization from the Job's subject
      // (the Workflow's metadata_defaults template `{subject.id}`) — no
      // follow-up PUT. The old fill-in write lost read-overlay-write
      // races against dispatcher assignment and workforce completion,
      // and terminal-metadata immutability then sealed the empty value
      // (the 2026-07-14 "doc_path is empty" incident).
      await load();
      // Open the review where it is readable. Creating the Job and
      // dropping the operator back on a table row means the next
      // click lands on the job page, which renders the document in a
      // panel beside the sidebar and step list — the reason reviewing
      // a doc in-app felt cramped.
      const opened = openReviewsByPath[doc.path];
      if (opened) {
        await enterReview(opened);
        return;
      }
      // The reload did not see the new packet yet (steps materialize
      // asynchronously, and the station evaluates over what exists
      // when it is asked). Use the id the POST just returned rather
      // than leaving the operator on the table wondering whether the
      // click worked — a click that creates a Job and goes nowhere is
      // the inconsistency this function exists to remove.
      if (created.id) navigate(reviewHref(created.id, await reviewStepId(created.id)));
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  $effect(() => {
    void load();
  });

  // What is actually asking for something, versus what merely claims
  // to be. See designGroups.ts — the short version is that a doc's
  // `**Status**:` line is the one input nobody updates when the last
  // question closes, so it drifts stale in one direction and eleven
  // settled docs had accumulated in the top section (David, bedda461:
  // "This page is full of stale info").
  const grouped = $derived(
    groupDocs(docs, (path) => openReviewsByPath[path] !== undefined),
  );
  const waiting = $derived(openWeight(grouped.needsYou));

  function relTime(iso: string): string {
    const d = new Date(iso);
    const now = new Date();
    const days = Math.floor((now.getTime() - d.getTime()) / 86_400_000);
    if (days < 1) return 'today';
    if (days === 1) return '1d ago';
    if (days < 30) return `${days}d ago`;
    if (days < 365) return `${Math.floor(days / 30)}mo ago`;
    return `${Math.floor(days / 365)}y ago`;
  }
</script>

<PageHeader eyebrow={header.eyebrow} title={header.title} subtitle={header.subtitle} />

{#if loading}
  <p class="empty">Loading the review queue…</p>
{:else if error}
  <p class="design-error">Error: {error}</p>
{:else}
  {#each panels as panel (panel)}
    {#if panel === 'rejections' && staleStatuses.length > 0}
      <Section title={`Status drifted (${staleStatuses.length})`} wide>
        <p class="reject-lede">
          These docs say they are under discussion but have no open
          questions. The status line is hand-written and almost
          nothing updates it, so it goes stale by default — a doc that
          reads <code>in-review</code> here may simply be finished.
          Not an error: a doc can legitimately wait on a person with
          nothing registered. It is a prompt to check.
        </p>
        <table class="design-table">
          <thead>
            <tr><th>Doc</th><th>Says</th><th>Why it looks wrong</th></tr>
          </thead>
          <tbody>
            {#each staleStatuses as d (d.path)}
              <tr>
                <td><code>{d.path}</code></td>
                <td class="reject-age">{d.status}</td>
                <td class="reject-reason">{d.reason}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </Section>
    {/if}
    {#if panel === 'rejections' && rejections.length > 0}
      <Section title={`Not indexed (${rejections.length})`} wide>
        <p class="reject-lede">
          These files are in <code>docs/design/</code> but are <strong>not</strong>
          in the lists below — the reindexer refused them. Until each is
          fixed, this panel is showing an incomplete corpus.
        </p>
        <table class="design-table">
          <thead>
            <tr><th>Doc</th><th>Invisible for</th><th>Why</th></tr>
          </thead>
          <tbody>
            {#each rejections as r (r.path)}
              <tr>
                <td><code>{r.path}</code></td>
                <td class="reject-age">
                  {daysSince(r.first_seen_at)}
                  {daysSince(r.first_seen_at) === 1 ? 'day' : 'days'}
                </td>
                <td class="reject-reason">{r.reason}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </Section>
    {:else if panel === 'corpus'}
      <Section
        title={`Needs you (${grouped.needsYou.length})`}
        wide
      >
        {#if grouped.needsYou.length === 0}
          <p class="empty">
            Nothing is waiting on a decision. New questions land here
            when a doc adds <code>### Qn:</code> headings, and recorded
            answers land here until they are flushed into the doc.
          </p>
        {:else}
          <p class="design-lede">
            {waiting.questions}
            {waiting.questions === 1 ? 'open question' : 'open questions'}{#if waiting.pending > 0},
              and {waiting.pending} recorded
              {waiting.pending === 1 ? 'answer' : 'answers'} not yet flushed into
              {waiting.pending === 1 ? 'its doc' : 'their docs'}{/if}. Deepest first.
          </p>
          {@render docTable(grouped.needsYou, 'Start review')}
        {/if}
      </Section>

      {#if grouped.drafts.length > 0}
        <Section title={`Being written (${grouped.drafts.length})`} wide>
          <p class="design-lede">
            Drafts with nothing to decide yet. They are not settled —
            they just have not asked anything.
          </p>
          {@render docTable(grouped.drafts, 'Start review')}
        </Section>
      {/if}

      <Section title={`Design library (${grouped.library.length})`} wide>
        <!-- The settled corpus, and the pointer David asked for. The
             flattened record is NOT served in-app: it folds into
             docs/architecture-decisions.md each release, and the IT
             Knowledge Base is where that is explained. Linking to the
             page that tells the truth about where it lives beats
             inventing a URL for a file the SPA does not serve. -->
        <p class="design-lede">
          Living references and finished discussions — nothing here is
          waiting on anyone. Settled decisions fold into
          <code>docs/architecture-decisions.md</code>, the one
          current-truth record;
          <a
            href={href('/it/kb')}
            onclick={(e) => { e.preventDefault(); navigate(href('/it/kb')); }}
          >the IT Knowledge Base</a>
          is the in-app entry point.
        </p>
        {@render docTable(grouped.library, 'Reopen discussion')}
      </Section>
    {/if}
  {/each}
{/if}

{#snippet docTable(rows: ReadonlyArray<DesignDoc>, buttonLabel: string)}
  <table class="design-table">
    <thead>
      <tr>
        <th>Doc</th>
        <th>Status</th>
        <th>Open Qs</th>
        <th>Pending decisions</th>
        <th>Last modified</th>
        <th>Review</th>
      </tr>
    </thead>
    <tbody>
      {#each rows as doc (doc.path)}
        {@const review = openReviewsByPath[doc.path]}
        <tr>
          <td>
            <strong>{doc.title}</strong>
            <div class="design-path">{doc.path}</div>
          </td>
          <td class="design-status">{doc.status}</td>
          <td>{doc.open_questions}</td>
          <td>{doc.pending_count}</td>
          <td class="design-when">{relTime(doc.last_modified)}</td>
          <td>
            <!-- One affordance, one destination. This column used to
                 fork: a doc with a Job rendered a text link labelled
                 "In review — open", and one without rendered a button
                 — so the same column carried what looked like a status
                 in some rows and an action in others, and only one of
                 them reliably navigated. Both go to the review surface
                 now; the packet's state is reported below the control
                 instead of impersonating it. -->
            <button class="wb-btn" type="button" onclick={() => openReview(doc)}>
              {review ? 'Review' : buttonLabel}
            </button>
            {#if review}
              <div class="design-when">In queue · {review.status}</div>
            {/if}
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
{/snippet}

<style>
  /* Warning prose, not an empty-state: FOG at reading line-height. It was
     STATIC via `.empty`, which buried the one paragraph explaining why the
     corpus above is incomplete. */
  .reject-lede {
    color: var(--fog, #E8ECEF);
    line-height: 1.6;
    max-width: 720px;
    margin: 0 0 12px;
  }
  .reject-age {
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  /* Body prose in a cell. 0.85rem was 11.9px at the 14px root — below the
     13px body floor — with cramped leading. */
  .reject-reason {
    font-size: 13px;
    line-height: 1.6;
    max-width: 60ch;
  }
  .design-table {
    width: 100%;
    border-collapse: collapse;
  }
  .design-table th,
  .design-table td {
    text-align: left;
    padding: 8px 12px;
    border-bottom: 1px solid var(--hairline, #2A3138);
    vertical-align: top;
    font-variant-numeric: tabular-nums;
  }
  /* Column labels are instrument text: DM Mono caps in STATIC, not bold
     browser-default headers competing with the rows. Yard-board idiom. */
  .design-table th {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
    font-weight: 400;
    letter-spacing: var(--ls-nav, 0.14em);
    text-transform: uppercase;
    color: var(--static, #7A838C);
  }
  .design-table tr:last-child td {
    border-bottom: none;
  }
  .design-status {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
    letter-spacing: var(--ls-label, 0.1em);
    text-transform: uppercase;
    color: var(--static, #7A838C);
    white-space: nowrap;
  }
  .design-when {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px;
    color: var(--static, #7A838C);
    white-space: nowrap;
  }
  .design-path {
    color: var(--static, #7A838C);
    font-size: 12px;
    font-family: var(--font-mono, ui-monospace, monospace);
    margin-top: 2px;
  }
  /* Inline literals (paths, `### Qn:` markers) in the system mono, pinned
     to 12px — bare <code> falls into the browser's monospace-shrink. */
  code {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px;
  }
  .empty {
    color: var(--static, #7A838C);
    margin: 12px 0;
    line-height: 1.5;
  }
  .design-error {
    color: var(--err, #e2685c);
    margin: 12px 0;
  }
</style>
