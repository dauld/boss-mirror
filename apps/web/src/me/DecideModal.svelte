<script lang="ts">
  // The decision modal (David, feedback 0ab5fa3a): "For steps that are
  // an official question and response, let's create a nice step UX
  // modal where I can quickly see the question and prepared context and
  // then have buttons to select along with the potentially optional
  // (protocol dependent) response field."
  //
  // The question, the context panel, the verdict buttons, and whether
  // the response field is required all already live in each kind's
  // surface — plugin row or platform surface, dispatched by
  // StepSurface. What was missing is the trip: deciding meant leaving
  // the queue for the job page and finding your way back, once per
  // verdict. This overlay mounts the SAME surface over My Day, so a
  // docket of sign-offs is click → decide → next, and the queue is
  // still there behind each one.
  //
  // Deliberately a frame, not a surface: it renders no fields of its
  // own, so a protocol with a custom bundle gets its bundle here too,
  // and one that grows a new required field never has a second copy of
  // that rule to drift (CLAUDE.md 9a).
  import StepSurface from '../steps/StepSurface.svelte';
  import { navigate } from '../router';
  import { stepFocusHref } from './decideHref';
  import type { StepStatus } from '../jobs/types';

  type Step = {
    id: string;
    kind: string;
    title: string;
    status: string;
    assignee_id: string | null;
    sort_order: number;
    metadata: Record<string, unknown> | null;
    notes: string | null;
  };

  let { jobId, stepId, onClose, onDecided } = $props<{
    jobId: string;
    stepId: string;
    onClose: () => void;
    /** The step reached a terminal status inside the modal — the row
     *  is no longer a verdict, so the queue behind it must refetch. */
    onDecided: () => void;
  }>();

  let job = $state<{ title: string; metadata?: Record<string, unknown> } | null>(null);
  let step = $state<Step | null>(null);
  let error = $state<string | null>(null);
  // A soft aside ("saved, but the re-read failed"), not an error: the
  // render puts `error` before the step, so an error here would hide a
  // surface that is busy showing its receipt.
  let note = $state<string | null>(null);

  // The assignment row travels light; the surface needs the step's
  // full metadata. Fetch fresh rather than trusting a row that may be
  // a poll interval old — deciding on stale metadata is deciding on
  // evidence somebody since replaced.
  //
  // Returns whether the load landed. Each entry clears the previous
  // failure first: `error` renders INSTEAD of the surface, so a stale
  // one would poison the modal until close+reopen (2026-08-21 UX
  // audit) — a load that is about to succeed must not lose to a blip
  // that already passed.
  async function load(): Promise<boolean> {
    error = null;
    try {
      const r = await fetch(`/api/jobs/${encodeURIComponent(jobId)}`);
      if (!r.ok) throw new Error(`job: HTTP ${r.status}`);
      const body = (await r.json()) as {
        title?: string;
        metadata?: Record<string, unknown>;
        steps?: Step[];
      };
      job = { title: body.title ?? 'Packet', metadata: body.metadata };
      const found = (body.steps ?? []).find((s) => s.id === stepId) ?? null;
      if (!found) throw new Error('this step is no longer part of the packet');
      step = found;
      return true;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      return false;
    }
  }

  // After a surface saves: reload, and if the step left the queue's
  // definition of "waiting on you", tell the page. NOT auto-closing —
  // surfaces render their own receipt ("Answered — approved…"), and
  // yanking the panel mid-read would spend the trust the receipt buys.
  async function onSurfaceUpdate(): Promise<void> {
    if (await load()) {
      note = null;
      const s = step?.status;
      if (s === 'completed' || s === 'skipped') onDecided();
      return;
    }
    // The reload failed AFTER the surface reported a successful save.
    // The old behaviour stranded the row: `step` kept its pre-save
    // status, the completed-check never fired, onDecided never ran —
    // so a SUCCESSFUL decide stayed in "Yours to decide" and deciding
    // it again PUT completed over it (2026-08-21 UX audit). Be
    // conservative the cheap way round: the surface said something
    // changed, so tell the page to refetch its queues (correct even if
    // the step did not complete), and say what happened as an aside
    // rather than poisoning the modal that is showing its receipt.
    error = null;
    note = 'Saved — but the step could not be re-read. The queue is refreshing.';
    onDecided();
  }

  void load();

  // Escape-to-close via $effect, NOT the svelte window tag — the
  // bun+svelte bundler crashes on that event lookup (PacketModal
  // documents the trap; no-svelte-window.test.ts pins it).
  $effect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === 'Escape') onClose();
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  });

  const brief = $derived.by(() => {
    const m = job?.metadata;
    const msg = m?.message ?? m?.body;
    return typeof msg === 'string' && msg.trim() ? msg : null;
  });

  function surfaceStep(s: Step) {
    return { ...s, status: s.status as StepStatus, metadata: s.metadata ?? {} };
  }
</script>

<div
  class="dm-back"
  role="presentation"
  onclick={(e) => {
    if (e.target === e.currentTarget) onClose();
  }}
>
  <div class="dm" role="dialog" aria-modal="true" aria-label={step?.title ?? 'Decide'}>
    <header class="dm-head">
      <div class="dm-head-text">
        <h2 class="dm-title">{step?.title ?? 'Loading…'}</h2>
        {#if job}<p class="dm-sub">{job.title}</p>{/if}
      </div>
      <!-- A design review is a document; the modal can be the wrong
           size for one. The full page stays a click away and its Back
           returns here, to My Day — not to the job page. -->
      <button
        class="dm-btn"
        type="button"
        onclick={() => navigate(stepFocusHref(jobId, stepId))}
      >Full page</button>
      <button class="dm-btn" type="button" onclick={onClose} aria-label="Close">Close</button>
    </header>

    {#if brief}
      <p class="dm-brief">{brief}</p>
    {/if}

    <div class="dm-body">
      {#if note}
        <p class="dm-note" role="status">{note}</p>
      {/if}
      {#if error}
        <p class="dm-error">{error}</p>
      {:else if step}
        <StepSurface step={surfaceStep(step)} {jobId} onUpdate={onSurfaceUpdate} />
      {:else}
        <p class="dm-quiet">Loading the step…</p>
      {/if}
    </div>
  </div>
</div>

<style>
  .dm-back {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding: 40px 16px;
    z-index: 50;
  }
  /* Wider than the packet modal on purpose: this one hosts working
     surfaces (two-pane reviews, context panels), not a summary. */
  .dm {
    background: var(--card);
    border: 1px solid var(--hairline);
    border-radius: 4px;
    width: min(1100px, 100%);
    max-height: calc(100vh - 80px);
    display: flex;
    flex-direction: column;
  }
  .dm-head {
    flex: none;
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 18px 24px 12px;
    border-bottom: 1px solid var(--hairline);
  }
  .dm-head-text {
    min-width: 0;
    flex: 1 1 auto;
  }
  .dm-title {
    margin: 0;
    font-size: 1.05rem;
    line-height: 1.35;
    color: var(--fog);
  }
  .dm-sub {
    margin: 4px 0 0;
    font-size: 0.8rem;
    color: var(--fog);
  }
  .dm-btn {
    flex: none;
    background: transparent;
    border: 1px solid var(--hairline);
    color: var(--fog);
    padding: 4px 10px;
    cursor: pointer;
    font: inherit;
    font-size: 0.8rem;
  }
  .dm-btn:hover {
    color: var(--fog);
  }
  .dm-brief {
    flex: none;
    margin: 0;
    padding: 10px 24px;
    border-bottom: 1px solid var(--hairline);
    font-size: 13px;
    line-height: 1.55;
    white-space: pre-wrap;
    color: var(--fog);
    max-height: 20vh;
    overflow-y: auto;
  }
  /* The surface owns everything below the header and scrolls inside
     the panel, so a long context never pushes the verdict buttons off
     screen with no way back to them. */
  .dm-body {
    flex: 1 1 auto;
    overflow-y: auto;
    padding: 18px 24px 22px;
  }
  .dm-quiet {
    margin: 0;
    color: var(--fog);
    font-size: 0.85rem;
  }
  .dm-error {
    margin: 0;
    color: var(--err);
    font-size: 0.85rem;
  }
  /* The soft aside for "saved, but the re-read failed" — quiet on
     purpose: the surface below it is showing a successful receipt. */
  .dm-note {
    margin: 0 0 10px;
    color: var(--fog);
    font-size: 0.8rem;
  }
</style>
