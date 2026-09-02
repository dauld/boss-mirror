<script lang="ts">
  // /it/experiments — the surface for controlled change.
  //
  // This replaces a placeholder that reserved the slot and fetched
  // nothing. What makes it worth building now is that the machinery
  // beneath it exists: `protocol-experiment` packets carry a
  // hypothesis stated BEFORE the result (the step order enforces it —
  // `measure` cannot open until `state` closes), and the per-version
  // terminal report already groups packets by their pinned
  // workflow_version.
  //
  // Two panels, because an experiment has two honest states:
  //
  //   * Running — the hypothesis and decision rule are visible while
  //     the answer is still unknown. That is the whole point: a
  //     prediction you can read before the number lands is falsifiable;
  //     one you read afterwards is a story.
  //   * Concluded — what was predicted, what was measured, what was
  //     decided, and the confounds admitted. Retired experiments are
  //     shown as prominently as promoted ones. A surface that quietly
  //     drops refuted hypotheses teaches people to stop writing them
  //     down.
  //
  // Failure renders as failure, per IncidentsPage: an experiments page
  // that shows an empty calm while the API is down is worse than one
  // that says it cannot read.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { href } from '../../router';
  import type { Job, Step } from '../../jobs/types';

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'failed'; message: string }
    | { kind: 'ready'; jobs: ReadonlyArray<Job> };

  let load = $state<LoadState>({ kind: 'loading' });

  async function fetchExperiments(): Promise<void> {
    load = { kind: 'loading' };
    try {
      const res = await fetch('/api/jobs?kind=protocol-experiment&limit=200');
      if (!res.ok) {
        load = { kind: 'failed', message: `the jobs API answered ${res.status}` };
        return;
      }
      const body = await res.json();
      const jobs = (body?.data ?? body ?? []) as ReadonlyArray<Job>;
      load = { kind: 'ready', jobs };
    } catch (e) {
      load = { kind: 'failed', message: e instanceof Error ? e.message : String(e) };
    }
  }
  onMount(fetchExperiments);

  const jobs = $derived(load.kind === 'ready' ? load.jobs : []);

  const running = $derived(
    [...jobs.filter((j) => j.status !== 'closed' && j.status !== 'cancelled')].sort(
      (a, b) => (b.opened_on ?? '').localeCompare(a.opened_on ?? ''),
    ),
  );
  const concluded = $derived(
    [...jobs.filter((j) => j.status === 'closed' || j.status === 'cancelled')].sort(
      (a, b) => (b.closed_on ?? '').localeCompare(a.closed_on ?? ''),
    ),
  );

  const stepsOf = (j: Job): ReadonlyArray<Step> =>
    [...(j.steps ?? [])].sort((a, b) => a.sort_order - b.sort_order);

  const stepMeta = (j: Job, slug: string): Record<string, unknown> =>
    (stepsOf(j).find((s) => s.spec_slug === slug)?.metadata ?? {}) as Record<string, unknown>;

  const field = (j: Job, slug: string, key: string): string => {
    const v = stepMeta(j, slug)[key];
    return typeof v === 'string' ? v : v == null ? '' : String(v);
  };

  /// The step someone can act on now — what the experiment is waiting for.
  const waitingOn = (j: Job): string => {
    const s = stepsOf(j).find((x) => x.status === 'ready' || x.status === 'active');
    return s?.spec_slug ?? '';
  };

  const outcomeOf = (j: Job): string => {
    const o = (j.metadata as Record<string, unknown> | undefined)?.outcome;
    return typeof o === 'string' ? o : '';
  };

  /// v5 widened the arms from workflow versions to anything: the
  /// `state` fields are now `control` / `candidate` (free text). The
  /// `*_version` names survive only on packets stated under v1–v4, so
  /// they are the fallback, not the field.
  const armOf = (j: Job, which: 'control' | 'candidate'): string =>
    field(j, 'state', which) || field(j, 'state', `${which}_version`);
</script>

<PageHeader
  title="Experiments"
  subtitle="Controlled change: a hypothesis on the record before the result."
/>

{#if load.kind === 'loading'}
  <p class="exp-msg">Loading experiments…</p>
{:else if load.kind === 'failed'}
  <div class="exp-failed" role="alert">
    <p class="exp-failed-text">
      Could not read experiments — {load.message}. This panel is blank because the
      record is unreachable, not because nothing is running.
    </p>
    <button class="exp-btn" type="button" onclick={fetchExperiments}>Retry</button>
  </div>
{:else}
  <section class="exp-running" aria-label="Running experiments">
    <h2 class="exp-h2">Running <span class="exp-count">{running.length}</span></h2>
    {#if running.length === 0}
      <p class="exp-empty">
        No experiment is running. Changes are shipping on judgement alone — which is
        fine for the obvious ones, and is how three predictions went wrong on 2026-08-26.
      </p>
    {:else}
      {#each running as j (j.id)}
        <article class="exp-card">
          <header class="exp-card-head">
            <a class="exp-title" href={href(`/ux/jobs/${j.id}`)}>{j.title}</a>
            <span class="exp-waiting">waiting on: {waitingOn(j) || '—'}</span>
          </header>
          {#if field(j, 'state', 'hypothesis')}
            <dl class="exp-dl">
              <dt>Hypothesis</dt>
              <dd>{field(j, 'state', 'hypothesis')}</dd>
              <dt>Metric</dt>
              <dd>{field(j, 'state', 'metric')}</dd>
              <dt>Arms</dt>
              <dd>{armOf(j, 'control')} vs {armOf(j, 'candidate')}</dd>
              <dt>Decision rule</dt>
              <dd>{field(j, 'state', 'decision_rule')}</dd>
            </dl>
          {:else}
            <p class="exp-note">
              No hypothesis recorded yet — the experiment cannot measure until it states one.
            </p>
          {/if}
        </article>
      {/each}
    {/if}
  </section>

  <section class="exp-concluded" aria-label="Concluded experiments">
    <h2 class="exp-h2">Concluded <span class="exp-count">{concluded.length}</span></h2>
    {#if concluded.length === 0}
      <p class="exp-empty">Nothing concluded yet.</p>
    {:else}
      {#each concluded as j (j.id)}
        <article class="exp-card exp-card-done">
          <header class="exp-card-head">
            <a class="exp-title" href={href(`/ux/jobs/${j.id}`)}>{j.title}</a>
            <span class="exp-outcome exp-outcome-{outcomeOf(j) || 'none'}">{outcomeOf(j) || '—'}</span>
          </header>
          <dl class="exp-dl">
            <dt>Predicted</dt>
            <dd>{field(j, 'state', 'hypothesis') || '—'}</dd>
            <dt>Measured</dt>
            <dd>
              control {field(j, 'measure', 'control_result') || '—'} · candidate
              {field(j, 'measure', 'candidate_result') || '—'} · n={field(j, 'measure', 'samples') || '?'}
            </dd>
            <dt>Decided</dt>
            <dd>{field(j, 'decide', 'decision') || '—'}</dd>
            {#if field(j, 'decide', 'confounds')}
              <dt>Confounds</dt>
              <dd class="exp-confounds">{field(j, 'decide', 'confounds')}</dd>
            {/if}
          </dl>
        </article>
      {/each}
    {/if}
  </section>
{/if}

<style>
  .exp-msg,
  .exp-empty,
  .exp-note {
    color: var(--text-muted);
    font-size: 0.9rem;
  }
  .exp-failed {
    border: 1px solid var(--danger, #b3261e);
    border-radius: var(--radius, 6px);
    padding: 0.75rem 1rem;
    margin-bottom: 1rem;
  }
  .exp-failed-text {
    margin: 0 0 0.5rem;
  }
  .exp-h2 {
    font-size: 1rem;
    margin: 1.25rem 0 0.5rem;
  }
  .exp-count {
    color: var(--text-muted);
    font-weight: 400;
  }
  .exp-card {
    border: 1px solid var(--border);
    border-radius: var(--radius, 6px);
    padding: 0.75rem 1rem;
    margin-bottom: 0.75rem;
  }
  .exp-card-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 1rem;
  }
  .exp-title {
    font-weight: 600;
  }
  .exp-waiting,
  .exp-outcome {
    font-size: 0.8rem;
    color: var(--text-muted);
    white-space: nowrap;
  }
  .exp-dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.25rem 0.75rem;
    margin: 0.5rem 0 0;
    font-size: 0.875rem;
  }
  .exp-dl dt {
    color: var(--text-muted);
  }
  .exp-dl dd {
    margin: 0;
  }
  .exp-confounds {
    color: var(--text-muted);
  }
</style>
