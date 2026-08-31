<script lang="ts">
  // /it/registry — port of apps/web/src/admin/WorkflowsPage.tsx.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import type { WorkflowSpec } from './workflowTypes';
  import { href } from '../router';

  let kinds = $state<ReadonlyArray<WorkflowSpec>>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function load(): Promise<void> {
    loading = true;
    try {
      const r = await fetch('/api/workflows');
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
      kinds = (await r.json()) as WorkflowSpec[];
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    void load();
  });

  let byCategory = $derived.by(() => {
    const m = new Map<string, WorkflowSpec[]>();
    for (const k of kinds) {
      const arr = m.get(k.category) ?? [];
      arr.push(k);
      m.set(k.category, arr);
    }
    for (const [, arr] of m) arr.sort((a, b) => a.kind.localeCompare(b.kind));
    return m;
  });
  let categoryKeys = $derived([...byCategory.keys()].sort());
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Platform · Job kinds"
    title="Job kinds"
    subtitle={loading
      ? 'Loading…'
      : `${kinds.length} active kinds across ${categoryKeys.length} categories`}
  />
  {#if error}
    <p class="empty" style="color:#dc2626">Failed to load: {error}</p>
  {/if}

  <div style="padding:0 24px 16px">
    <Link to={href('/it/registry/new')} className="wb-btn wb-btn-primary">
      + Create new kind
    </Link>
  </div>

  <div class="tab-grid">
    {#each categoryKeys as cat (cat)}
      <Section title={cat} wide>
          <table class="data-table data-table-striped">
            <thead>
              <tr>
                <th>Kind</th>
                <th>Label</th>
                <th>Owner</th>
                <th class="num">Version</th>
                <th class="num">Steps</th>
                <th>Subjects</th>
              </tr>
            </thead>
            <tbody>
              {#each byCategory.get(cat) ?? [] as k (k.kind)}
                <tr>
                  <td>
                    <Link to={href(`/it/registry/${encodeURIComponent(k.kind)}`)}>
                      <span class="mono">{k.kind}</span>
                    </Link>
                  </td>
                  <td>{k.label}</td>
                  <td>
                    {#if k.owning_team === 'platform'}
                      <span style="color:#888; font-size:12px">system</span>
                    {:else}
                      <span class="mono">{k.owning_team}</span>
                    {/if}
                  </td>
                  <td class="num">{k.version}</td>
                  <td class="num">{k.steps.length}</td>
                  <td>
                    {#each k.subject_kinds as s (s)}
                      <span class="chip chip-stage chip-stage-muted" style="margin-right:4px">{s}</span>
                    {/each}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
      </Section>
    {/each}
  </div>
</div>
