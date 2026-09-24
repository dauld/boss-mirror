<script lang="ts">
  // The abort modal (design c6f9fb3e, accepted 2026-09-15; backlog
  // 7a98040e). David, feedback 33324fe9: "I need to be able to Abort a
  // job in the UI, but make me provide a reason."
  //
  // Deliberately thin: it names the terminal it will complete, asks
  // for the reason, and sends the ONE write a person would have made
  // by hand through the step inspector — the ordinary step PUT — so
  // policy, authority, the blocker gate and the event log apply
  // unchanged. What it adds is the finding and the asking: the
  // operator no longer has to know which step is the abort, and cannot
  // close a packet without saying why. Two aborted terminals: it lists
  // both and asks which. The page decides whether to mount it at all
  // (none → no control), so this component never sees an empty list.
  import { putStep } from '../steps/stepWrite';
  import { abortBody, reasonIsSentence, type AbortTerminal } from './abort';

  type Props = Readonly<{
    jobId: string;
    jobTitle: string;
    terminals: ReadonlyArray<AbortTerminal>;
    onClose: () => void;
    /** The write landed — the page refetches so the closed packet
     *  renders from the server's answer, not a local guess. */
    onAborted: () => void;
  }>;
  let { jobId, jobTitle, terminals, onClose, onAborted }: Props = $props();

  let chosenId = $state<string>('');
  let reason = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);

  // One terminal picks itself; two or more wait for the choice.
  $effect(() => {
    if (terminals.length === 1) chosenId = terminals[0]?.id ?? '';
  });
  const chosen = $derived(terminals.find((t) => t.id === chosenId) ?? null);
  const canSubmit = $derived(chosen !== null && reasonIsSentence(reason) && !busy);

  async function submit(): Promise<void> {
    const t = chosen;
    if (!t) return;
    // The terminal carries the step's materialised metadata; the body
    // merges the reason over it, because a step PUT replaces metadata
    // wholesale and `outcome_kind` lives in that same object.
    const body = abortBody({ metadata: t.metadata }, reason);
    if (!body) {
      error = 'A reason is required — at least one sentence.';
      return;
    }
    busy = true;
    error = null;
    // putStep can only come back as a discriminated result: a refused
    // write (a 409 from the blocker gate, a 403 from policy) renders
    // the server's own words here and leaves the modal open.
    const result = await putStep(jobId, t.id, body);
    busy = false;
    if (result.kind === 'failed') {
      error = result.error;
      return;
    }
    onAborted();
    onClose();
  }

  // Escape-to-close via $effect, NOT the svelte window tag — the
  // bun+svelte bundler crashes on that event lookup (PacketModal
  // documents the trap; no-svelte-window.test.ts pins it).
  $effect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === 'Escape' && !busy) onClose();
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  });
</script>

<div
  class="am-back"
  role="presentation"
  onclick={(e) => {
    if (e.target === e.currentTarget && !busy) onClose();
  }}
>
  <div class="am" role="dialog" aria-modal="true" aria-label="Abort this job">
    <header class="am-head">
      <div class="am-head-text">
        <h2 class="am-title">Abort this job</h2>
        <p class="am-sub">{jobTitle}</p>
      </div>
      <button class="am-btn" type="button" onclick={onClose} disabled={busy} aria-label="Close">
        Close
      </button>
    </header>

    <form
      class="am-body"
      onsubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      {#if terminals.length === 1 && chosen}
        <!-- The row's own name for the terminal: the operator sees what
             the protocol will record, not a synonym the page chose. -->
        <p class="am-terminal">
          Completes <strong>{chosen.title}</strong>
          <span class="am-slug">({chosen.slug})</span> and closes the packet.
        </p>
      {:else}
        <fieldset class="am-choice">
          <legend>This workflow has {terminals.length} aborted terminals — which one?</legend>
          {#each terminals as t (t.id)}
            <label class="am-option">
              <input type="radio" name="am-terminal" value={t.id} bind:group={chosenId} />
              <span><strong>{t.title}</strong> <span class="am-slug">({t.slug})</span></span>
            </label>
          {/each}
        </fieldset>
      {/if}

      <label class="am-label" for="am-reason">Reason</label>
      <textarea
        id="am-reason"
        class="am-reason"
        rows="3"
        bind:value={reason}
        disabled={busy}
        placeholder="At least one sentence — what the next reader needs to know."
      ></textarea>
      <p class="am-hint">
        Required. Recorded on the step as its reason, signed by you.
      </p>

      {#if error}
        <p class="step-write-error" role="alert">{error}</p>
      {/if}

      <div class="am-actions">
        <button type="submit" class="btn btn-danger-outline" disabled={!canSubmit}>
          {busy ? 'Aborting…' : 'Abort job'}
        </button>
        <button type="button" class="btn" onclick={onClose} disabled={busy}>Cancel</button>
      </div>
    </form>
  </div>
</div>

<style>
  .am-back {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding: 40px 16px;
    z-index: 50;
  }
  /* Narrower than DecideModal on purpose: one question, one field. */
  .am {
    background: var(--card);
    border: 1px solid var(--hairline);
    border-radius: 4px;
    width: min(560px, 100%);
    display: flex;
    flex-direction: column;
  }
  .am-head {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 18px 24px 12px;
    border-bottom: 1px solid var(--hairline);
  }
  .am-head-text { min-width: 0; flex: 1 1 auto; }
  .am-title {
    margin: 0;
    font-size: 1.05rem;
    line-height: 1.35;
    color: var(--fog);
  }
  .am-sub {
    margin: 4px 0 0;
    font-size: 0.8rem;
    color: var(--fog);
  }
  .am-btn {
    flex: none;
    background: transparent;
    border: 1px solid var(--hairline);
    color: var(--fog);
    padding: 4px 10px;
    cursor: pointer;
    font: inherit;
    font-size: 0.8rem;
  }
  .am-btn:hover:not(:disabled) { color: var(--fog); }
  .am-body {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 18px 24px 22px;
  }
  .am-terminal { margin: 0 0 6px; font-size: 13px; color: var(--text); }
  .am-slug { font-family: monospace; font-size: 12px; color: var(--text-dim); }
  .am-choice {
    border: 1px solid var(--hairline);
    border-radius: var(--radius);
    padding: 8px 12px 10px;
    margin: 0 0 6px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 13px;
  }
  .am-choice legend { color: var(--text-dim); font-size: 12px; padding: 0 4px; }
  .am-option { display: flex; gap: 8px; align-items: baseline; cursor: pointer; }
  .am-label { font-size: 12px; color: var(--text-dim); }
  .am-reason {
    width: 100%;
    box-sizing: border-box;
    font: inherit;
    font-size: 13px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--wash);
    color: var(--text);
    resize: vertical;
  }
  .am-hint { margin: 0; font-size: 12px; color: var(--text-dim); }
  .am-actions { display: flex; gap: 8px; margin-top: 8px; }
</style>
