<script lang="ts">
  // Approval / sign-off surface — port of
  // apps/web-legacy/src/steps/ApprovalSurface.tsx.

  import { session } from '@boss/web-kit/session/session.svelte';
  import { appNow, appToday } from '@boss/web-kit/sim-clock';
  import { needsPresence, performPresenceCeremony } from './presence';
  import { describeWriteFailure, putStep, saveStep } from './stepWrite';

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
  $effect(() => {
    // The surface instance is reused when the rail switches steps —
    // an error from step A must not render under step B.
    void step.id;
    signError = '';
  });

  async function decide(d: string): Promise<void> {
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
      // Sign-off contract: a stamp attests the step's current shape, so the
      // decision lands first, then the stamp, then the completion.
      // Each leg is checked: a refused decision aborts the chain —
      // stamping and completing a step whose decision the server
      // rejected is how phantom approvals happen (packet cc9d7fc6).
      const decided = await saveStep(jobId, step.id, body);
      if (decided.kind === 'failed') {
        signError = decided.error;
        return;
      }
      const required = step.sign_offs_required ?? [];
      if (required.includes(userRole)) {
        let stamp = await fetch(`/api/jobs/${jobId}/steps/${step.id}/sign-offs`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ role: userRole }),
        });
        // A presence-gated step refuses a plain session stamp; run
        // the passkey ceremony against this step's current shape and
        // retry with the issued ticket. No fallback: if the ceremony
        // fails, the refusal surfaces and the step waits (Q3).
        if (await needsPresence(stamp)) {
          try {
            const ticket = await performPresenceCeremony(jobId, step.id);
            stamp = await fetch(`/api/jobs/${jobId}/steps/${step.id}/sign-offs`, {
              method: 'POST',
              headers: {
                'Content-Type': 'application/json',
                'x-presence-ticket': ticket,
              },
              body: JSON.stringify({ role: userRole }),
            });
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
        const done = await putStep(jobId, step.id, { status: 'completed' });
        // 409 (stamps missing or stale) renders as the same
        // "sign-offs outstanding: …" line as before — describeWriteFailure
        // names the roles from the conflict body.
        if (done.kind === 'failed') signError = done.error;
      }
      onUpdate();
    } finally {
      saving = false;
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
          class="step-btn step-btn-approve"
          onclick={() => decide('approved')}
          disabled={saving}
        >
          Approve
        </button>
        <button
          class="step-btn step-btn-reject"
          onclick={() => decide('rejected')}
          disabled={saving}
        >
          Reject
        </button>
        <button
          class="step-btn"
          onclick={() => decide('changes-requested')}
          disabled={saving}
        >
          Request changes
        </button>
      </div>
    </div>
  {/if}
</div>
