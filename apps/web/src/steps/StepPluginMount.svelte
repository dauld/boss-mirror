<script lang="ts">
  // Host a step-plugin mount-function inside a Svelte page.
  //
  // The plugin contract is framework-agnostic: bundles call
  // `window.__boss_register_step_plugin(kind, mount)` where `mount`
  // takes a container element and props, renders into it with
  // whatever tech it likes, and returns an optional cleanup fn.
  //
  // Layout: the card reserves a min-height via `.step-plugin-mount`
  // so the skeleton and the eventual plugin render occupy the same
  // box. Sibling steps below don't shift when the plugin hydrates.
  //
  // Re-mount semantics: the effect re-runs when the kind, the step's
  // ID or the step's STATUS changes — cleanup first, then mount with
  // the new props — and on nothing else. This comment used to say it
  // re-ran on every new step object, which was never true of the
  // effect (the step is read after an await, untracked); what remounted
  // the plugin on every packet reload was StepSurface's probe, and that
  // remount wiped an answer David was typing (backlog fec57f5f). A
  // mounted plugin owns its unsaved input, so a reload carrying the
  // same step in the same status must leave it alone. A different step
  // (a click on the rail) or a status move (completed, by this surface
  // or another actor) is a different thing to show, so it remounts.

  import {
    getStepPluginMount,
    pluginLoadFailure,
    type StepPluginProps,
    type PluginCleanup,
  } from './pluginHost';
  import GenericSurface from './GenericSurface.svelte';
  import type { StepStatus } from '../jobs/types';

  type Props = StepPluginProps & { kind: string };
  let { kind, step, jobId, onUpdate, currentUser }: Props = $props();

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'missing' }
    | { kind: 'failed'; reason: string }
    | { kind: 'ready' };

  let loadState: LoadState = $state<LoadState>({ kind: 'loading' });
  let container: HTMLDivElement | null = $state(null);
  // Bumped by the Retry affordance; failed loads are not cached, so
  // re-running the effect genuinely retries.
  let retryNonce = $state(0);
  // The plugin contract keeps status as a plain string (bundles are
  // framework-agnostic); the platform surface wants the union.
  let genericStep = $derived({ ...step, status: step.status as StepStatus });
  // Primitives, so a same-value reload does not re-run the effect.
  let stepId = $derived(step.id);
  let stepStatus = $derived(step.status);

  $effect(() => {
    if (!container) return;
    void retryNonce;
    void stepId;
    void stepStatus;
    const k = kind;
    const currentContainer = container;
    let cancelled = false;
    let cleanup: PluginCleanup | void;

    loadState = { kind: 'loading' };

    (async () => {
      const mount = await getStepPluginMount(k);
      if (cancelled) return;
      if (!mount) {
        // Two different facts, two different messages (ff87f782):
        // the registry has no plugin, or it HAS one whose bundle
        // failed to load — the second wore the first's message and
        // a decision surface degraded with no trace.
        const reason = pluginLoadFailure(k);
        loadState = reason ? { kind: 'failed', reason } : { kind: 'missing' };
        return;
      }
      currentContainer.replaceChildren();
      cleanup = mount(currentContainer, { step, jobId, onUpdate, currentUser });
      loadState = { kind: 'ready' };
    })();

    return () => {
      cancelled = true;
      if (cleanup) cleanup();
      currentContainer.replaceChildren();
    };
  });
</script>

<div class="step-surface step-plugin-mount">
  <div
    class="step-plugin-root"
    bind:this={container}
    style:display={loadState.kind === 'ready' ? 'block' : 'none'}
  ></div>

  {#if loadState.kind === 'loading'}
    <div class="step-plugin-skeleton" aria-hidden="true">
      <div class="step-surface-header">
        <h3>{step.title}</h3>
        <span class="step-kind-label">{kind}</span>
        <span class="step-status step-status-{step.status}">{step.status}</span>
      </div>
      <div class="step-plugin-skeleton-row short"></div>
      <div class="step-plugin-skeleton-row mid"></div>
      <div class="step-plugin-skeleton-row tall"></div>
      <div class="step-plugin-skeleton-row short"></div>
    </div>
  {:else if loadState.kind === 'missing'}
    <div class="step-plugin-missing">
      No plugin registered for <code class="mono">{kind}</code>.
    </div>
  {:else if loadState.kind === 'failed'}
    <div class="step-plugin-failed" role="alert">
      <span>
        The <code class="mono">{kind}</code> surface failed to load
        ({loadState.reason}) — using the generic form below.
      </span>
      <button class="step-btn" onclick={() => (retryNonce += 1)}>Retry</button>
    </div>
    <GenericSurface step={genericStep} {jobId} {onUpdate} />
  {/if}
</div>
