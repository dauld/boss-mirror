<script lang="ts">
  // The departure board: one row per wagon on the floor — where it is
  // right now, what it is doing, since when. In flight first, in the
  // order a change travels along the line; landed below, dimmed. The
  // rows are the scene's `boardRows`, already ordered (yard-floor.ts),
  // so this table and the map can never disagree about a car's place.
  // Clicking a row selects the car, the same selection the map makes.
  import { sinceText, type Scene } from './yard-floor';

  type Props = Readonly<{
    scene: Scene;
    selected: string;
    onselect: (key: string) => void;
    nowMs: number;
  }>;
  let { scene, selected, onselect, nowMs }: Props = $props();

  const wagonById = $derived(new Map(scene.wagons.map(w => [w.id, w])));
  const inFlight = $derived(scene.boardRows.filter(r => !r.landed));
  const landed = $derived(scene.boardRows.filter(r => r.landed));

  const pickKey = (key: string) => (e: KeyboardEvent) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      onselect(key);
    }
  };
</script>

{#snippet row(r: Scene['boardRows'][number])}
  {@const w = wagonById.get(r.id)}
  {#if w}
    <tr
      class="row"
      class:selected={selected === `car:${w.id}`}
      class:landed={r.landed}
      tabindex="0"
      onclick={() => onselect(`car:${w.id}`)}
      onkeydown={pickKey(`car:${w.id}`)}>
      <td class="car">
        {w.title}
        <small class="mono">{w.branch}@{w.head ?? '—'} · {w.id.slice(0, 8)}</small>
      </td>
      <td class="where">{r.where}</td>
      <td>
        <span class="status">
          <span class="lamp {w.lamp}"></span>
          <span>{w.status}</span>
        </span>
      </td>
      <td class="since mono">{sinceText(w.since, nowMs)}</td>
    </tr>
  {/if}
{/snippet}

<h2 class="head">
  Departure board
  <small>{inFlight.length} in flight · {landed.length} landed</small>
</h2>
<div class="scroll">
  <table class="board">
    <thead>
      <tr><th>Car</th><th>Where</th><th>Status</th><th>Since</th></tr>
    </thead>
    <tbody>
      {#each inFlight as r (r.id)}{@render row(r)}{/each}
      {#if inFlight.length === 0}
        <tr><td colspan="4" class="empty">nothing in flight</td></tr>
      {/if}
      {#if landed.length > 0}
        <tr class="sep"><td colspan="4" class="label">Landed</td></tr>
        {#each landed as r (r.id)}{@render row(r)}{/each}
      {/if}
    </tbody>
  </table>
</div>

<style>
  .head {
    margin: 0;
    font-size: 11px;
    letter-spacing: var(--ls-label, 0.1em);
    text-transform: uppercase;
    color: var(--static, #7a838c);
    font-weight: 600;
    display: flex;
    justify-content: space-between;
    gap: var(--s3, 12px);
  }
  .head small {
    font-weight: 500;
    letter-spacing: var(--ls-label, 0.1em);
    color: var(--text-faint, #5c656e);
  }
  .scroll { overflow-x: auto; }
  .board { width: 100%; border-collapse: collapse; margin-top: var(--s3, 12px); font-size: 12.5px; }
  .board th {
    text-align: left;
    font-size: 10.5px;
    letter-spacing: var(--ls-label, 0.1em);
    text-transform: uppercase;
    color: var(--text-faint, #5c656e);
    font-weight: 600;
    padding: 4px 8px 6px 0;
    border-bottom: 1px solid var(--hairline, #2a3138);
  }
  .board td { padding: 6px 8px 6px 0; border-bottom: 1px solid var(--hairline, #2a3138); vertical-align: top; }
  .row { cursor: pointer; outline: none; }
  .row:hover td, .row:focus-visible td { background: var(--wash, rgba(232, 236, 239, 0.04)); }
  .row.selected td { background: color-mix(in srgb, var(--signal, #5fd4a8) 10%, transparent); }
  .row.landed td { color: var(--static, #7a838c); }
  .car { font-weight: 500; }
  .car small { display: block; color: var(--static, #7a838c); font-weight: 400; overflow-wrap: anywhere; }
  .mono { font-family: var(--font-mono, ui-monospace, monospace); font-variant-numeric: tabular-nums; }
  .where { white-space: nowrap; }
  .status { display: flex; gap: 6px; align-items: baseline; }
  .status .lamp { position: relative; top: -1px; }
  .since { color: var(--static, #7a838c); white-space: nowrap; }
  .sep td { padding: 8px 0 2px; border: 0; }
  .label {
    font-size: 11px;
    letter-spacing: var(--ls-label, 0.1em);
    text-transform: uppercase;
    color: var(--static, #7a838c);
    font-weight: 600;
  }
  .empty { color: var(--text-faint, #5c656e); font-size: 12px; }
  .lamp {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--static, #7a838c);
    flex: none;
  }
  .lamp.ok { background: var(--ok, #4fb98a); box-shadow: 0 0 6px var(--ok, #4fb98a); }
  .lamp.working { background: var(--signal, #5fd4a8); box-shadow: 0 0 6px var(--signal, #5fd4a8); animation: pulse 1.6s ease-in-out infinite; }
  .lamp.warn { background: var(--warn, #d9a441); box-shadow: 0 0 6px var(--warn, #d9a441); }
  .lamp.err { background: var(--err, #e2685c); box-shadow: 0 0 7px var(--err, #e2685c); animation: blink 1s steps(2) infinite; }
  .lamp.off { background: var(--border-strong, #3a434d); }
  @keyframes pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.45; } }
  @keyframes blink { 50% { opacity: 0.25; } }
  @media (prefers-reduced-motion: reduce) { .lamp { animation: none !important; } }
</style>
