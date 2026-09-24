<script lang="ts">
  // Repair step — two-column surface: work log on the left, KB
  // context (failure modes + spare parts for the system's model) on
  // the right. Failure-mode selection pulls the typical fix into a
  // hint strip above the work-notes box.
  //
  // Fetches on mount:
  //   1. /api/assets/{assetId} — pull sku + phase for the header
  //   2. /api/catalog/models — match the sku to its KB record
  // Both are fire-and-forget; KB panels stay empty when the calls
  // fail (offline / missing data / unknown model).

  import { isPending, type StepStatus } from '../jobs/types';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { formatMoney } from '@boss/web-kit/ui/money';
  import { saveStep } from './stepWrite';

  type StepData = {
    id: string;
    kind: string;
    title: string;
    status: StepStatus;
    assignee_id: string | null;
    metadata: Record<string, unknown>;
    notes: string | null;
  };

  type FailureMode = {
    code: string;
    name: string;
    frequency: number;
    typical_fix: string;
  };
  type SparePart = {
    part_sku: string;
    name: string;
    unit_price_cents: number;
    currency: string;
    high_usage: boolean;
  };
  type CatalogModel = {
    sku: string;
    name: string;
    service?: {
      common_failure_modes?: FailureMode[];
      required_skill_level?: number;
    };
    spare_parts?: SparePart[];
  };
  type SystemInfo = { sku: string | null; phase: string; accountId: string | null };

  type Props = {
    step: StepData;
    jobId: string;
    onUpdate: () => void;
  };
  let { step, jobId, onUpdate }: Props = $props();

  const assetId = (step.metadata.asset_id as string | undefined) ?? '';

  let laborHours = $state<number>(Number(step.metadata.labor_hours ?? 0));
  let failureCode = $state<string>(String(step.metadata.failure_mode_code ?? ''));
  let workNotes = $state<string>(String(step.metadata.work_notes ?? ''));
  let saving = $state(false);
  let writeError = $state<string | null>(null);
  $effect(() => {
    // The surface instance is reused when the rail switches steps —
    // an error from step A must not render under step B.
    void step.id;
    writeError = null;
  });

  let systemInfo = $state<SystemInfo | null>(null);
  let model = $state<CatalogModel | null>(null);

  $effect(() => {
    if (!assetId) return;
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch(`/api/assets/${encodeURIComponent(assetId)}`);
        if (!r.ok || cancelled) return;
        const data = await r.json();
        const state = data.current_state;
        if (state && !cancelled) {
          systemInfo = {
            sku: state.sku,
            phase: state.phase,
            accountId: state.account_id ?? null,
          };
        }
      } catch {
        /* ignore */
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  $effect(() => {
    const sku = systemInfo?.sku;
    if (!sku) return;
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/catalog/models');
        if (!r.ok || cancelled) return;
        const rows = (await r.json()) as CatalogModel[];
        const match = rows.find((m) => m.sku === sku) ?? null;
        if (!cancelled) model = match;
      } catch {
        /* ignore */
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let failureModes = $derived<FailureMode[]>(
    model?.service?.common_failure_modes ?? [],
  );
  let spareParts = $derived<SparePart[]>(model?.spare_parts ?? []);
  let selectedTypicalFix = $derived(
    failureModes.find((fm) => fm.code === failureCode)?.typical_fix ?? '',
  );

  async function save(newStatus?: string): Promise<void> {
    saving = true;
    writeError = null;
    try {
      // The keys this surface owns, through the merge door; an emptied
      // field is sent as null and deleted, where it used to be cleared
      // by omission from a wholesale PUT (backlog e39a9d2a).
      const body = {
        ...(newStatus ? { status: newStatus } : {}),
        metadata: {
          labor_hours: laborHours,
          work_notes: workNotes || undefined,
          failure_mode_code: failureCode || undefined,
        },
      };
      const res = await saveStep(jobId, step.id, body);
      if (res.kind === 'failed') {
        writeError = res.error;
        return;
      }
      onUpdate();
    } finally {
      saving = false;
    }
  }
</script>

<div class="step-surface step-repair">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-kind-label">repair</span>
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
      <Section title="Work log">
          <div class="step-field">
            <label for="repair-labor-{step.id}">Labor hours</label>
            <input
              id="repair-labor-{step.id}"
              type="number"
              step="0.5"
              min="0"
              bind:value={laborHours}
              disabled={saving}
            />
          </div>

          <div class="step-field">
            <label for="repair-fm-{step.id}">Failure mode</label>
            <select
              id="repair-fm-{step.id}"
              bind:value={failureCode}
              disabled={saving}
            >
              <option value="">— Select —</option>
              {#each failureModes as fm (fm.code)}
                <option value={fm.code}>
                  {fm.code}: {fm.name} ({Math.round(fm.frequency * 100)}% frequency)
                </option>
              {/each}
            </select>
          </div>

          {#if failureCode && selectedTypicalFix}
            <div class="step-kb-hint">
              <strong>KB suggestion:</strong> {selectedTypicalFix}
            </div>
          {/if}

          <div class="step-field">
            <label for="repair-notes-{step.id}">Work notes</label>
            <textarea
              id="repair-notes-{step.id}"
              rows="4"
              bind:value={workNotes}
              disabled={saving}
              placeholder="Describe the diagnosis and repair..."
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
          >Start work</button>
        {:else if step.status === 'active'}
          <button class="btn" onclick={() => save()} disabled={saving}>
            Save progress
          </button>
          <button
            class="btn btn-primary"
            onclick={() => save('completed')}
            disabled={saving}
          >Mark complete</button>
        {/if}
      </div>
    </div>

    <div class="step-repair-context">
      {#if systemInfo}
        <Section title="System">
            <div class="step-kb-card">
              <div class="step-kb-row">
                <strong>ID:</strong>
                <EntityLink kind="asset" id={assetId} />
              </div>
              <div class="step-kb-row">
                <strong>Model:</strong> {model?.name ?? systemInfo?.sku ?? '—'}
              </div>
              <div class="step-kb-row">
                <strong>Phase:</strong> {systemInfo?.phase ?? '—'}
              </div>
              {#if model?.service?.required_skill_level}
                <div class="step-kb-row">
                  <strong>Skill required:</strong> {model.service.required_skill_level}/5
                </div>
              {/if}
            </div>
        </Section>
      {/if}

      {#if failureModes.length > 0}
        <Section title={`Known failure modes (${failureModes.length})`}>
            <div class="step-kb-list">
              {#each failureModes as fm (fm.code)}
                <button
                  type="button"
                  class="step-kb-item"
                  class:step-kb-item-active={failureCode === fm.code}
                  onclick={() => (failureCode = fm.code)}
                >
                  <div class="step-kb-item-header">
                    <span class="step-kb-code">{fm.code}</span>
                    <span class="step-kb-freq">{Math.round(fm.frequency * 100)}%</span>
                  </div>
                  <div class="step-kb-item-name">{fm.name}</div>
                  <div class="step-kb-item-fix">{fm.typical_fix}</div>
                </button>
              {/each}
            </div>
        </Section>
      {/if}

      {#if spareParts.length > 0}
        <Section title={`Spare parts (${spareParts.length})`}>
            <div class="step-kb-list">
              {#each spareParts.slice(0, 10) as p (p.part_sku)}
                <div class="step-kb-item">
                  <div class="step-kb-item-header">
                    <span class="step-kb-code">{p.part_sku}</span>
                    {#if p.high_usage}
                      <span class="step-kb-badge">high usage</span>
                    {/if}
                  </div>
                  <div class="step-kb-item-name">{p.name}</div>
                  <div class="step-kb-item-price">
                    {formatMoney({ amount_cents: p.unit_price_cents, currency: p.currency })}
                  </div>
                </div>
              {/each}
            </div>
        </Section>
      {/if}
    </div>
  </div>
</div>
