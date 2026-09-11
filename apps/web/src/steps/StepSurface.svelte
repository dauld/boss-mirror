<script lang="ts">
  // Step dispatcher — mounts the surface the StepType REGISTRY names
  // for this step's kind (docs/architecture-decisions.md §Step UX &
  // frontend). The kind → surface mapping is
  // registry data served by /api/jobs/step-types; this file holds the
  // surface-id → component table for the platform-shipped surfaces.
  // Precedence: tenant StepPlugin (if one is registered for the kind)
  // → platform surface named by the registry → GenericSurface (the
  // universal fields/notes card — also the loading/unknown fallback).
  //
  // There is deliberately no kind match here — the
  // no-step-kind-match lint fails the build if one returns.

  import GenericSurface from './GenericSurface.svelte';
  import DecisionContext from './DecisionContext.svelte';
  import StepProcedure from './StepProcedure.svelte';
  import ApprovalSurface from './ApprovalSurface.svelte';
  import RepairSurface from './RepairSurface.svelte';
  import InspectionSurface from './InspectionSurface.svelte';
  import BillingSurface from './BillingSurface.svelte';
  import IntakeSurface from './IntakeSurface.svelte';
  import ShipmentSurface from './ShipmentSurface.svelte';
  import SchedulingSurface from './SchedulingSurface.svelte';
  import ProductionConsumeSurface from './ProductionConsumeSurface.svelte';
  import HandoffSurface from './HandoffSurface.svelte';
  import ReceivingSurface from './ReceivingSurface.svelte';
  import ProcurementSurface from './ProcurementSurface.svelte';
  import StepPluginMount from './StepPluginMount.svelte';
  import { probeActivePlugin } from './pluginHost';
  import {
    loadStepTypeRegistry,
    stepTypeRegistry,
    surfaceOf,
  } from './surfaceRegistry.svelte';
  import { session } from '@boss/web-kit/session/session.svelte';
  import WriteGate from '@boss/web-kit/ui/WriteGate.svelte';
  import FileAttachments from '../content/FileAttachments.svelte';
  import type { StepStatus } from '../jobs/types';

  type StepData = {
    id: string;
    kind: string;
    title: string;
    status: StepStatus;
    assignee_id: string | null;
    sort_order: number;
    sign_offs_required?: string[];
    sign_offs?: {
      authority_id: string;
      role: string;
      stamped_at: string;
      shape_hash: string;
    }[];
    metadata: Record<string, unknown>;
    notes: string | null;
  };

  type Props = {
    step: StepData;
    jobId: string;
    onUpdate: () => void;
  };
  let { step, jobId, onUpdate }: Props = $props();

  // Async-resolved: does the boss-jobs step-plugin registry have
  // an active row for this kind? Until the fetch returns we
  // render GenericSurface; if a plugin IS registered, we swap to
  // StepPluginMount once the result lands. A FAILED probe is its
  // own state — it renders the degraded-registry notice below, it
  // is not silently "no plugin".
  let pluginAvailable = $state<boolean | null>(null);
  let pluginProbeFailed = $state(false);
  // Bumped by the Retry affordance so the probe effect re-runs.
  let retryNonce = $state(0);
  $effect(() => {
    void retryNonce;
    pluginAvailable = null;
    let cancelled = false;
    probeActivePlugin(step.kind).then((probe) => {
      if (cancelled) return;
      if (probe.kind === 'failed') {
        pluginProbeFailed = true;
        pluginAvailable = false;
      } else {
        pluginProbeFailed = false;
        pluginAvailable = probe.active;
      }
    });
    return () => {
      cancelled = true;
    };
  });

  // The one-fetch downgrade (packet cc9d7fc6): when either registry
  // read failed, every step silently rendered the generic fallback
  // for the rest of the session. The fallback stays (degraded but
  // usable) — the failure just becomes visible and retryable.
  let registryDegraded = $derived(
    stepTypeRegistry.value.kind === 'error' || pluginProbeFailed,
  );

  function retryRegistries(): void {
    if (stepTypeRegistry.value.kind === 'error') {
      void loadStepTypeRegistry();
    }
    retryNonce += 1;
  }

  let user = $derived(
    session.value.kind === 'ready'
      ? { id: session.value.user.id, role: session.value.user.role }
      : undefined,
  );
</script>

<!-- Every step surface — platform, generic fallback, and mounted
     plugins alike — stands behind the readonly gate. This dispatcher
     is the shared write path for step work (JobDetailPage,
     StepFocusPage, DecideModal all mount it), so gating HERE is the
     one edit instead of one per surface. -->
<WriteGate>
<!-- The step's authored instructions, above BOTH sides of the plugin
     fork (backlog 3c640ac3). The decision panel below is exempt for
     plugin-backed steps because a mounted plugin is its own
     presentation of the case — but none of the thirteen bundles under
     infra/step-plugins reads `procedure` at all, so exempting them here
     would leave a plugin-backed step as blind to its own instructions
     as the generic surface was. A procedure is what the protocol says
     about doing the work; it is not a presentation choice. -->
<StepProcedure {step} />
{#if pluginAvailable === true}
  <!-- Plugin-backed steps can also take the whole viewport. Reading
       tasks (a design review is a document plus decisions) compete
       badly with the job chrome and step list around this panel. -->
  <div class="step-surface-expand">
    <a class="step-surface-expand-link" href={`/ux/jobs/${jobId}/steps/${step.id}`}>
      Open full page ↗
    </a>
  </div>
  <StepPluginMount
    kind={step.kind}
    {step}
    {jobId}
    {onUpdate}
    currentUser={user}
  />
{:else}
  {#if registryDegraded}
    <div class="step-registry-error" role="alert">
      <span>
        Couldn't load the step-surface registry — showing the generic
        surface for now.
      </span>
      <button class="step-btn" onclick={retryRegistries}>Retry</button>
    </div>
  {/if}
  <!-- Non-plugin surfaces get the packet's case rendered above the
       action (19db52de: "there is just a sign and complete button,
       which doesn't seem like much of a choice"). A mounted plugin is
       its own presentation, so the panel lives on this side of the
       fork — once, for every platform surface and the generic
       fallback alike. -->
  <DecisionContext {step} {jobId} />
  {#if surfaceOf(step.kind) === 'approval'}
    <ApprovalSurface {step} {jobId} {onUpdate} />
  {:else if surfaceOf(step.kind) === 'repair'}
    <RepairSurface {step} {jobId} {onUpdate} />
  {:else if surfaceOf(step.kind) === 'inspection'}
    <InspectionSurface {step} {jobId} {onUpdate} />
  {:else if surfaceOf(step.kind) === 'billing'}
    <BillingSurface {step} {jobId} {onUpdate} />
  {:else if surfaceOf(step.kind) === 'intake'}
    <IntakeSurface {step} {jobId} {onUpdate} />
  {:else if surfaceOf(step.kind) === 'shipment'}
    <ShipmentSurface {step} {jobId} {onUpdate} />
  {:else if surfaceOf(step.kind) === 'scheduling'}
    <SchedulingSurface {step} {jobId} {onUpdate} />
  {:else if surfaceOf(step.kind) === 'production-consume'}
    <ProductionConsumeSurface {step} {jobId} {onUpdate} />
  {:else if surfaceOf(step.kind) === 'handoff'}
    <HandoffSurface {step} {jobId} {onUpdate} />
  {:else if surfaceOf(step.kind) === 'receiving'}
    <ReceivingSurface {step} {jobId} {onUpdate} />
  {:else if surfaceOf(step.kind) === 'procurement'}
    <ProcurementSurface {step} {jobId} {onUpdate} />
  {:else}
    <GenericSurface {step} {jobId} {onUpdate} />
  {/if}
{/if}

<!--
  Attachments slot — same component every step kind gets, regardless
  of which surface above rendered. Files are a column on every
  primitive, not a per-kind affordance (docs/architecture-decisions.md
  §Content, files, knowledge).
  Lives below the dispatched surface so it doesn't compete with the
  step's primary controls; collapsed empty when there are no files.
-->
<div class="step-attachments">
  <FileAttachments targetKind="step" targetId={step.id} />
</div>
</WriteGate>

<style>
  .step-surface-expand { display: flex; justify-content: flex-end; }
  .step-surface-expand-link {
    font-size: 12px;
    color: var(--text-dim, #78716c);
    text-decoration: none;
    padding: 2px 6px;
    border-radius: 4px;
  }
  .step-surface-expand-link:hover {
    background: var(--bg, #f5f5f4);
    color: var(--text, #1c1917);
  }
  .step-attachments {
    margin-top: 12px;
    padding: 8px 12px;
    border-top: 1px dashed var(--border);
  }
</style>
