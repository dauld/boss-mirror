<script lang="ts">
  // Inspection step — structured QA check surface. Two columns:
  // result + inspector notes on the left, system model + preventive maintenance
  // checklist (pulled from the catalog KB) on the right.
  //
  // If the step requires sign-offs, completion stamps land first via
  // POST .../sign-offs (the sign-off contract) and the server gates the flip
  // from the session user. The backend policy check rejects
  // unauthorized callers.

  import { isPending, type StepStatus } from '../jobs/types';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { session } from '@boss/web-kit/session/session.svelte';
  import { appToday } from '@boss/web-kit/sim-clock';
  import { describeWriteFailure, putStep, saveStep } from './stepWrite';

  type StepData = {
    id: string;
    kind: string;
    title: string;
    status: StepStatus;
    assignee_id: string | null;
    metadata: Record<string, unknown>;
    notes: string | null;
    sign_offs_required?: string[];
    sign_offs?: { role: string; shape_hash: string }[];
  };

  type CatalogModel = {
    sku: string;
    name: string;
    service?: {
      pm_checklist?: string[];
      required_skill_level?: number;
    };
  };

  type Props = {
    step: StepData;
    jobId: string;
    onUpdate: () => void;
  };
  let { step, jobId, onUpdate }: Props = $props();

  const assetId = (step.metadata.asset_id as string | undefined) ?? '';

  let result = $state<string>(String(step.metadata.overall_result ?? ''));
  let notes = $state<string>(String(step.metadata.inspector_notes ?? ''));
  let saving = $state(false);
  let writeError = $state<string | null>(null);
  $effect(() => {
    // The surface instance is reused when the rail switches steps —
    // an error from step A must not render under step B.
    void step.id;
    writeError = null;
  });

  let sku = $state<string | null>(null);
  let model = $state<CatalogModel | null>(null);

  $effect(() => {
    if (!assetId) return;
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch(`/api/assets/${encodeURIComponent(assetId)}`);
        if (!r.ok || cancelled) return;
        const data = await r.json();
        if (!cancelled) sku = data.current_state?.sku ?? null;
      } catch {
        /* ignore */
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  $effect(() => {
    const s = sku;
    if (!s) return;
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/catalog/models');
        if (!r.ok || cancelled) return;
        const rows = (await r.json()) as CatalogModel[];
        const match = rows.find((m) => m.sku === s) ?? null;
        if (!cancelled) model = match;
      } catch {
        /* ignore */
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let pmChecklist = $derived<string[]>(model?.service?.pm_checklist ?? []);
  let currentUserId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : null,
  );

  async function save(newStatus?: string): Promise<void> {
    saving = true;
    writeError = null;
    try {
      const required = step.sign_offs_required ?? [];
      const completing = newStatus === 'completed' && required.length > 0;
      // The keys this surface owns, through the merge door; an emptied
      // field is sent as null and deleted, where it used to be cleared
      // by omission from a wholesale PUT (backlog e39a9d2a).
      const body = {
        ...(newStatus && !completing ? { status: newStatus } : {}),
        metadata: {
          overall_result: result || undefined,
          inspector_notes: notes || undefined,
        },
      };
      // Metadata first, then stamps attesting the final shape, then
      // the status flip. Server gates the completion. Every leg is
      // checked — a refused write aborts the chain and renders inline
      // instead of stamping/completing on top of it (packet cc9d7fc6).
      const wrote = await saveStep(jobId, step.id, body);
      if (wrote.kind === 'failed') {
        writeError = wrote.error;
        return;
      }
      if (completing) {
        const myRole =
          session.value.kind === 'ready' ? session.value.user.role : '';
        if (required.includes(myRole)) {
          const stamp = await fetch(`/api/jobs/${jobId}/steps/${step.id}/sign-offs`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ role: myRole }),
          });
          if (!stamp.ok) {
            writeError = describeWriteFailure(
              stamp.status,
              await stamp.text().catch(() => ''),
            );
            // The metadata write DID land — refresh so the surface
            // renders the recorded state, with the refusal beside it.
            onUpdate();
            return;
          }
        }
        const done = await putStep(jobId, step.id, { status: 'completed' });
        if (done.kind === 'failed') writeError = done.error;
      }
      onUpdate();
    } finally {
      saving = false;
    }
  }
</script>

<div class="step-surface step-inspection">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-kind-label">inspection</span>
    <span class="step-status step-status-{step.status}">{step.status}</span>
    {#if step.assignee_id}
      <span class="step-assignee">
        Assigned:
        <EntityLink kind="employee" id={step.assignee_id} />
      </span>
    {/if}
  </div>

  <div class="step-repair-layout">
    <div class="step-repair-form">
      <Section title="Inspection">
          <div class="step-field">
            <label for="insp-result-{step.id}">Overall result</label>
            <select
              id="insp-result-{step.id}"
              bind:value={result}
              disabled={saving}
            >
              <option value="">— Pending —</option>
              <option value="pass">Pass</option>
              <option value="fail">Fail</option>
              <option value="conditional">Conditional</option>
            </select>
          </div>

          <div class="step-field">
            <label for="insp-notes-{step.id}">Inspector notes</label>
            <textarea
              id="insp-notes-{step.id}"
              rows="4"
              bind:value={notes}
              disabled={saving}
              placeholder="Observations, measurements, anomalies..."
            ></textarea>
          </div>
      </Section>

      {#if writeError}
        <p class="step-write-error" role="alert">{writeError}</p>
      {/if}

      <div class="step-actions">
        {#if isPending(step.status)}
          <button
            class="btn btn-primary"
            onclick={() => save('active')}
            disabled={saving}
          >Start inspection</button>
        {:else if step.status === 'active'}
          <button class="btn" onclick={() => save()} disabled={saving}>
            Save progress
          </button>
          <button
            class="btn btn-primary"
            onclick={() => save('completed')}
            disabled={saving || !result}
          >Complete inspection</button>
        {/if}
      </div>
    </div>

    <div class="step-repair-context">
      {#if model}
        <Section title="System model">
            <div class="step-kb-card">
              <div class="step-kb-row">
                <strong>Model:</strong> {model?.name ?? '—'}
              </div>
              <div class="step-kb-row">
                <strong>SKU:</strong> {sku ?? '—'}
              </div>
              {#if model?.service?.required_skill_level}
                <div class="step-kb-row">
                  <strong>Skill:</strong> {model.service.required_skill_level}/5
                </div>
              {/if}
            </div>
        </Section>
      {/if}

      {#if pmChecklist.length > 0}
        <Section title="preventive maintenance checklist (from KB)">
            <div class="step-kb-checklist">
              {#each pmChecklist as item, i (i)}
                <div class="step-kb-checklist-item">
                  <span class="step-kb-check">☐</span>
                  {item}
                </div>
              {/each}
            </div>
        </Section>
      {/if}
    </div>
  </div>
</div>
