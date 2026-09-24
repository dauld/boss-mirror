<script lang="ts">
  // Debug gear — floating button in the lower-right that opens a
  // compact panel. Houses the debug-mode toggle (shared state in
  // debugMode.svelte.ts — gates operator-only UI across the app);
  // future convenience actions land inside the `debug mode on` block.
  //
  // Customer-facing simulator controls live in the upper-right
  // SimulatorPanel (Reset / New order / Wholesale / Anomaly);
  // operator/dev-facing diagnostics stay here. The bottom-right
  // corner used to also house the SimClockBadge — the badge moved
  // to upper-right on 2026-05-02 so the gear has the corner to
  // itself.
  //
  // Gated on the session user's role — visible only to
  // `platform-admin`. This matches the deploy-time identity used
  // by every BOSS bootstrap binary (boss-brewery-bootstrap,
  // boss-policy-bootstrap, the brewery-engine LiveApiOutput), so
  // the SPA gear and the CLI tooling share one privilege model.

  import { session } from '@boss/web-kit/session/session.svelte';
  import { moduleEnabled } from '@boss/web-kit/session/manifest.svelte';
  import { debugState, setDebugMode } from './debugMode.svelte';
  import {
    showSimClock,
    showResetInstructions,
    runHire,
    placeShopOrder,
    placeWholesaleOrder,
    triggerAnomaly,
    type SimLogger,
  } from './simulations';

  const ALLOWED_ROLE = 'platform-admin';
  const LOG_MAX = 60;

  type LogEntry = { at: string; msg: string; level: 'info' | 'error' };

  let open = $state(false);
  let log = $state<LogEntry[]>([]);
  let running = $state(false);

  let user = $derived(session.value.kind === 'ready' ? session.value.user : null);
  let allowed = $derived(!!user && user.role === ALLOWED_ROLE);

  function toggleOpen() {
    open = !open;
  }

  function toggleDebug() {
    setDebugMode(!debugState.enabled);
  }

  function close() {
    open = false;
  }

  // Attach the Escape-to-close listener via $effect rather than
  // a svelte:window tag: the bun+svelte HMR bundler crashes on the
  // svelte:window event lookup ($.window resolves undefined when
  // DebugGear's effect runs first), which takes the whole app down.
  // A direct addEventListener is the same behaviour without the
  // bundler-internal dependency.
  $effect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === 'Escape' && open) close();
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  });

  function hhmmss(): string {
    const d = new Date();
    return [d.getHours(), d.getMinutes(), d.getSeconds()]
      .map((n) => String(n).padStart(2, '0'))
      .join(':');
  }

  function appendLog(msg: string, level: 'info' | 'error' = 'info'): void {
    log = [...log.slice(-(LOG_MAX - 1)), { at: hhmmss(), msg, level }];
  }

  function clearLog(): void {
    log = [];
  }

  // Operator-relevant actions for the playground.
  //
  // #87 rethink: tenant-specific quick-action buttons (New shop
  // order / Wholesale order / Anomaly / Hire) live here in
  // operator-only debug land. The always-visible top-right
  // Simulator panel now carries only the universal controls
  // (clock, view-as-of, reset loop, pause/resume) that apply to
  // every tenant. The four packet-opening buttons are shaped for the
  // playground tenant — they hardcode its Workflows (direct-shop-order
  // / wholesale-keg-order / vendor-delay-anomaly / hiring), so they
  // ride behind its `sim` module (ce68f137): a company running on
  // BOSS has no simulation to drive and must never be offered a
  // button that files another tenant's packets. A future "Open Job…"
  // picker that lists active Workflows from the registry would let
  // any tenant exercise its own flows without per-tenant SPA code.
  type Sim = { label: string; run: (l: SimLogger) => Promise<void> };
  const UNIVERSAL: ReadonlyArray<Sim> = [
    { label: 'Show sim clock', run: showSimClock },
    { label: 'Reset to baseline (host instructions)', run: showResetInstructions },
  ];
  const PLAYGROUND: ReadonlyArray<Sim> = [
    { label: 'Hire an employee', run: runHire },
    { label: 'Place direct-shop order', run: placeShopOrder },
    { label: 'Place wholesale order', run: placeWholesaleOrder },
    { label: 'Trigger anomaly', run: triggerAnomaly },
  ];
  let sims = $derived<ReadonlyArray<Sim>>(
    moduleEnabled('sim') ? [...UNIVERSAL, ...PLAYGROUND] : UNIVERSAL,
  );

  async function runSim(
    label: string,
    fn: (l: SimLogger) => Promise<void>,
  ): Promise<void> {
    if (running) return;
    running = true;
    appendLog(`— ${label} —`);
    try {
      await fn(appendLog);
    } catch (e) {
      appendLog(e instanceof Error ? e.message : String(e), 'error');
    } finally {
      running = false;
    }
  }
</script>

{#if allowed}
  <div class="debug-gear">
    {#if open}
      <div class="debug-gear-panel" role="dialog" aria-label="Debug panel">
        <div class="debug-gear-header">
          <strong>Debug</strong>
          <button
            type="button"
            class="debug-gear-close"
            aria-label="Close debug panel"
            onclick={close}
          >×</button>
        </div>

        <label class="debug-gear-toggle">
          <input type="checkbox" checked={debugState.enabled} onchange={toggleDebug} />
          <span>Debug mode</span>
        </label>

        {#if debugState.enabled}
          <div class="debug-gear-actions">
            <div class="debug-gear-sim-grid">
              {#each sims as s (s.label)}
                <button
                  type="button"
                  class="debug-gear-sim-btn"
                  disabled={running}
                  onclick={() => runSim(s.label, s.run)}
                >
                  {s.label}
                </button>
              {/each}
            </div>
            {#if log.length > 0}
              <div class="debug-gear-log">
                {#each log as entry, i (i)}
                  <div class="debug-gear-log-row" class:debug-gear-log-err={entry.level === 'error'}>
                    <span class="debug-gear-log-at">{entry.at}</span>
                    <span class="debug-gear-log-msg">{entry.msg}</span>
                  </div>
                {/each}
              </div>
              <button type="button" class="debug-gear-log-clear" onclick={clearLog}>
                Clear log
              </button>
            {/if}
          </div>
        {/if}

        <div class="debug-gear-ident">
          {user?.name} · {user?.email}
        </div>
      </div>
    {/if}

    <button
      type="button"
      class="debug-gear-btn"
      class:debug-gear-btn-on={debugState.enabled}
      aria-label="Debug panel"
      aria-expanded={open}
      onclick={toggleOpen}
    >
      <svg
        viewBox="0 0 24 24"
        width="20"
        height="20"
        aria-hidden="true"
        fill="none"
        stroke="currentColor"
        stroke-width="1.8"
        stroke-linecap="round"
        stroke-linejoin="round"
      >
        <circle cx="12" cy="12" r="3"></circle>
        <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"></path>
      </svg>
    </button>
  </div>
{/if}

<style>
  .debug-gear {
    position: fixed;
    right: 16px;
    bottom: 16px;
    z-index: 100;
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 8px;
    pointer-events: none;
  }
  .debug-gear > * {
    pointer-events: auto;
  }

  .debug-gear-btn {
    width: 40px;
    height: 40px;
    border-radius: 20px;
    border: 1px solid var(--border-strong);
    background: var(--band);
    color: var(--on-band);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition: transform 0.12s ease, background 0.12s ease;
  }
  .debug-gear-btn:hover {
    transform: rotate(20deg);
    background: var(--signal);
  }
  .debug-gear-btn-on {
    background: var(--clear);
    border-color: var(--clear);
  }
  .debug-gear-btn-on:hover {
    background: var(--clear);
  }

  .debug-gear-panel {
    width: 320px;
    background: var(--ink);
    border: 1px solid var(--hairline);
    border-radius: 8px;
    padding: 12px 14px;
    font-size: 13px;
    color: var(--fog);
    display: flex;
    flex-direction: column;
    gap: 10px;
    max-height: calc(100vh - 100px);
    overflow: hidden;
  }
  .debug-gear-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .debug-gear-close {
    border: none;
    background: none;
    color: var(--static);
    font-size: 20px;
    line-height: 1;
    cursor: pointer;
    padding: 0 4px;
  }
  .debug-gear-close:hover {
    color: var(--fog);
  }
  .debug-gear-toggle {
    display: flex;
    align-items: center;
    gap: 8px;
    cursor: pointer;
    user-select: none;
  }
  .debug-gear-actions {
    padding-top: 8px;
    border-top: 1px solid var(--hairline);
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-height: 0;
  }
  .debug-gear-sim-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 6px;
  }
  .debug-gear-sim-btn {
    padding: 6px 8px;
    font-size: 12px;
    background: var(--ink-raised);
    border: 1px solid var(--hairline);
    border-radius: 4px;
    color: var(--fog);
    cursor: pointer;
    text-align: left;
  }
  .debug-gear-sim-btn:hover:not(:disabled) {
    background: var(--ok-wash);
    border-color: var(--clear);
  }
  .debug-gear-sim-btn:disabled {
    opacity: 0.5;
    cursor: wait;
  }
  .debug-gear-log {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 11px;
    line-height: 1.45;
    background: var(--ink-raised);
    color: var(--fog);
    padding: 8px 10px;
    border-radius: 4px;
    max-height: 200px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .debug-gear-log-row {
    display: flex;
    gap: 8px;
  }
  .debug-gear-log-at {
    color: var(--static);
    flex-shrink: 0;
  }
  .debug-gear-log-msg {
    white-space: pre-wrap;
    word-break: break-word;
  }
  .debug-gear-log-err .debug-gear-log-msg {
    color: var(--err);
  }
  .debug-gear-log-clear {
    align-self: flex-end;
    font-size: 11px;
    padding: 2px 8px;
    background: none;
    border: 1px solid var(--hairline);
    border-radius: 3px;
    color: var(--static);
    cursor: pointer;
  }
  .debug-gear-log-clear:hover {
    color: var(--fog);
  }
  .debug-gear-ident {
    padding-top: 8px;
    border-top: 1px solid var(--hairline);
    font-size: 11px;
    color: var(--static);
  }
</style>
