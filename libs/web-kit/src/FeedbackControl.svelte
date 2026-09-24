<script lang="ts">
  // Feedback, from the chrome bar, on every surface.
  //
  // Submitting opens a `user-feedback` Job. That is the whole design:
  // feedback is work, so it goes in the Job model rather than a table
  // with its own screen. It inherits an owner, a policy-gated triage
  // step, an audit trail of who handled it and when, and a place in
  // the queues operators already read — none of which a bespoke
  // feedback store would have without rebuilding them.
  //
  // The Job's Subject is the surface the feedback is ABOUT: a `custom`
  // Subject whose id is the current route path. So "what have people
  // said about this page" is a Subject-history question, answerable by
  // the same machinery as everything else.
  //
  // No new endpoint — this posts to /api/jobs like any other Job.
  import { session } from './session/session.svelte';

  let open = $state(false);

  /// Bug or feature. They are not the same report and were being
  /// collected as though they were.
  ///
  /// A bug is a claim that the software is wrong, and a claim needs
  /// two facts to be actionable: what happened, and what was expected.
  /// Filed as one free-text blob, that pair is usually half-missing —
  /// the reporter knows both and only writes the surprising one, so
  /// triage starts by asking a question the reporter already answered
  /// in their head.
  ///
  /// A feature request has no such shape. It is an idea, and forcing
  /// it into reality/expectation boxes would make people write "it
  /// doesn't exist" in one of them.
  type Kind = 'bug' | 'feature';
  let kind = $state<Kind>('bug');

  // Bug: two fields. Feature: one.
  let reality = $state('');
  let expectation = $state('');
  let message = $state('');
  let sending = $state(false);
  let sent = $state(false);
  let error = $state<string | null>(null);
  let box: HTMLTextAreaElement | null = $state(null);

  /// The surface being commented on. Read at submit time rather than
  /// on mount — the panel can outlive a client-side navigation.
  function currentRoute(): string {
    return typeof window === 'undefined' ? '/' : window.location.pathname;
  }

  /// Only a platform-admin sees the auto-triage control, and only
  /// because they are the one who would otherwise do it by hand
  /// seconds later. The audit log shows exactly that: feedback filed
  /// at 01:27:15 and triaged from the board at 01:27:29.
  const isAdmin = $derived(
    session.value.kind === 'ready' && session.value.user.role === 'platform-admin',
  );
  /// Route it on submit rather than leaving it for the board.
  let autoTriage = $state(true);

  /// What the report is worth on the board. A bug goes to `reproduce`
  /// because the next action is to make it happen again; a feature
  /// goes to `design` because the next action is to decide what it
  /// should be. Both are dispositions the user-feedback Workflow
  /// already declares — this picks one, it does not invent a route.
  const disposition = $derived(kind === 'bug' ? 'reproduce' : 'design');

  /// The Job's `message`, whichever shape it was collected in. One
  /// field so every existing reader — the triage board, the detail
  /// modal, the queue script — keeps working unchanged.
  function composed(): string {
    return kind === 'bug'
      ? `What happened:\n${reality.trim()}\n\nWhat I expected:\n${expectation.trim()}`
      : message.trim();
  }

  function ready(): boolean {
    return kind === 'bug'
      ? reality.trim().length > 0 && expectation.trim().length > 0
      : message.trim().length > 0;
  }

  function toggle(): void {
    open = !open;
    if (open) {
      sent = false;
      error = null;
      // Focus after the panel exists.
      queueMicrotask(() => box?.focus());
    }
  }

  async function submit(): Promise<void> {
    if (!ready() || sending) return;
    const body = composed();
    sending = true;
    error = null;
    const route = currentRoute();
    const who = session.value.kind === 'ready' ? session.value.user.id : 'anonymous';
    try {
      const r = await fetch('/api/jobs', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          kind: 'user-feedback',
          // Identity-first Subject. The route path IS the subject id,
          // the same shape design-doc-review uses for a doc path.
          subject: { subject_kind: 'custom', id: route },
          title: `Feedback on ${route}`,
          owner_id: who,
          priority: 'standard',
          status: 'open',
          metadata: {
            message: body,
            route,
            submitted_by: who,
            // The structured halves are kept alongside the composed
            // message, not instead of it. `message` stays the one
            // field every reader already knows; these let a future
            // surface show the pair as a pair.
            feedback_kind: kind,
            ...(kind === 'bug' ? { reality: reality.trim(), expectation: expectation.trim() } : {}),
          },
          tags: ['feedback', kind],
        }),
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
      const created = (await r.json()) as { id?: string };
      if (isAdmin && autoTriage && created.id) {
        // Best-effort. A failed triage must not report the FEEDBACK as
        // failed — it is filed either way, and the board is where it
        // would have been routed anyway.
        await triage(created.id).catch(() => {});
      }
      sent = true;
      message = '';
      reality = '';
      expectation = '';
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      sending = false;
    }
  }

  /// Complete the Job's triage step with the disposition implied by the
  /// kind. Matches the step by TITLE because materialization keeps the
  /// rendered title and discards the spec slug (backlog item 6c6b9e06);
  /// when that is fixed this should match the slug instead.
  ///
  /// Metadata is merged, never replaced: `PUT .../steps/{id}` swaps
  /// `metadata` wholesale, and `authority_role` lives in there — it is
  /// what keeps a gated step gated.
  async function triage(jobId: string): Promise<void> {
    const jr = await fetch(`/api/jobs/${encodeURIComponent(jobId)}`);
    if (!jr.ok) return;
    const job = (await jr.json()) as {
      steps?: Array<{ id: string; title: string; status: string; metadata?: Record<string, unknown> }>;
    };
    const step = job.steps?.find((s) => s.title === 'Triage feedback');
    if (!step || step.status === 'completed' || step.status === 'skipped') return;
    await fetch(`/api/jobs/${encodeURIComponent(jobId)}/steps/${encodeURIComponent(step.id)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        status: 'completed',
        metadata: { ...(step.metadata ?? {}), disposition },
      }),
    });
  }

  function onKey(e: KeyboardEvent): void {
    if (e.key === 'Escape') open = false;
    // Cmd/Ctrl+Enter submits — the convention for a textarea whose
    // Enter key means newline.
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') void submit();
  }
</script>

<div class="fb">
  <button
    class="fb-trigger"
    type="button"
    aria-expanded={open}
    aria-haspopup="dialog"
    onclick={toggle}
    title="Send feedback about this page"
  >
    Feedback
  </button>

  {#if open}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div class="fb-panel" role="dialog" aria-label="Send feedback" onkeydown={onKey}>
      {#if sent}
        <p class="fb-done">
          Thanks — that opened a feedback Job for <code>{currentRoute()}</code>. It is
          queued for triage like any other work.
        </p>
        <button class="fb-send" type="button" onclick={() => (open = false)}>Close</button>
      {:else}
        <div class="fb-kind" role="radiogroup" aria-label="Kind of feedback">
          <button
            type="button"
            class="fb-kind-btn"
            class:fb-kind-on={kind === 'bug'}
            role="radio"
            aria-checked={kind === 'bug'}
            onclick={() => (kind = 'bug')}>Bug</button
          >
          <button
            type="button"
            class="fb-kind-btn"
            class:fb-kind-on={kind === 'feature'}
            role="radio"
            aria-checked={kind === 'feature'}
            onclick={() => (kind = 'feature')}>Feature</button
          >
          <span class="fb-kind-note">on <code>{currentRoute()}</code></span>
        </div>

        {#if kind === 'bug'}
          <!-- Two fields, because a bug is a claim that needs both
               halves to be actionable. Asked separately so the one the
               reporter finds obvious still gets written down. -->
          <label class="fb-label" for="fb-reality">What happened</label>
          <textarea
            id="fb-reality"
            bind:this={box}
            bind:value={reality}
            rows="3"
            placeholder="What the software actually did."
          ></textarea>
          <label class="fb-label" for="fb-expectation">What you expected</label>
          <textarea
            id="fb-expectation"
            bind:value={expectation}
            rows="2"
            placeholder="What it should have done instead."
          ></textarea>
        {:else}
          <label class="fb-label" for="fb-message">What would you like</label>
          <textarea
            id="fb-message"
            bind:this={box}
            bind:value={message}
            rows="5"
            placeholder="An idea, a gap, something that should exist."
          ></textarea>
        {/if}

        {#if isAdmin}
          <label class="fb-triage">
            <input type="checkbox" bind:checked={autoTriage} />
            Triage now as <strong>{disposition}</strong>
          </label>
        {/if}

        {#if error}<p class="fb-err">{error}</p>{/if}
        <div class="fb-actions">
          <span class="fb-hint">⌘/Ctrl + Enter</span>
          <button
            class="fb-send"
            type="button"
            disabled={sending || !ready()}
            onclick={submit}
          >
            {sending ? 'Sending…' : 'Send'}
          </button>
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .fb-kind {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 10px;
  }
  .fb-kind-btn {
    font: inherit;
    font-size: 12px;
    padding: 3px 12px;
    border-radius: 999px;
    border: 1px solid var(--hairline);
    background: var(--ink);
    color: var(--static);
    cursor: pointer;
  }
  .fb-kind-on {
    background: var(--band);
    border-color: var(--border-strong);
    color: var(--on-band);
  }
  .fb-kind-note {
    font-size: 11px;
    color: var(--static);
    margin-left: auto;
  }
  .fb-triage {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--static);
    margin-top: 8px;
  }

  .fb {
    position: relative;
    display: flex;
    align-items: center;
  }
  /* The chrome bar is an unconditionally dark surface (#0c0a09) that
     sets no `color`, so anything inside it must declare its own. This
     button used `color: inherit` and therefore picked up the DOCUMENT
     text colour: fine in dark theme, near-black on near-black in
     light theme. Reported as "I can't read this feedback button" —
     and correctly diagnosed as a light/dark issue.

     Matching `.signin-btn` rather than inventing another treatment:
     the two are the only bordered buttons in the bar, and they sat
     one gap apart with different borders, radii, and padding. */
  /* Ghost button, §04 — matches .signin-btn beside it. */
  .fb-trigger {
    background: transparent;
    border: 1px solid var(--hairline);
    border-radius: var(--radius);
    padding: 5px 12px;
    font-family: var(--font-mono);
    font-size: 11px;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: var(--ls-nav);
    line-height: 1.4;
    color: var(--fog);
    cursor: pointer;
    white-space: nowrap;
    transition: background 0.1s, color 0.1s, border-color 0.1s;
  }
  .fb-trigger:hover {
    background: var(--fog);
    color: var(--void);
    border-color: var(--fog);
  }
  .fb-trigger:focus-visible,
  .fb-send:focus-visible {
    outline: 2px solid currentColor;
    outline-offset: 2px;
  }
  /* Anchored below the 44px bar, right-aligned so it never runs off
     the viewport on the control that sits nearest the edge. */
  .fb-panel {
    position: absolute;
    top: calc(100% + 8px);
    right: 0;
    z-index: 60;
    width: 320px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    border-radius: 8px;
    background: var(--card);
    color: var(--text);
    border: 1px solid var(--border);
  }
  .fb-label {
    font-size: 12px;
    color: var(--text-dim);
  }
  .fb-panel code {
    font-size: 11px;
    background: var(--bg);
    padding: 1px 4px;
    border-radius: 3px;
  }
  .fb-panel textarea {
    font: inherit;
    font-size: 13px;
    padding: 6px;
    border: 1px solid var(--border);
    border-radius: 4px;
    resize: vertical;
  }
  .fb-actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .fb-hint {
    font-size: 11px;
    color: var(--text-dim);
    margin-right: auto;
  }
  .fb-send {
    font: inherit;
    font-size: 12px;
    padding: 4px 12px;
    border-radius: 4px;
    border: 1px solid var(--border);
    background: var(--bg);
    color: inherit;
    cursor: pointer;
  }
  .fb-send:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .fb-done {
    font-size: 13px;
    margin: 0;
  }
  .fb-err {
    font-size: 12px;
    color: var(--err);
    margin: 0;
  }
</style>
