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
    letter-spacing: var(--ls-label);
    text-transform: uppercase;
    color: var(--static);
    font-weight: 600;
    display: flex;
    justify-content: space-between;
    gap: var(--s3);
  }
  .head small { font-weight: 500; letter-spacing: var(--ls-label); color: var(--text-faint); }
  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .list { margin-top: var(--s2); display: grid; }
  .sig {
    display: grid; grid-template-columns: 68px 1fr; gap: var(--s3); align-items: baseline;
    font: inherit; font-size: 12.5px; color: inherit; text-align: left; padding: 4px 0;
    border: 0; border-top: 1px solid var(--hairline); background: transparent; cursor: pointer; border-radius: 0;
    min-width: 0;
  }
  .sig span { overflow-wrap: anywhere; }
  .sig:hover, .sig:focus-visible { background: var(--wash); }
  .sig time { color: var(--static); }
  .sig .who { color: var(--signal); }
  .sig.hand .who { color: var(--warn); }
  .sig.warn .who { color: var(--warn); }
  .sig.err .who { color: var(--err); }
  .empty { color: var(--text-faint); font-size: 12px; padding: 6px 0; }
</style>
