<script lang="ts">
  // /it/subjects — Subjects & Classes: the model's vocabulary, read-only.
  // Left: the SubjectKind taxonomy (boss-subject-kinds). Right: the
  // selected kind's Class registry (boss-classes), grouped by the Subject
  // attribute each class set keys (role / department / type / …). Authoring
  // is deliberately out of scope for v1 — this is the "what vocabulary does
  // the running model speak?" surface that pairs with /it/dispatcher and
  // /it/monitoring.

  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import {
    listSubjectKinds,
    listClasses,
    buildKindTree,
    groupClassesByAttribute,
    type SubjectKind,
    type ClassRow,
    type KindTreeNode,
  } from './subjects';

  let tree = $state<ReadonlyArray<KindTreeNode>>([]);
  let kindsByCode = $state<Map<string, SubjectKind>>(new Map());
  let selected = $state<string | null>(null);
  let classes = $state<ReadonlyArray<ClassRow>>([]);
  let loadingKinds = $state(true);
  let loadingClasses = $state(false);
  let error = $state<string | null>(null);

  async function loadKinds(): Promise<void> {
    loadingKinds = true;
    try {
      const all = await listSubjectKinds();
      tree = buildKindTree(all);
      kindsByCode = new Map(all.map((k) => [k.kind, k]));
      error = null;
      const first = tree[0];
      if (first) await select(first.kind.kind);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loadingKinds = false;
    }
  }

  async function select(kind: string): Promise<void> {
    selected = kind;
    loadingClasses = true;
    try {
      classes = await listClasses(kind);
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      classes = [];
    } finally {
      loadingClasses = false;
    }
  }

  onMount(() => {
    void loadKinds();
  });

  let selectedKind = $derived(selected ? (kindsByCode.get(selected) ?? null) : null);
  let grouped = $derived(groupClassesByAttribute(classes));
  let kindCount = $derived(kindsByCode.size);

  function fmtVal(v: unknown): string {
    if (v === null || v === undefined) return '';
    if (typeof v === 'string' || typeof v === 'number' || typeof v === 'boolean') return String(v);
    return JSON.stringify(v);
  }
</script>

{#snippet metaCell(meta: Readonly<Record<string, unknown>>)}
  {@const entries = Object.entries(meta)}
  {#if entries.length === 0}
    <span class="sc-dim">—</span>
  {:else}
    <span class="sc-chips">
      {#each entries as [k, v] (k)}
        <span class="sc-chip"><span class="sc-chip-k">{k}</span>{fmtVal(v)}</span>
      {/each}
    </span>
  {/if}
{/snippet}

<div class="subjects theme-exec">
  <PageHeader
    eyebrow="Platform · Model vocabulary"
    title="Subjects & Classes"
    subtitle={loadingKinds
      ? 'Loading…'
      : `${kindCount} subject kind${kindCount === 1 ? '' : 's'} · the Class registry`}
  />

  {#if error}
    <p class="empty" style="color:var(--err); padding:0 24px">Failed to load: {error}</p>
  {/if}

  <div class="sc-body">
    <aside class="sc-tree">
      <div class="sc-tree-head">Subject kinds</div>
      {#each tree as node (node.kind.kind)}
        <button
          class="sc-kind sc-root"
          class:active={selected === node.kind.kind}
          onclick={() => select(node.kind.kind)}
        >
          <span class="sc-kind-label">{node.kind.label}</span>
          <span class="sc-kind-code mono">{node.kind.kind}</span>
        </button>
        {#each node.children as child (child.kind)}
          <button
            class="sc-kind sc-child"
            class:active={selected === child.kind}
            onclick={() => select(child.kind)}
          >
            <span class="sc-kind-label">{child.label}</span>
            <span class="sc-kind-code mono">{child.kind}</span>
          </button>
        {/each}
      {/each}
    </aside>

    <div class="sc-detail">
      {#if selectedKind}
        <div class="sc-detail-head">
          <h2>
            {selectedKind.label}
            <span class="sc-detail-code mono">{selectedKind.kind}</span>
          </h2>
          {#if selectedKind.description}
            <p class="sc-detail-desc">{selectedKind.description}</p>
          {/if}
          <div class="sc-meta">
            {#if selectedKind.parent_kind}
              <span>parent <span class="mono">{selectedKind.parent_kind}</span></span>
            {/if}
            <span>owner {selectedKind.owning_team}</span>
          </div>
        </div>

        {#if loadingClasses}
          <p class="empty">Loading classes…</p>
        {:else if grouped.length === 0}
          <p class="empty">
            No classes registered for <span class="mono">{selectedKind.kind}</span>. Classes are
            tenant reference data — see
            <code class="mono">docs/design/class-registry.md</code>.
          </p>
        {:else}
          {#each grouped as [attr, rows] (attr)}
            <Section title={`${attr} · ${rows.length}`} wide>
              <table class="data-table data-table-striped">
                <thead>
                  <tr>
                    <th>Code</th>
                    <th>Display name</th>
                    <th>Parent</th>
                    <th>Metadata</th>
                  </tr>
                </thead>
                <tbody>
                  {#each rows as c (c.code)}
                    <tr>
                      <td><span class="mono">{c.code}</span></td>
                      <td>{c.display_name}</td>
                      <td>
                        {#if c.parent_code}
                          <span class="mono">{c.parent_code}</span>
                        {:else}
                          <span class="sc-dim">—</span>
                        {/if}
                      </td>
                      <td>{@render metaCell(c.metadata)}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </Section>
          {/each}
        {/if}
      {:else if !loadingKinds}
        <p class="empty">Select a subject kind to see its classes.</p>
      {/if}
    </div>
  </div>
</div>

<style>
  .subjects {
    padding-bottom: 40px;
  }
  .sc-body {
    display: grid;
    grid-template-columns: 260px 1fr;
    gap: 24px;
    padding: 0 24px;
    align-items: start;
  }
  .sc-tree {
    position: sticky;
    top: 16px;
    border: 1px solid var(--hairline);
    border-radius: 8px;
    overflow: hidden;
    background: var(--ink);
  }
  .sc-tree-head {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--static);
    padding: 10px 12px;
    background: var(--ink-raised);
    border-bottom: 1px solid var(--hairline);
  }
  .sc-kind {
    display: flex;
    flex-direction: column;
    gap: 1px;
    width: 100%;
    text-align: left;
    padding: 8px 12px;
    background: none;
    border: none;
    border-bottom: 1px solid var(--hairline);
    cursor: pointer;
  }
  .sc-kind:hover {
    background: var(--ink-raised);
  }
  .sc-kind.active {
    background: var(--signal-wash);
    box-shadow: inset 3px 0 0 var(--signal);
  }
  .sc-root .sc-kind-label {
    font-weight: 600;
  }
  .sc-child {
    padding-left: 24px;
  }
  .sc-kind-label {
    font-size: 13px;
    color: var(--fog);
  }
  .sc-kind-code {
    font-size: 11px;
    color: var(--static);
  }
  .sc-detail-head {
    padding: 4px 0 12px;
  }
  .sc-detail-head h2 {
    margin: 0;
    font-size: 18px;
  }
  .sc-detail-code {
    font-size: 13px;
    color: var(--static);
    font-weight: 400;
  }
  .sc-detail-desc {
    margin: 6px 0 0;
    color: var(--static);
    font-size: 13px;
    max-width: 60ch;
  }
  .sc-meta {
    display: flex;
    gap: 16px;
    margin-top: 6px;
    font-size: 12px;
    color: var(--static);
  }
  .sc-dim {
    color: var(--text-faint);
  }
  .sc-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .sc-chip {
    font-size: 11px;
    background: var(--ink-raised);
    border-radius: 4px;
    padding: 1px 6px;
    color: var(--static);
  }
  .sc-chip-k {
    color: var(--static);
    margin-right: 4px;
  }
</style>
