<script lang="ts">
  // The signals panel — what fired what, newest first, read off the
  // packets' own completed steps (yard-signals.ts). Time · who · what.
  // The actor's name wears the signal colour; a step the record says
  // was completed BY HAND wears the warn colour and says so; a red
  // verdict or a refusal wears the error colour. Each row opens the
  // packet it came from.
  import { clockText } from './yard-status';
  import type { Signal } from './yard-signals';

  type Props = Readonly<{
    signals: readonly Signal[];
    onopen: (packetId: string) => void;
  }>;
  let { signals, onopen }: Props = $props();

  /** The instant as `HH:MM` (the header says UTC once), or the raw
   *  stamp when it is not one. */
  const at = (s: string): string => clockText(s)?.replace(' UTC', '') ?? s;
</script>

<h2 class="head">
  Signals · what fired what
  <small>{signals.length} newest · UTC</small>
</h2>
<div class="list">
  {#each signals as s (s.id)}
    <button type="button" class="sig {s.sev}" class:hand={s.hand} onclick={() => onopen(s.packetId)} title="open the packet this signal came from">
      <time class="mono">{at(s.at)}</time>
      <span>
        <span class="who">{s.who}{#if s.hand} · by hand{/if}</span> — {s.what}
      </span>
    </button>
  {/each}
  {#if signals.length === 0}
    <span class="empty">quiet — no stamped step in the window</span>
  {/if}
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
  .head small { font-weight: 500; letter-spacing: var(--ls-label, 0.1em); color: var(--text-faint, #5c656e); }
  .mono { font-family: var(--font-mono, ui-monospace, monospace); font-variant-numeric: tabular-nums; }
  .list { margin-top: var(--s2, 8px); display: grid; }
  .sig {
    display: grid; grid-template-columns: 68px 1fr; gap: var(--s3, 12px); align-items: baseline;
    font: inherit; font-size: 12.5px; color: inherit; text-align: left; padding: 4px 0;
    border: 0; border-top: 1px solid var(--hairline, #2a3138); background: transparent; cursor: pointer; border-radius: 0;
    min-width: 0;
  }
  .sig span { overflow-wrap: anywhere; }
  .sig:hover, .sig:focus-visible { background: var(--wash, rgba(232, 236, 239, 0.04)); }
  .sig time { color: var(--static, #7a838c); }
  .sig .who { color: var(--signal, #5fd4a8); }
  .sig.hand .who { color: var(--warn, #d9a441); }
  .sig.warn .who { color: var(--warn, #d9a441); }
  .sig.err .who { color: var(--err, #e2685c); }
  .empty { color: var(--text-faint, #5c656e); font-size: 12px; padding: 6px 0; }
</style>
