<script lang="ts">
  // Shown when a list fetch came back capped at its server-side
  // limit. The honest read is "you are looking at a window into a
  // larger set" — without this banner, capped data renders
  // indistinguishably from a complete result.

  type Props = Readonly<{
    showing: number;
    total: number;
    noun: string;
    hint?: string;
  }>;

  let { showing, total, noun, hint }: Props = $props();
</script>

<div class="overflow-banner" role="status">
  <span class="icon" aria-hidden="true">⋯</span>
  <span class="text">
    Showing <strong>{showing.toLocaleString()}</strong> of
    <strong>{total.toLocaleString()}</strong>
    {noun}.
    {#if hint}<span class="hint"> {hint}</span>{/if}
  </span>
</div>

<style>
  .overflow-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    background: var(--warn-wash);
    border: 1px solid var(--busy);
    border-radius: 6px;
    font-size: 13px;
    color: var(--warn);
    margin: 8px 0;
  }
  .icon { font-weight: 700; }
  .hint { color: var(--warn); }
  strong { font-weight: 600; }
</style>
