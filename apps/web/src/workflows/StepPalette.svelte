<!--
  Step palette (Slice 2). Click a StepType to append a step of that
  kind to the draft. The vocabulary is the StepType registry
  (/api/jobs/step-types) — the same data the list editor's <select>
  offers — surfaced as a browsable, filterable strip so authors can
  see what transitions exist instead of hunting a dropdown. The parent
  owns the fetch (one per surface) and passes the rows in.
-->
<script lang="ts">
  import type { StepTypeInfo } from './stepTypes';

  type Props = Readonly<{
    stepTypes: ReadonlyArray<StepTypeInfo>;
    onadd: (kind: string) => void;
  }>;
  let { stepTypes, onadd }: Props = $props();

  let filter = $state('');

  // Filter by kind/label/category, then group by category so related
  // transitions sit together. Derived — recomputes as the author types.
  let groups = $derived.by(() => {
    const q = filter.trim().toLowerCase();
    const matched = stepTypes.filter(
      (t) =>
        q.length === 0 ||
        t.kind.includes(q) ||
        t.label.toLowerCase().includes(q) ||
        t.category.toLowerCase().includes(q),
    );
    const byCategory = new Map<string, StepTypeInfo[]>();
    for (const t of matched) {
      const list = byCategory.get(t.category) ?? [];
      list.push(t);
      byCategory.set(t.category, list);
    }
    return [...byCategory.entries()]
      .map(([category, types]) => ({ category, types }))
      .sort((a, b) => a.category.localeCompare(b.category));
  });
</script>

<div class="jk-palette">
  <div class="jk-palette-head">
    <span class="jk-palette-title">Add a step</span>
    <input
      class="jk-palette-filter"
      type="text"
      bind:value={filter}
      placeholder="filter step types…"
    />
  </div>
  {#if stepTypes.length === 0}
    <span class="jk-palette-empty">Loading step types…</span>
  {:else if groups.length === 0}
    <span class="jk-palette-empty">No step types match “{filter}”.</span>
  {:else}
    <div class="jk-palette-groups">
      {#each groups as g (g.category)}
        <div class="jk-palette-group">
          <span class="jk-palette-cat">{g.category}</span>
          <div class="jk-palette-chips">
            {#each g.types as t (t.kind)}
              <button
                type="button"
                class="jk-chip"
                title={t.description}
                onclick={() => onadd(t.kind)}
              >
                <span class="jk-chip-plus">+</span>
                <span class="jk-chip-label">{t.label}</span>
                <span class="jk-chip-kind mono">{t.kind}</span>
              </button>
            {/each}
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .jk-palette {
    border: 1px solid var(--hairline);
    border-radius: 8px;
    background: var(--ink-raised);
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .jk-palette-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  .jk-palette-title {
    font-size: 12px;
    font-weight: 600;
    color: var(--static);
  }
  .jk-palette-filter {
    font-size: 12px;
    padding: 4px 8px;
    border: 1px solid var(--hairline);
    border-radius: 6px;
    width: 220px;
    max-width: 50%;
  }
  .jk-palette-empty {
    font-size: 12px;
    color: var(--static);
  }
  .jk-palette-groups {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .jk-palette-group {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .jk-palette-cat {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--static);
    font-weight: 600;
  }
  .jk-palette-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .jk-chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 1px solid var(--hairline);
    border-radius: 999px;
    background: var(--ink);
    padding: 3px 10px 3px 8px;
    cursor: pointer;
    font-size: 12px;
    line-height: 1.4;
  }
  .jk-chip:hover {
    border-color: var(--signal);
    background: var(--signal-wash);
  }
  .jk-chip-plus {
    color: var(--signal);
    font-weight: 700;
  }
  .jk-chip-kind {
    color: var(--static);
    font-size: 11px;
  }
</style>
