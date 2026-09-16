<script lang="ts">
  // The Surfaces section of /it/codebase (backlog 628f182b): the last
  // seven days of surface opens, per actor — opens per route sorted,
  // distinct routes — and the catalogued surfaces nobody opened.
  //
  // David, 2026-09-16: "let's measure which surfaces I open for a
  // week." The measurement's mechanism is documented once, at the top
  // of apps/web/src/shell/surface-opens.ts (the record) and
  // infra/surface-usage.sh (the daily packet); this section reads the
  // same roll-up the chore files and renders it beside the codebase
  // numbers, because both are the department's own measurements of
  // itself. No chart: counts per route per actor are the whole product.
  import { onMount } from 'svelte';
  import type { Remote } from '../../data/remote';
  import { catalogPaths, loadSurfaceRollup, neverOpened, perActor, type Rollup } from './surfaces';

  /** The reading's window. Seven days: the week David asked for. */
  const DAYS = 7;

  let rollup = $state<Remote<Rollup>>({ kind: 'loading' });
  onMount(() => {
    void loadSurfaceRollup(DAYS).then((r) => {
      rollup = r;
    });
  });

  const ready = $derived(rollup.kind === 'ready' ? rollup.data : null);
  const actors = $derived(ready ? perActor(ready.rows) : []);
  const roster = catalogPaths();
  const unopened = $derived(ready ? neverOpened(ready.rows, roster) : []);
  const opens = $derived(actors.reduce((s, a) => s + a.opens, 0));
  const when = (iso: string): string => (iso ? `${iso.slice(0, 10)} ${iso.slice(11, 16)}Z` : '—');
  const n = (v: number): string => v.toLocaleString('en-US');
</script>

<div class="ct-section">05 — SURFACES · which pages each operator opened, last {DAYS} days</div>
{#if rollup.kind === 'failed'}
  <p class="ct-fail load-failed">
    The surface roll-up did not answer: {rollup.error}. An unreachable read is not a quiet week, so nothing is
    listed.
  </p>
{:else if rollup.kind === 'loading'}
  <p class="ct-quiet">Reading the surface opens…</p>
{:else if actors.length === 0}
  <p class="ct-quiet">
    No surface open is recorded in the last {DAYS} days. The record starts with the first session after this
    landed; until a week has passed, "never opened" below is a window that has not filled, not a verdict.
  </p>
  <div class="ct-panel">
    <div class="h">Catalogued surfaces · {roster.length}</div>
    <ul class="su-list mono">
      {#each unopened as p (p)}<li>{p}</li>{/each}
    </ul>
  </div>
{:else}
  <div class="ct-strip">
    <div>
      <div class="k">opens · {DAYS} days</div>
      <div class="v">{n(opens)}</div>
    </div>
    <div>
      <div class="k">actors</div>
      <div class="v">{actors.length}</div>
    </div>
    <div>
      <div class="k">catalogued surfaces never opened</div>
      <div class="v">{unopened.length}<small>of {roster.length}</small></div>
    </div>
  </div>
  <div class="su-actors">
    {#each actors as a (a.actor_id)}
      <div class="ct-panel">
        <div class="h">
          <span class="mono">{a.actor_id}</span> · {n(a.opens)} open{a.opens === 1 ? '' : 's'} across {a.distinct}
          route{a.distinct === 1 ? '' : 's'}
        </div>
        <div class="ct-tbl">
          <table class="ct-table">
            <thead>
              <tr><th>route</th><th class="num">opens</th><th>last</th></tr>
            </thead>
            <tbody>
              {#each a.routes as r (r.route)}
                <tr>
                  <td class="mono">{r.route}</td>
                  <td class="num mono">{n(r.opens)}</td>
                  <td class="mono dim">{when(r.last_at)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      </div>
    {/each}
  </div>
  <div class="ct-panel">
    <div class="h">Never opened in {DAYS} days · {unopened.length} of {roster.length} catalogued surfaces — the deletion candidates</div>
    {#if unopened.length === 0}
      <p class="ct-quiet">Every catalogued surface was opened at least once.</p>
    {:else}
      <ul class="su-list mono">
        {#each unopened as p (p)}<li>{p}</li>{/each}
      </ul>
    {/if}
  </div>
  <p class="ct-footnote">
    Read from <span class="mono">/api/surface-opens/rollup</span>
    {#if ready?.since}({ready.since.slice(0, 10)} to {ready?.until?.slice(0, 10) ?? '…'}){/if}. A route is a
    pattern — <span class="mono">/ux/jobs/:jobId</span>, never the id — and the actor is the session's, signed by
    the gateway. Agents do not run the SPA, so nothing of theirs is counted. The daily
    <span class="mono">maintenance-surface-usage</span> packet files the same roll-up over 24 hours.
  </p>
{/if}

<style>
  /* Svelte scopes a component's styles to itself, so the page's ct-*
     shapes (heading, strip, panel, table) are restated here with the
     same tokens rather than reached for across the boundary. */
  .ct-section {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px; letter-spacing: var(--ls-eyebrow, 0.3em);
    color: var(--signal, #5fd4a8); margin: 28px 0 8px;
    display: flex; align-items: center; gap: 12px;
  }
  .ct-section::after { content: ''; flex: 1; border-top: 1px solid var(--hairline, #2a3138); }
  .ct-quiet { color: var(--static, #7a838c); font-size: 13px; }
  .ct-fail {
    color: var(--warn, #d9a441); border: 1px solid var(--warn, #d9a441);
    padding: 8px 12px; font-size: 13px;
  }
  .mono { font-family: var(--font-mono, ui-monospace, monospace); font-variant-numeric: tabular-nums; }
  .dim { color: var(--static, #7a838c); }
  .ct-strip {
    display: grid; grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    border: 1px solid var(--hairline, #2a3138); margin-bottom: 10px;
  }
  .ct-strip > div { padding: 12px 16px; border-right: 1px solid var(--hairline, #2a3138); min-width: 0; }
  .ct-strip > div:last-child { border-right: 0; }
  .ct-strip .k {
    font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: 0.1em; text-transform: uppercase; color: var(--static, #7a838c);
  }
  .ct-strip .v { font-size: 28px; font-weight: 500; font-variant-numeric: tabular-nums; line-height: 1.1; margin-top: 4px; }
  .ct-strip .v small { font-size: 12px; color: var(--static, #7a838c); font-weight: 400; margin-left: 5px; }
  .ct-panel { border: 1px solid var(--hairline, #2a3138); padding: 14px 16px; min-width: 0; }
  .ct-panel .h { font-size: 12px; font-weight: 600; margin-bottom: 8px; }
  .su-actors { display: grid; gap: 10px; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); margin-bottom: 10px; }
  .ct-tbl { overflow-x: auto; }
  .ct-table { width: 100%; border-collapse: collapse; font-size: 12px; }
  .ct-table th { text-align: left; font-weight: 500; color: var(--static, #7a838c); padding: 4px 8px; border-bottom: 1px solid var(--hairline, #2a3138); }
  .ct-table td { padding: 4px 8px; border-bottom: 1px solid var(--hairline, #2a3138); vertical-align: top; white-space: nowrap; }
  .ct-table .num { text-align: right; }
  .su-list { margin: 0; padding-left: 16px; font-size: 12px; columns: 3; column-gap: 24px; }
  .ct-footnote { color: var(--text-faint, #5c656e); font-size: 12px; max-width: 90ch; margin-top: 20px; }
</style>
