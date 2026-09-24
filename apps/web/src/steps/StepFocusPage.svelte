<script lang="ts">
  // Full-page step surface.
  //
  // A plugin-backed step normally renders as a panel inside the job
  // page, below the job header and beside the step list. That is right
  // for a checklist and wrong for anything you have to *read*: the
  // design-review surface is a document plus a set of decisions, and it
  // was competing for width with a sidebar, a step list and job chrome.
  //
  // This route gives the step the viewport. It renders OUTSIDE AppShell
  // (App.svelte branches before the shell, as it does for login), so
  // there is no sidebar — only the chrome bar above and a slim bar
  // naming the job you came from.
  //
  // Host-side by necessity: a plugin cannot decide how it is mounted.
  // Everything below the header is still the plugin's, unchanged — the
  // same bundle renders here and inline.
  import { onMount } from 'svelte';
  import StepPluginMount from './StepPluginMount.svelte';
  import StepSurface from './StepSurface.svelte';
  import { getStepPluginMount } from './pluginHost';
  import type { StepStatus } from '../jobs/types';
  import type { StepPluginProps } from './pluginHost';
  import { navigate } from '../router';
  import { session } from '@boss/web-kit/session/session.svelte';

  let { jobId, stepId, from, fromLabel } = $props<{
    jobId: string;
    stepId: string;
    /// Where the operator came from. David, feedback 40fe7291, filed
    /// while working the design queue: "The 'Back' functionality from
    /// a design review went to the job. What I expected: gone back to
    /// the Design Review queue."
    ///
    /// Back used to be hardcoded to the job page, which is the one
    /// place you were deliberately NOT sent — a review opens the
    /// full-page step surface precisely because the job page buries
    /// the document beside a sidebar and a step list. So Back undid
    /// the routing choice and dropped you one queue further away.
    ///
    /// The lens that opened the step says where back goes, because it
    /// is the only thing that knows. Absent (a deep link, or a
    /// surface that has not adopted it) the job page remains the
    /// fallback, so nothing regresses.
    from?: string;
    fromLabel?: string;
  }>();

  // Reuse the plugin contract's own step shape rather than
  // redeclaring it — a local copy drifts, and the drift only shows up
  // as a type error at the mount site (it did: `notes`, and metadata's
  // optionality).
  type Step = StepPluginProps['step'];
  type Job = { id: string; title: string; kind: string; status: string };

  let job = $state<Job | null>(null);
  let step = $state<Step | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Declared after `job` because the label falls back to it. Kept as
  // one pair so the destination and its name can never disagree — a
  // Back reading "Design Review" that lands on the job page is worse
  // than the bug this fixes.
  const backTo = $derived(from ?? `/ux/jobs/${jobId}`);
  const backText = $derived(fromLabel ?? (from ? 'Back' : (job?.title ?? 'Back to job')));

  // The plugin contract takes `PluginCurrentUser | undefined`, not
  // null — undefined means "no user known", which is what a
  // not-yet-ready session is.
  let currentUser = $derived(
    session.value.kind === 'ready'
      ? { id: session.value.user.id, role: session.value.user.role }
      : undefined,
  );

  async function load(): Promise<void> {
    loading = true;
    error = null;
    try {
      const jr = await fetch(`/api/jobs/${jobId}`);
      if (!jr.ok) throw new Error(`job: HTTP ${jr.status}`);
      const body = await jr.json();
      job = body as Job;
      const steps: Step[] = Array.isArray(body.steps) ? body.steps : [];
      const found = steps.find((s) => s.id === stepId) ?? null;
      if (!found) throw new Error(`step ${stepId} is not part of this job`);
      step = found;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  /// The plugin contract types `status` as a plain string — plugins
  /// are framework-agnostic and get no Svelte types — while the
  /// platform surfaces take the narrowed `StepStatus` union. Narrow
  /// once here rather than loosening the surfaces, which would give up
  /// the exhaustiveness they rely on.
  function surfaceStep(s: Step) {
    return { ...s, status: s.status as StepStatus };
  }

  /// Which surface to render. `null` while we are still asking —
  /// rendering the fallback before the answer arrives would flash the
  /// generic surface under every plugin-backed step.
  let hasPlugin = $state<boolean | null>(null);

  $effect(() => {
    const kind = step?.kind;
    if (!kind) return;
    let cancelled = false;
    void (async () => {
      const mount = await getStepPluginMount(kind);
      if (!cancelled) hasPlugin = mount != null;
    })();
    return () => {
      cancelled = true;
    };
  });

  onMount(load);

  /// The Job's own brief — the filing message a feedback/backlog item
  /// carries. Without it the step page is a form with no problem
  /// statement ("start buttons with no context", 2026-08-10).
  let jobBrief = $derived.by(() => {
    const m = (job as unknown as { metadata?: Record<string, unknown> } | null)?.metadata;
    const msg = m?.message ?? m?.body;
    return typeof msg === 'string' && msg.trim() ? msg : null;
  });
</script>

<div class="step-focus">
  <div class="step-focus-bar">
    <button class="step-focus-back" onclick={() => navigate(backTo)}>
      ← {backText}
    </button>
    {#if step}
      <span class="step-focus-title">{step.title}</span>
      <span class="step-focus-status">{step.status}</span>
    {/if}
  </div>

  {#if jobBrief}
    <details class="step-focus-brief" open>
      <summary>Why this Job exists</summary>
      <p>{jobBrief}</p>
    </details>
  {/if}

  <div class="step-focus-body">
    {#if loading}
      <p class="step-focus-msg">Loading step…</p>
    {:else if error}
      <p class="step-focus-msg load-failed" role="alert">{error}</p>
    {:else if step && hasPlugin === true}
      <StepPluginMount
        kind={step.kind}
        {step}
        {jobId}
        {currentUser}
        onUpdate={load}
      />
    {:else if step && hasPlugin === false}
      <!-- No plugin for this kind. The page was written for
           plugin-backed steps, so it used to render "No plugin
           registered for task" and stop — which became a dead end the
           moment inbox notifications started linking here for every
           authority-gated step. `StepSurface` is the platform's own
           dispatcher and lands on GenericSurface, so the route now
           works for any kind rather than only the ones with a bundle. -->
      <div class="step-focus-fallback">
        <StepSurface step={surfaceStep(step)} {jobId} onUpdate={load} />
      </div>
    {/if}
  </div>
</div>

<style>
  .step-focus-brief {
    margin: 10px 0 14px;
    border: 1px solid var(--hairline);
    border-radius: 8px;
    padding: 10px 14px;
    background: var(--card);
  }
  .step-focus-brief summary {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--static);
    font-weight: 600;
    cursor: pointer;
  }
  .step-focus-brief p {
    margin: 8px 0 0;
    font-size: 13.5px;
    line-height: 1.6;
    white-space: pre-wrap;
    max-width: 74ch;
  }
  /* Offset below the fixed 44px chrome bar, then take everything. */
  .step-focus {
    position: absolute;
    inset: 44px 0 0 0;
    display: flex;
    flex-direction: column;
    background: var(--bg);
  }
  .step-focus-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 24px;
    border-bottom: 1px solid var(--border);
    background: var(--card);
  }
  .step-focus-back {
    background: none;
    border: none;
    padding: 4px 6px;
    margin-left: -6px;
    border-radius: 4px;
    cursor: pointer;
    font: inherit;
    font-size: 13px;
    color: var(--text-dim);
  }
  .step-focus-back:hover {
    background: var(--wash);
    color: var(--text);
  }
  .step-focus-title {
    font-size: 14px;
    font-weight: 600;
    flex: 1 1 auto;
  }
  .step-focus-status {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-dim);
  }
  /* The plugin owns everything from here down. */
  .step-focus-body {
    flex: 1 1 auto;
    overflow-y: auto;
    padding: 24px;
  }
  /* Let a two-pane plugin fill the viewport instead of scrolling
     inside its own 78vh box inside this one — two nested scroll
     regions is exactly the cramped feeling this route exists to fix.
     Scoped to the pane classes, so any other plugin just scrolls the
     body normally. */
  .step-focus-body :global(.step-review-design .srd-doc),
  .step-focus-body :global(.step-review-design .srd-rail) {
    max-height: calc(100vh - 190px);
  }
  .step-focus-msg {
    color: var(--text-dim);
    font-size: 14px;
  }
</style>
