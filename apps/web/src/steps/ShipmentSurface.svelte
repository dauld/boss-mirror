<script lang="ts">
  import { isPending, isTerminal as _isTerminal, type StepStatus } from '../jobs/types';
  import { saveStep } from './stepWrite';
  // Shipment step surface — wholesale-keg-order's last tier and
  // the equipment-preventive-maintenance depot-return path. Captures carrier +
  // tracking + ETA so the wholesale-courier counterparty's scan
  // chain (in-transit → out-for-delivery → delivered) has
  // something to attach to.
  //
  // Demo framing: most fields are pre-populated by the Workflow
  // (origin = brewery, destination = customer account). User
  // confirms carrier + tracking and either marks shipped or
  // delivered. Scan-event timeline (if any) renders read-only
  // below.

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

  function pickStr(key: string, fallback = ''): string {
    const v = step.metadata[key];
    return typeof v === 'string' ? v : fallback;
  }

  let direction = $derived(pickStr('direction', 'outbound'));
  let origin = $derived(pickStr('origin', 'loc-brewery-brewhouse'));
  let destination = $derived(pickStr('destination'));

  let carrier = $state(pickStr('carrier', 'local-pickup'));
  let trackingNumber = $state(pickStr('tracking_number'));
  let shippedDate = $state(pickStr('shipped_date'));
  let estimatedDelivery = $state(pickStr('estimated_delivery'));
  let deliveredDate = $state(pickStr('delivered_date'));
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


  // Tracking events (read-only) — populated by the
  // shipping-sim-bridge when the wholesale-courier counterparty
  // fires its scan chain. Not editable from this surface.
  type TrackingEvent = {
    occurred_at: string;
    status: string;
    note?: string | null;
  };
  let trackingEvents = $derived<TrackingEvent[]>(
    Array.isArray(step.metadata.tracking_events)
      ? (step.metadata.tracking_events as TrackingEvent[])
      : [],
  );



  async function persist(status?: string): Promise<void> {
    saving = true;
    writeError = null;
    try {
      // The keys this surface owns, through the merge door; an emptied
      // date is sent as null and deleted, where it used to be cleared by
      // omission from a wholesale PUT (backlog e39a9d2a).
      const body = {
        notes: notes || undefined,
        status: status ?? step.status,
        metadata: {
          carrier,
          tracking_number: trackingNumber,
          shipped_date: shippedDate || undefined,
          estimated_delivery: estimatedDelivery || undefined,
          delivered_date: deliveredDate || undefined,
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

<div class="step-surface step-shipment">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-kind-label">{step.kind}</span>
    <span class="step-status step-status-{step.status}">{step.status}</span>
  </div>

  <div class="step-meta-row">
    <strong>Direction:</strong> {direction}
    <span class="muted"> · </span>
    <strong>From:</strong> <code class="mono">{origin}</code>
    {#if destination}
      <span class="muted"> → </span>
      <code class="mono">{destination}</code>
    {/if}
  </div>

  <div class="step-field step-assign-row">
    <label for={`carrier-${step.id}`}>Carrier</label>
    <select
      id={`carrier-${step.id}`}
      bind:value={carrier}
      disabled={terminal || saving}
    >
      <option value="local-pickup">Local pickup</option>
      <option value="freight">Freight</option>
      <option value="ups">UPS</option>
      <option value="fedex">FedEx</option>
      <option value="other">Other</option>
    </select>
  </div>

  <div class="step-field step-assign-row">
    <label for={`tracking-${step.id}`}>Tracking #</label>
    <input
      id={`tracking-${step.id}`}
      type="text"
      bind:value={trackingNumber}
      disabled={terminal || saving}
      placeholder="(optional)"
    />
  </div>

  <div class="step-field-row">
    <div class="step-field step-assign-row">
      <label for={`ship-${step.id}`}>Shipped</label>
      <input
        id={`ship-${step.id}`}
        type="date"
        bind:value={shippedDate}
        disabled={terminal || saving}
      />
    </div>
    <div class="step-field step-assign-row">
      <label for={`eta-${step.id}`}>ETA</label>
      <input
        id={`eta-${step.id}`}
        type="date"
        bind:value={estimatedDelivery}
        disabled={terminal || saving}
      />
    </div>
    <div class="step-field step-assign-row">
      <label for={`delivered-${step.id}`}>Delivered</label>
      <input
        id={`delivered-${step.id}`}
        type="date"
        bind:value={deliveredDate}
        disabled={terminal || saving}
      />
    </div>
  </div>

  {#if trackingEvents.length > 0}
    <div class="step-field">
      <label>Tracking events</label>
      <ul class="step-tracking">
        {#each trackingEvents as ev (ev.occurred_at + ev.status)}
          <li>
            <span class="tracking-time">{ev.occurred_at}</span>
            <span class="tracking-status">{ev.status}</span>
            {#if ev.note}<span class="tracking-note">— {ev.note}</span>{/if}
          </li>
        {/each}
      </ul>
    </div>
  {/if}

  <div class="step-field">
    <label for={`notes-${step.id}`}>Notes</label>
    <textarea
      id={`notes-${step.id}`}
      rows="2"
      bind:value={notes}
      placeholder="Routing notes, exceptions, dock #..."
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
        disabled={saving}
      >
        {saving ? 'Saving…' : (deliveredDate ? 'Mark delivered' : 'Mark shipped')}
      </button>
    {/if}
  </div>
</div>

<style>
  .step-field-row {
    display: flex;
    gap: 12px;
    flex-wrap: wrap;
  }
  .step-field-row .step-field {
    flex: 1;
    min-width: 130px;
  }
  .step-tracking {
    list-style: none;
    margin: 4px 0;
    padding: 0;
    font-size: 13px;
  }
  .step-tracking li {
    padding: 3px 0;
    border-bottom: 1px solid var(--hairline);
  }
  .step-tracking .tracking-time {
    color: var(--static);
    margin-right: 8px;
    font-variant-numeric: tabular-nums;
  }
  .step-tracking .tracking-status {
    font-weight: 500;
  }
  .step-tracking .tracking-note {
    color: var(--static);
    margin-left: 4px;
  }
  .muted {
    color: var(--static);
  }
</style>
