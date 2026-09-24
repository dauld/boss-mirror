<script lang="ts">
  // Engine controls for the simulator. POSTs to the boss-simulator
  // service's control surface under /simulator/api/control/*:
  //
  //   POST /simulator/api/control/pause          (no body)
  //   POST /simulator/api/control/resume         (no body)
  //   POST /simulator/api/control/restart-epoch  (no body) — confirm()
  //   POST /simulator/api/control/configure      { epoch_start?,
  //                                                epoch_end?,
  //                                                warp_factor? }
  //
  // All control endpoints return the updated SimClockState on success.
  // A 403 means the visitor isn't a signed-in operator — the engine
  // controls are operator-only, so we render a read-only notice and
  // disable the buttons rather than surfacing it as an error.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import type { SimClockState, JobLiveSummary, SimBehaviorConfig } from './types';

  const STATUS_POLL_MS = 3_000;
  // The daemon restarts on POST /config; give it a few seconds, then
  // re-read both the config and the clock so the editor reflects the
  // freshly-applied (and validated) effective config.
  const RESTART_RELOAD_MS = 5_000;

  let clock = $state<SimClockState | null>(null);
  // Set true once any control returns 403 — the visitor can't drive
  // the engine. Latches so the whole panel switches to read-only.
  let readOnly = $state(false);
  // Transient feedback for the last action (success or failure).
  let notice = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  // In-flight guard so double-clicks don't fire a control twice.
  let busy = $state(false);

  // Configure-form fields. All optional — only non-blank fields are
  // sent. warp_factor is parsed to a number; blank means "leave it".
  let epochStart = $state('');
  let epochEnd = $state('');
  let warpFactor = $state('');

  // Behavior config (GET/POST /simulator/api/config). Loaded on mount;
  // the four editor Sections bind directly to its nested objects so
  // edits accumulate here. `configError` is shown inline if the GET
  // fails (editor skipped, clock controls unaffected). `restarting`
  // latches true briefly after a successful Apply.
  let config = $state<SimBehaviorConfig | null>(null);
  let configError = $state<string | null>(null);
  let restarting = $state(false);

  let paused = $derived(clock?.paused ?? false);

  // Stable, sorted entry lists for the editor rows. Iterating entries
  // (not keys + index access) keeps the bound object references
  // definitely-defined — under noUncheckedIndexedAccess, `record[key]`
  // is `T | undefined`, which `bind:value` rejects. Empty when config
  // is null so the editor markup degrades cleanly. Each row binds to
  // the second tuple element, which is the live object reference (Svelte
  // 5 mutates it in place), so edits accumulate on `config`.
  let jobRateEntries = $derived(
    config ? Object.entries(config.job_rates).sort(byKey) : [],
  );
  let subjectRateEntries = $derived(
    config ? Object.entries(config.subject_rates).sort(byKey) : [],
  );
  let counterpartyEntries = $derived(
    config ? Object.entries(config.counterparty).sort(byKey) : [],
  );
  let anomalyEntries = $derived(
    config ? Object.entries(config.anomalies).sort(byKey) : [],
  );
  let periodicEntries = $derived(
    config ? Object.entries(config.periodic).sort(byKey) : [],
  );

  // Sort Object.entries() tuples by their key.
  function byKey(a: [string, unknown], b: [string, unknown]): number {
    return a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0;
  }

  // Sorted [name, value] pairs of a probs record — primitives, so we
  // can't bind to the tuple (no write-back); the markup uses an
  // explicit oninput handler to assign back through the object + key.
  function probEntries(probs: Record<string, number>): [string, number][] {
    return Object.entries(probs).sort(byKey);
  }

  // Coerce a number-input's string value to a finite number; blank or
  // unparseable falls back to 0 so a probs cell never becomes NaN.
  function numFromInput(v: string): number {
    const n = Number(v);
    return Number.isFinite(n) ? n : 0;
  }

  // Poll the live sim_clock so `paused` (and the rest of the clock)
  // stays current even when another operator drives the engine.
  async function refreshClock(): Promise<void> {
    try {
      const r = await fetch('/api/jobs/live', { headers: { accept: 'application/json' } });
      if (!r.ok) return;
      const body = (await r.json()) as JobLiveSummary;
      clock = body.sim_clock ?? null;
    } catch {
      // Silent — the next poll retries.
    }
  }

  // Shared POST helper. On 200, swallows the updated SimClockState and
  // refreshes local state. On 403, latches read-only mode. Otherwise
  // surfaces the error text.
  async function postControl(
    path: string,
    body?: Record<string, unknown>,
  ): Promise<boolean> {
    busy = true;
    actionError = null;
    notice = null;
    try {
      const r = await fetch(`/simulator/api/control/${path}`, {
        method: 'POST',
        headers: body
          ? { 'content-type': 'application/json', accept: 'application/json' }
          : { accept: 'application/json' },
        body: body ? JSON.stringify(body) : undefined,
      });
      if (r.status === 403) {
        readOnly = true;
        return false;
      }
      if (!r.ok) {
        const msg = await r.text();
        actionError = `HTTP ${r.status}${msg ? `: ${msg}` : ''}`;
        return false;
      }
      // Success — the endpoint returns the updated SimClockState.
      try {
        clock = (await r.json()) as SimClockState;
      } catch {
        // No/!JSON body — fall back to a fresh poll.
        await refreshClock();
      }
      return true;
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      busy = false;
    }
  }

  async function togglePause(): Promise<void> {
    const path = paused ? 'resume' : 'pause';
    if (await postControl(path)) {
      notice = paused ? 'Resumed.' : 'Paused.';
    }
  }

  async function restartEpoch(): Promise<void> {
    if (
      !window.confirm(
        'Restart the epoch? This rewinds the simulator clock and replays the model from the start of the epoch.',
      )
    ) {
      return;
    }
    if (await postControl('restart-epoch')) {
      notice = 'Epoch restart requested.';
    }
  }

  async function submitConfigure(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const body: Record<string, unknown> = {};
    if (epochStart.trim()) body['epoch_start'] = epochStart.trim();
    if (epochEnd.trim()) body['epoch_end'] = epochEnd.trim();
    if (warpFactor.trim()) {
      const n = Number(warpFactor.trim());
      if (Number.isFinite(n)) body['warp_factor'] = n;
    }
    if (Object.keys(body).length === 0) {
      actionError = 'Nothing to configure — set at least one field.';
      return;
    }
    if (await postControl('configure', body)) {
      notice = 'Configuration applied.';
    }
  }

  // Load the effective behavior config. Open read — works for demo
  // visitors too, so this runs regardless of operator status. On
  // failure we set an inline error and leave `config` null; the editor
  // markup is gated on `config` so the clock controls keep working.
  async function loadConfig(): Promise<void> {
    try {
      const r = await fetch('/simulator/api/config', {
        headers: { accept: 'application/json' },
      });
      if (!r.ok) {
        configError = `Couldn't load behavior config (HTTP ${r.status}).`;
        return;
      }
      config = (await r.json()) as SimBehaviorConfig;
      configError = null;
    } catch (e) {
      configError = `Couldn't load behavior config: ${
        e instanceof Error ? e.message : String(e)
      }`;
    }
  }

  // Build the POST body from the live edited config. Bound number
  // inputs can leave `step_speed_multiplier` as null/NaN when blanked;
  // coerce that to 1 (normal speed) so the daemon's validation passes
  // and the round-trip stays structurally complete. Everything else is
  // sent verbatim (passthrough fields preserved).
  function buildConfigBody(src: SimBehaviorConfig): SimBehaviorConfig {
    const ssm = src.meta.step_speed_multiplier;
    const meta = {
      ...src.meta,
      step_speed_multiplier:
        typeof ssm === 'number' && Number.isFinite(ssm) ? ssm : 1,
    };
    return { ...src, meta };
  }

  async function applyConfig(): Promise<void> {
    if (!config) return;
    if (
      !window.confirm(
        'Apply this behavior config? This restarts the simulator and ' +
          'rewinds in-flight sim state, replaying the model under the new ' +
          'configuration.',
      )
    ) {
      return;
    }
    busy = true;
    actionError = null;
    notice = null;
    try {
      const r = await fetch('/simulator/api/config', {
        method: 'POST',
        headers: { 'content-type': 'application/json', accept: 'application/json' },
        body: JSON.stringify(buildConfigBody(config)),
      });
      if (r.status === 403) {
        readOnly = true;
        return;
      }
      if (!r.ok) {
        // 422 (validation) carries the error string in the body.
        const msg = await r.text();
        actionError = `HTTP ${r.status}${msg ? `: ${msg}` : ''}`;
        return;
      }
      notice = 'Behavior config applied — the simulator is restarting…';
      restarting = true;
      // The daemon takes a few seconds to come back; re-read the config
      // and clock once it's up, then clear the restarting latch.
      window.setTimeout(() => {
        void (async () => {
          await Promise.all([loadConfig(), refreshClock()]);
          restarting = false;
        })();
      }, RESTART_RELOAD_MS);
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  onMount(() => {
    void refreshClock();
    void loadConfig();
    const handle = window.setInterval(() => void refreshClock(), STATUS_POLL_MS);
    return () => window.clearInterval(handle);
  });
</script>

<PageHeader
  eyebrow="Simulator"
  title="Engine controls"
  subtitle="Pause, resume, restart, and configure the simulator that drives the brewery tenant."
/>

{#if readOnly}
  <div class="notice readonly" role="status">
    <strong>Sim controls require a signed-in operator.</strong>
    You're viewing read-only — the controls below are disabled. Sign in
    as an operator to drive the simulator engine.
  </div>
{/if}

{#if notice}
  <div class="notice ok" role="status">{notice}</div>
{/if}
{#if actionError}
  <div class="notice err" role="alert">{actionError}</div>
{/if}

<div class="controls-grid">
  <Section title="Run state">
    <p class="run-state">
      The simulator is currently
      <span class="plate" class:plate-busy={!paused} class:plate-troubled={paused}
        >{paused ? 'paused' : 'running'}</span
      >.
    </p>
    <div class="btn-row">
      <button
        type="button"
        class="btn btn-primary"
        disabled={readOnly || busy}
        onclick={togglePause}
      >
        {paused ? 'Resume' : 'Pause'}
      </button>
      <button
        type="button"
        class="btn btn-danger-outline"
        disabled={readOnly || busy}
        onclick={restartEpoch}
      >
        Restart epoch
      </button>
    </div>
  </Section>

  <Section title="Configure">
    <form class="configure-form" onsubmit={submitConfigure}>
      <label class="field">
        <span class="field-label">Epoch start (YYYY-MM-DD)</span>
        <input
          type="date"
          bind:value={epochStart}
          disabled={readOnly || busy}
          placeholder="YYYY-MM-DD"
        />
      </label>
      <label class="field">
        <span class="field-label">Epoch end (YYYY-MM-DD)</span>
        <input
          type="date"
          bind:value={epochEnd}
          disabled={readOnly || busy}
          placeholder="YYYY-MM-DD"
        />
      </label>
      <label class="field">
        <span class="field-label">Warp factor</span>
        <input
          type="number"
          min="1"
          step="1"
          bind:value={warpFactor}
          disabled={readOnly || busy}
          placeholder="e.g. 1000"
        />
      </label>
      <div class="btn-row">
        <button type="submit" class="btn btn-primary" disabled={readOnly || busy}>
          Apply configuration
        </button>
      </div>
      <p class="hint">All fields optional — only the fields you set are changed.</p>
    </form>
  </Section>
</div>

<div class="behavior-header">
  <h2>Behavior configuration</h2>
  <p class="behavior-sub">
    How the simulator engages the public API — work generation,
    workforce pace, counterparty reliability, and cadences/anomalies.
    Applying any change <strong>restarts the simulator</strong> and
    rewinds in-flight sim state.
  </p>
</div>

{#if configError}
  <div class="load-failed" role="alert">{configError}</div>
{:else if !config}
  <div class="notice" role="status">Loading behavior config…</div>
{:else}
  {#if restarting}
    <div class="notice readonly" role="status">
      Simulator restarting — reloading the effective config shortly…
    </div>
  {/if}

  <div class="behavior-grid">
    <Section title="Work generation">
      <p class="hint">
        Per-Workflow open rates and per-SubjectKind arrival rates. Rate is
        the expected number opened per sim day.
      </p>
      <h4 class="subhead">Job rates</h4>
      <div class="rate-list">
        {#each jobRateEntries as [kind, jr] (kind)}
          <label class="field row">
            <span class="field-label">{kind}</span>
            <input
              type="number"
              min="0"
              step="0.1"
              bind:value={jr.rate}
              disabled={readOnly || busy}
            />
          </label>
        {/each}
        {#if jobRateEntries.length === 0}
          <p class="hint">No job rates configured.</p>
        {/if}
      </div>

      <h4 class="subhead">New-subject arrivals</h4>
      <div class="rate-list">
        {#each subjectRateEntries as [kind, sr] (kind)}
          <label class="field row">
            <span class="field-label">{kind}</span>
            <input
              type="number"
              min="0"
              step="0.1"
              bind:value={sr.rate}
              disabled={readOnly || busy}
            />
          </label>
        {/each}
        {#if subjectRateEntries.length === 0}
          <p class="hint">No subject rates configured.</p>
        {/if}
      </div>
    </Section>

    <Section title="Workforce execution">
      <p class="hint">
        How fast the simulated workforce advances steps, relative to the
        baseline cadence.
      </p>
      <label class="field">
        <span class="field-label">Step speed multiplier</span>
        <input
          type="number"
          min="0"
          step="0.1"
          bind:value={config.meta.step_speed_multiplier}
          disabled={readOnly || busy}
          placeholder="1"
        />
        <span class="hint">1 = normal · &lt;1 faster · &gt;1 slower</span>
      </label>
    </Section>

    <Section title="Counterparty reliability">
      <p class="hint">
        Probability each external counterparty emits its expected
        response, and the mean delay (in sim days) before it does.
      </p>
      <div class="rate-list">
        {#each counterpartyEntries as [name, cp] (name)}
          <div class="cp-row">
            <span class="cp-name">{name}</span>
            <label class="field inline">
              <span class="field-label">emit probability</span>
              <input
                type="number"
                min="0"
                max="1"
                step="0.01"
                bind:value={cp.emit_probability}
                disabled={readOnly || busy}
              />
            </label>
            <label class="field inline">
              <span class="field-label">mean delay (days)</span>
              <input
                type="number"
                min="0"
                step="1"
                bind:value={cp.delay.mean_days}
                disabled={readOnly || busy}
              />
            </label>
          </div>
        {/each}
        {#if counterpartyEntries.length === 0}
          <p class="hint">No counterparties configured.</p>
        {/if}
      </div>
    </Section>

    <Section title="Cadences & anomalies">
      <p class="hint">
        Per-Workflow anomaly probabilities. Each value is the chance the
        named anomaly is injected on a given Job.
      </p>
      <div class="rate-list">
        {#each anomalyEntries as [kind, an] (kind)}
          <div class="cp-row">
            <span class="cp-name">{kind}</span>
            {#each probEntries(an) as [probName, probVal] (probName)}
              <label class="field inline">
                <span class="field-label">{probName}</span>
                <input
                  type="number"
                  min="0"
                  max="1"
                  step="0.01"
                  value={probVal}
                  oninput={(e) =>
                    (an[probName] = numFromInput(e.currentTarget.value))}
                  disabled={readOnly || busy}
                />
              </label>
            {/each}
          </div>
        {/each}
        {#if anomalyEntries.length === 0}
          <p class="hint">No anomalies configured.</p>
        {/if}
      </div>

      <h4 class="subhead">Periodic cadences</h4>
      <p class="hint">Read-only in v1 — edit cadences via config for now.</p>
      <ul class="periodic-list">
        {#each periodicEntries as [name, p] (name)}
          <li>
            <span class="cp-name">{name}</span>
            <span class="periodic-meta">
              {p.cadence} · anchor {p.anchor_date}
            </span>
          </li>
        {/each}
        {#if periodicEntries.length === 0}
          <li class="hint">No periodic cadences configured.</li>
        {/if}
      </ul>
    </Section>
  </div>

  <div class="apply-row">
    <button
      type="button"
      class="btn btn-primary"
      disabled={readOnly || busy}
      onclick={applyConfig}
    >
      Apply behavior config
    </button>
    <span class="hint">Applying restarts the simulator.</span>
  </div>
{/if}

<style>
  .controls-grid {
    display: grid;
    grid-template-columns: minmax(280px, 1fr) minmax(280px, 1fr);
    gap: 24px;
    align-items: start;
  }
  /* Enamel (backlog 6f471ff6, car 4). The buttons are the shared .btn
     (the board's 13px/800 caps in a 2px ink frame; primary action blue,
     danger in troubled words), the fields inherit the web stylesheet's
     field rule (2px ink frame, amber halo on focus), the run state is a
     plate, and a failed read is the .load-failed rail — all from the one
     stylesheet the Simulator imports. What is left here is layout, and
     colours only as tokens. */
  .notice {
    border: 1px solid var(--hairline);
    border-radius: var(--radius);
    background: var(--card);
    padding: 12px 14px;
    margin-bottom: 16px;
    line-height: 1.5;
  }
  .notice.readonly {
    color: var(--warn);
  }
  .notice.ok {
    color: var(--ok);
  }
  /* A refused write is troubled, so it wears the failed read's rail. */
  .notice.err {
    border: 0;
    border-left: 8px solid var(--troubled);
    color: var(--troubled-ink);
    font-weight: 600;
  }
  .load-failed {
    margin-bottom: 16px;
  }
  .run-state {
    margin: 0 0 12px;
  }
  .btn-row {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .configure-form {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  /* The board's field label: 13.5px/700; help 12.5px muted. */
  .field-label {
    font-size: 13.5px;
    font-weight: 700;
  }
  .field input:disabled {
    opacity: 0.6;
  }
  .hint {
    margin: 0;
    font-size: 12.5px;
    color: var(--static);
  }
  .behavior-header {
    margin: 32px 0 16px;
    border-top: 1px solid var(--hairline);
    padding-top: 24px;
  }
  .behavior-header h2 {
    margin: 0 0 4px;
    font-size: 18px;
    color: var(--fog);
  }
  .behavior-sub {
    margin: 0;
    color: var(--static);
    line-height: 1.5;
    max-width: 70ch;
  }
  .behavior-grid {
    display: grid;
    grid-template-columns: minmax(280px, 1fr) minmax(280px, 1fr);
    gap: 24px;
    align-items: start;
  }
  .subhead {
    margin: 16px 0 8px;
    font-family: var(--font-mono);
    font-size: 11px;
    font-weight: 600;
    color: var(--static);
    text-transform: uppercase;
    letter-spacing: var(--ls-label);
  }
  .rate-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .field.row {
    flex-direction: row;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  .field.row .field-label {
    flex: 1 1 auto;
    overflow-wrap: anywhere;
  }
  .field.row input {
    flex: 0 0 110px;
    width: 110px;
  }
  .field.inline {
    gap: 4px;
  }
  .field.inline input {
    width: 110px;
  }
  .cp-row {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 12px;
    padding: 8px 0;
    border-bottom: 1px solid var(--hairline);
  }
  .cp-row:last-child {
    border-bottom: none;
  }
  .cp-name {
    flex: 1 1 100%;
    font-weight: 600;
    color: var(--fog);
    overflow-wrap: anywhere;
  }
  .periodic-list {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .periodic-list li {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: baseline;
    font-size: 0.85rem;
  }
  .periodic-meta {
    color: var(--static);
    font-size: 12.5px;
  }
  .apply-row {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 20px;
    flex-wrap: wrap;
  }
  @media (max-width: 820px) {
    .controls-grid,
    .behavior-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
