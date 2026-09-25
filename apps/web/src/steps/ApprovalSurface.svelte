<script lang="ts">
  // Approval / sign-off surface — port of
  // apps/web-legacy/src/steps/ApprovalSurface.tsx.

  import { tick } from 'svelte';
  import { session } from '@boss/web-kit/session/session.svelte';
  import { appNow, appToday } from '@boss/web-kit/sim-clock';
  import {
    completeWithPresence,
    needsPresence,
    performPresenceCeremony,
    scrollNote,
    shownAfter,
    signedRows,
    signedText,
    type ShownStep,
  } from './presence';
  import { describeWriteFailure, saveStep } from './stepWrite';

  type StepData = {
    id: string;
    kind: string;
    title: string;
    status: string;
    metadata: Record<string, unknown>;
    notes: string | null;
    sign_offs_required?: string[];
    sign_offs?: { role: string; authority_id: string; shape_hash: string }[];
  };

  type Props = {
    step: StepData;
    jobId: string;
    onUpdate: () => void;
  };
  let { step, jobId, onUpdate }: Props = $props();

  let comment = $state(String(step.metadata.comment ?? ''));
  let saving = $state(false);

  let decision = $derived(String(step.metadata.decision ?? 'pending'));
  let userId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : '',
  );
  let userRole = $derived(
    session.value.kind === 'ready' ? session.value.user.role : '',
  );
  let signError = $state('');
  // The id as a VALUE: a derived notifies only when it changes, where
  // reading `step.id` in the effect re-ran it on every refresh of the
  // same step — so the onUpdate() after a refused completion wiped the
  // refusal it had just rendered (measured by the mocked spec for
  // backlog 3ce3c15f, 2026-09-25).
  let stepId = $derived(step.id);

  // WHAT THE PASSKEY SIGNS IS ON SCREEN (design f623e425 D3; backlog
  // 6c9183de extends b and c, 2026-09-25). A presence stamp binds
  // step_shape_hash(title, metadata) — every metadata key — and this
  // surface used to render `decision` and `comment` alone: an
  // ops-request's plan, verb, host, args and rendered_plan_sha256, or a
  // key someone planted, were signed on one tap and never seen. Now the
  // step's title (the header) and EVERY metadata key are rendered, with
  // the gesture's own write folded in while it is in flight (`pending`),
  // from the one object `onScreen` — and the ceremony is handed that same
  // object as its answer to "what is on screen", refusing any key it
  // would sign that is not there (presence.ts notShown). The rows are
  // derived from the keys, never from an allow-list, so they cannot fall
  // behind the hash.
  let pending = $state<Readonly<Record<string, unknown>> | null>(null);
  let onScreen = $derived<ShownStep>({
    title: step.title,
    metadata: shownAfter(step.metadata, pending ?? {}),
  });
  let signedVisible = $derived(
    pending !== null || (step.status !== 'completed' && step.status !== 'skipped'),
  );
  let signed = $derived(signedRows(onScreen));
  // Read at the instant a ceremony begins — never a copy taken earlier.
  const shownNow = (): ShownStep | null => (signedVisible ? onScreen : null);

  // The rows whose value box scrolls, by key: rendered is not read, so a
  // box that scrolls says so under it (backlog 6093cf13). Measured off
  // the laid-out box — on each draw of its text, and whenever the box
  // changes size.
  let scrolling = $state<Readonly<Record<string, boolean>>>({});
  function watchOverflow(node: HTMLElement, row: { key: string; text: string }) {
    let key = row.key;
    const check = (): void => {
      const scrolls =
        node.scrollHeight > node.clientHeight + 1 || node.scrollWidth > node.clientWidth + 1;
      if ((scrolling[key] ?? false) !== scrolls) scrolling = { ...scrolling, [key]: scrolls };
    };
    const observer = typeof ResizeObserver === 'function' ? new ResizeObserver(check) : null;
    observer?.observe(node);
    check();
    return {
      update(next: { key: string; text: string }) {
        key = next.key;
        requestAnimationFrame(check);
      },
      destroy() {
        observer?.disconnect();
      },
    };
  }

  $effect(() => {
    // The surface instance is reused when the rail switches steps —
    // an error from step A must not render under step B, nor A's
    // in-flight decision be drawn into B's signed content. A gesture
    // still running for A then finds A's content off screen, and its
    // ceremony refuses rather than sign what is no longer shown.
    void stepId;
    signError = '';
    pending = null;
    scrolling = {};
  });

  async function decide(d: string): Promise<void> {
    // The step this click was aimed at, captured BEFORE the first await
    // and the only step any write below names (backlog d82b5f60, review
    // of car 66de0e4b). The instance is reused when the rail switches
    // steps, so `step` read after an await is whichever step is on
    // screen NOW: a switch mid-gesture sent the stamp, the ceremony's
    // `shown` and the completion PUT to a step the approver never
    // clicked — and an ordinary step was completed by it.
    const target = {
      id: step.id,
      title: step.title,
      metadata: { ...step.metadata },
      signOffsRequired: [...(step.sign_offs_required ?? [])],
    };
    const job = jobId;
    const role = userRole;
    saving = true;
    signError = '';
    try {
      // v2: both approve and reject COMPLETE the step. The reject
      // decision lives in metadata.decision; downstream routing is
      // predicate-driven server-side (no client-set 'blocked'). The
      // decision is metadata alone, through the merge door: an emptied
      // comment is sent as null and deleted, where it used to be
      // cleared by omission from a wholesale PUT (backlog e39a9d2a).
      const body = {
        metadata: {
          decision: d,
          decided_at: appNow().toISOString(),
          comment: comment || undefined,
        },
      };
      // The decision, its time and the comment join the signed content
      // on screen BEFORE anything is written or signed (D3): the passkey
      // below signs them, so they are drawn first.
      pending = body.metadata;
      await tick();
      // Sign-off contract: a stamp attests the step's current shape, so the
      // decision lands first, then the stamp, then the completion.
      // Each leg is checked: a refused decision aborts the chain —
      // stamping and completing a step whose decision the server
      // rejected is how phantom approvals happen (packet cc9d7fc6).
      const decided = await saveStep(job, target.id, body);
      if (decided.kind === 'failed') {
        signError = decided.error;
        return;
      }
      const required = target.signOffsRequired;
      // The ticket this gesture's own ceremony was issued, if one ran.
      // The completion below carries it: the jobs API judges a
      // presence-gated step again on the request that completes it, and
      // a bare PUT after the presence stamp answered 422 (backlog
      // b568044a). Same step, same person, same shape — nothing wider.
      let presenceTicket: string | undefined;
      if (required.includes(role)) {
        let stamp = await fetch(`/api/jobs/${job}/steps/${target.id}/sign-offs`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ role }),
        });
        // A presence-gated step refuses a plain session stamp; run
        // the passkey ceremony against this step's current shape and
        // retry with the issued ticket. No fallback: if the ceremony
        // fails, the refusal surfaces and the step waits (Q3).
        if (await needsPresence(stamp)) {
          try {
            // The step as this surface showed it, with the decision it
            // just saved folded in — what the passkey may sign (fd7090cc).
            const ticket = await performPresenceCeremony(
              job,
              target.id,
              { title: target.title, metadata: shownAfter(target.metadata, body.metadata) },
              shownNow,
            );
            stamp = await fetch(`/api/jobs/${job}/steps/${target.id}/sign-offs`, {
              method: 'POST',
              headers: {
                'Content-Type': 'application/json',
                'x-presence-ticket': ticket,
              },
              body: JSON.stringify({ role }),
            });
            presenceTicket = ticket;
          } catch (e) {
            signError = e instanceof Error ? e.message : String(e);
            // The decision DID land — refresh so the surface renders
            // the recorded state, with the refusal beside it.
            onUpdate();
            return;
          }
        }
        if (!stamp.ok) {
          signError = describeWriteFailure(
            stamp.status,
            await stamp.text().catch(() => ''),
          );
          onUpdate();
          return;
        }
      }
      if (d === 'approved' || d === 'rejected') {
        // A completion refused for presence — no ticket after a reload,
        // this gesture's ticket past its life, or a presence step whose
        // sign-offs this user's role does not carry — is answered with
        // ONE tap on the step as shown and ONE retry (backlog 3ce3c15f).
        const done = await completeWithPresence(
          job,
          target.id,
          { title: target.title, metadata: shownAfter(target.metadata, body.metadata) },
          shownNow,
          presenceTicket,
        );
        // 409 (stamps missing or stale) renders as the same
        // "sign-offs outstanding: …" line as before — describeWriteFailure
        // names the roles from the conflict body.
        if (done.kind === 'failed') signError = done.error;
      }
      onUpdate();
    } finally {
      saving = false;
      pending = null;
    }
  }
</script>

<div class="step-surface step-approval">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-status step-status-{step.status}">{step.status}</span>
  </div>

  {#if signError}
    <p class="step-write-error" role="alert">{signError}</p>
  {/if}
  {#if signedVisible}
    <section class="step-signed-keys" aria-label="What your passkey signs">
      <div class="step-signed-keys-head">What your passkey signs</div>
      <p class="step-signed-keys-note">
        The step <strong class="step-signed-title">{signedText(onScreen.title)}</strong>
        and every key below, exactly as shown. Text in double quotes has each
        character you could not otherwise see or tell apart written as an
        escape. Your decision, its time and your comment join them when you
        press a button.
      </p>
      <dl>
        {#each signed as row (row.key)}
          <dt class="step-signed-key">{row.label}</dt>
          <dd>
            <pre class="step-signed-value" use:watchOverflow={{ key: row.key, text: row.text }}>{row.text}</pre>
            {#if scrolling[row.key]}
              <div class="step-signed-overflow">{scrollNote(row.text)}</div>
            {/if}
          </dd>
        {/each}
      </dl>
    </section>
  {/if}
  {#if decision !== 'pending' && decision !== ''}
    <div class="step-approval-result step-approval-{decision}">
      Decision: <strong>{decision}</strong>
      {#if comment}<div class="step-approval-comment">{comment}</div>{/if}
    </div>
  {:else}
    <div class="step-approval-form">
      <div class="step-field">
        <label for={`approval-comment-${step.id}`}>Comment (optional)</label>
        <textarea
          id={`approval-comment-${step.id}`}
          rows="2"
          bind:value={comment}
          placeholder="Add a comment..."
        ></textarea>
      </div>
      <div class="step-actions">
        <button
          class="btn btn-primary"
          onclick={() => decide('approved')}
          disabled={saving}
        >
          Approve
        </button>
        <button
          class="btn btn-danger-outline"
          onclick={() => decide('rejected')}
          disabled={saving}
        >
          Reject
        </button>
        <button
          class="btn"
          onclick={() => decide('changes-requested')}
          disabled={saving}
        >
          Request changes
        </button>
      </div>
    </div>
  {/if}
</div>
