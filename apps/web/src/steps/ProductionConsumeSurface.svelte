<script lang="ts">
  import { isPending, isTerminal as _isTerminal, type StepStatus } from '../jobs/types';
  import { saveStep } from './stepWrite';
  // Production-consume step surface — drains raw ingredients
  // consumed by a brewing batch. The brewer confirms the
  // ingredients_consumed list (typically pre-populated by the
  // Workflow from the recipe), and the inventory side-effect
  // handler decrements each part_sku's on_hand on done.
  //
  // Demo framing: the recipe arrives populated. The brewer's job
  // is to confirm the actual draw (occasional adjustments for
  // partial bags / weight variance) and mark done.

  type IngredientDraw = {
    part_sku: string;
    qty: number;
    description?: string;
  };

  type StepData = {
    id: string;
    kind: string;
    title: string;
    status: StepStatus;
    assignee_id: string | null;
    metadata: Record<string, unknown>;
    notes: string | null;
  };

  type Props = {
    step: StepData;
    jobId: string;
    onUpdate: () => void;
  };
  let { step, jobId, onUpdate }: Props = $props();

  function asDraws(v: unknown): IngredientDraw[] {
    if (!Array.isArray(v)) return [];
    return v
      .filter((x): x is Record<string, unknown> => typeof x === 'object' && x !== null)
      .map((x) => ({
        part_sku: typeof x.part_sku === 'string' ? x.part_sku : '',
        qty: typeof x.qty === 'number' ? x.qty : 0,
        description:
          typeof x.description === 'string' ? x.description : undefined,
      }));
  }

  let draws = $state<IngredientDraw[]>(
    asDraws(step.metadata.ingredients_consumed),
  );
  let batchId = $state(
    typeof step.metadata.batch_id === 'string' ? step.metadata.batch_id : '',
  );
  let notes = $state(step.notes ?? '');
  let saving = $state(false);
  let writeError = $state<string | null>(null);
  $effect(() => {
    // The surface instance is reused when the rail switches steps —
    // an error from step A must not render under step B.
    void step.id;
    writeError = null;
  });
  let terminal = $derived(_isTerminal(step.status));




  function updateQty(idx: number, qty: number): void {
    draws = draws.map((d, i) => (i === idx ? { ...d, qty } : d));
  }

  async function persist(status?: string): Promise<void> {
    saving = true;
    writeError = null;
    try {
      // The keys this surface owns, through the merge door; an emptied
      // batch id is sent as null and deleted, where it used to be
      // cleared by omission from a wholesale PUT (backlog e39a9d2a).
      const body = {
        notes: notes || undefined,
        status: status ?? step.status,
        metadata: {
          ingredients_consumed: draws,
          batch_id: batchId || undefined,
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

<div class="step-surface step-production-consume">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-kind-label">{step.kind}</span>
    <span class="step-status step-status-{step.status}">{step.status}</span>
  </div>

  <div class="step-field step-assign-row">
    <label for={`batch-${step.id}`}>Batch</label>
    <input
      id={`batch-${step.id}`}
      type="text"
      bind:value={batchId}
      disabled={terminal || saving}
      placeholder="e.g. PALE-2026-0418-A"
    />
  </div>

  {#if draws.length > 0}
    <div class="step-field">
      <label>Ingredients</label>
      <table class="step-line-items">
        <thead>
          <tr>
            <th class="col-sku">SKU</th>
            <th class="col-desc">Description</th>
            <th class="col-qty">Qty</th>
          </tr>
        </thead>
        <tbody>
          {#each draws as d, idx (idx)}
            <tr>
              <td><code class="mono">{d.part_sku}</code></td>
              <td class="desc">{d.description ?? ''}</td>
              <td>
                <input
                  type="number"
                  min="0"
                  step="0.01"
                  value={d.qty}
                  disabled={terminal || saving}
                  oninput={(e) => {
                    const n = parseFloat((e.target as HTMLInputElement).value);
                    if (Number.isFinite(n)) updateQty(idx, n);
                  }}
                />
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {:else}
    <div class="step-empty">
      No ingredient draws on this step yet — the Workflow's recipe
      should populate <code class="mono">ingredients_consumed</code>
      via metadata_defaults.
    </div>
  {/if}

  <div class="step-field">
    <label for={`notes-${step.id}`}>Notes</label>
    <textarea
      id={`notes-${step.id}`}
      rows="2"
      bind:value={notes}
      placeholder="Yield, gravity reading, weight variance..."
      disabled={terminal}
    ></textarea>
  </div>

  {#if writeError}
    <p class="step-write-error" role="alert">{writeError}</p>
  {/if}

  <div class="step-actions">
    {#if !terminal && isPending(step.status)}
      <button
        class="step-btn step-btn-primary"
        onclick={() => persist('active')}
        disabled={saving}
      >
        Start
      </button>
    {/if}
    {#if !terminal && step.status === 'active'}
      <button
        class="step-btn step-btn-primary"
        onclick={() => persist('completed')}
        disabled={saving || draws.length === 0}
        title={draws.length === 0
          ? 'No ingredients to consume — set ingredients_consumed first'
          : ''}
      >
        {saving ? 'Saving…' : 'Confirm draw'}
      </button>
    {/if}
  </div>
</div>

<style>
  .step-line-items {
    width: 100%;
    border-collapse: collapse;
    margin-top: 4px;
    font-size: 13px;
  }
  .step-line-items th {
    text-align: left;
    font-weight: 600;
    padding: 4px 6px;
    border-bottom: 1px solid var(--border);
    color: var(--static);
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }
  .step-line-items td {
    padding: 4px 6px;
    border-bottom: 1px solid var(--hairline);
  }
  .step-line-items .col-qty { width: 110px; }
  .step-line-items .col-sku { width: 180px; }
  .step-line-items .col-desc {}
  .step-line-items .desc {
    color: var(--static);
  }
  .step-line-items input[type="number"] {
    width: 100%;
    box-sizing: border-box;
    padding: 3px 5px;
    font-size: 13px;
    text-align: right;
    font-variant-numeric: tabular-nums;
    border: 1px solid var(--border);
    border-radius: 3px;
  }
  .step-empty {
    padding: 8px 12px;
    color: var(--static);
    font-size: 13px;
    font-style: italic;
    background: var(--ink-raised);
    border-radius: 4px;
    margin: 6px 0;
  }
</style>
