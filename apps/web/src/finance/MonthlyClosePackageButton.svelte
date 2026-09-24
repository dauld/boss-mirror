<script lang="ts">
  // Monthly close package — pick a month, get a ZIP with TB + IS + BS
  // + CF + README. Lives at the Finance page level so auditors find it
  // without drilling into a specific statement tab.

  import {
    buildMonthlyClosePackage,
    downloadZip,
    type CloseResult,
  } from './monthlyClose';
  import { appNow } from '@boss/web-kit/sim-clock';

  function defaultMonth(): string {
    // Default to the previous calendar month — an operator pulling a
    // "monthly close" almost always means "the month that just
    // wrapped," not the current in-flight one.
    const d = appNow();
    d.setDate(1);
    d.setMonth(d.getMonth() - 1);
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`;
  }

  type Status =
    | { kind: 'idle' }
    | { kind: 'busy' }
    | { kind: 'done'; warnings: ReadonlyArray<string> }
    | { kind: 'error'; message: string };

  let open = $state(false);
  let month = $state(defaultMonth());
  let status = $state<Status>({ kind: 'idle' });

  async function onDownload(): Promise<void> {
    status = { kind: 'busy' };
    try {
      const result: CloseResult = await buildMonthlyClosePackage(month);
      downloadZip(result.filename, result.blob);
      status = { kind: 'done', warnings: result.warnings };
    } catch (e) {
      status = {
        kind: 'error',
        message: e instanceof Error ? e.message : String(e),
      };
    }
  }

  function toggle(): void {
    open = !open;
    if (!open) status = { kind: 'idle' };
  }
</script>

<div class="mcp-wrap">
  <button type="button" class="fin-new-invoice" onclick={toggle}>
    {open ? 'Close' : 'Monthly close package'}
  </button>

  {#if open}
    <div class="mcp-panel" role="dialog" aria-label="Monthly close package">
      <div class="mcp-row">
        <label class="mcp-label">
          Month
          <input type="month" bind:value={month} />
        </label>
        <button
          type="button"
          class="fin-new-invoice"
          onclick={onDownload}
          disabled={status.kind === 'busy'}
        >
          {status.kind === 'busy' ? 'Building…' : 'Download ZIP'}
        </button>
      </div>
      <p class="mcp-note">
        Bundles trial balance, income statement, balance sheet, and cash
        flow for the selected month plus a README documenting source
        endpoints. All amounts in USD.
      </p>
      {#if status.kind === 'done'}
        <p class="mcp-ok">
          Download started.
          {#if status.warnings.length > 0}
            {' '}({status.warnings.length} warning{status.warnings.length === 1 ? '' : 's'})
          {/if}
        </p>
        {#if status.warnings.length > 0}
          <ul class="mcp-warn-list">
            {#each status.warnings as w (w)}<li>{w}</li>{/each}
          </ul>
        {/if}
      {:else if status.kind === 'error'}
        <p class="mcp-err">Failed: {status.message}</p>
      {/if}
    </div>
  {/if}
</div>

<style>
  .mcp-wrap {
    position: relative;
    display: inline-block;
  }
  .mcp-panel {
    margin-top: 8px;
    padding: 12px;
    border: 1px solid var(--hairline);
    border-radius: 6px;
    background: var(--ink-raised);
    min-width: 320px;
  }
  .mcp-row {
    display: flex;
    gap: 12px;
    align-items: flex-end;
  }
  .mcp-label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
    color: var(--static);
  }
  .mcp-label input {
    padding: 4px 6px;
    border: 1px solid var(--hairline);
    border-radius: 4px;
    background: var(--ink);
  }
  .mcp-note {
    margin: 10px 0 0;
    font-size: 12px;
    color: var(--static);
    line-height: 1.4;
  }
  .mcp-ok {
    margin: 8px 0 0;
    font-size: 12px;
    color: var(--ok);
  }
  .mcp-err {
    margin: 8px 0 0;
    font-size: 12px;
    color: var(--err);
  }
  .mcp-warn-list {
    margin: 4px 0 0 18px;
    padding: 0;
    font-size: 12px;
    color: var(--warn);
  }
</style>
