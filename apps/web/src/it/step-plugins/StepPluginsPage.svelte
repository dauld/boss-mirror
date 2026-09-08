<script lang="ts">
  // /admin/step-plugins — port of apps/web/src/admin/StepPluginsPage.tsx.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import type { StepPluginSpec } from './stepPluginTypes';
  import { href } from '../../router';

  let plugins = $state<ReadonlyArray<StepPluginSpec>>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function load(): Promise<void> {
    loading = true;
    try {
      const r = await fetch('/api/jobs/step-plugins');
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
      plugins = (await r.json()) as StepPluginSpec[];
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
    const m = new Map<string, StepPluginSpec[]>();
    for (const p of plugins) {
      const arr = m.get(p.category) ?? [];
      arr.push(p);
      m.set(p.category, arr);
    }
    for (const [, arr] of m) arr.sort((a, b) => a.kind.localeCompare(b.kind));
    return m;
  });
  let categoryKeys = $derived([...byCategory.keys()].sort());
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Platform · Step plugins"
    title="Step UX plugins"
    subtitle={loading
      ? 'Loading…'
      : `${plugins.length} active plugin${plugins.length === 1 ? '' : 's'} across ${categoryKeys.length} categor${categoryKeys.length === 1 ? 'y' : 'ies'}`}
  />
  {#if error}
    <p class="empty" style="color:#dc2626">Failed to load: {error}</p>
  {/if}

  {#if plugins.length === 0 && !loading && !error}
    <p class="empty" style="padding:0 24px">
      No plugins installed yet. See
      <code class="mono">infra/step-plugins/README.md</code> for the shape;
      seed one with <code class="mono">POST /api/jobs/step-plugins</code>.
    </p>
  {/if}

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
                <th>Frontend bundle</th>
              </tr>
            </thead>
            <tbody>
              {#each byCategory.get(cat) ?? [] as p (p.kind)}
                <tr>
                  <td>
                    <Link to={href(`/it/registry/step-plugins/${encodeURIComponent(p.kind)}`)}>
                      <span class="mono">{p.kind}</span>
                    </Link>
                  </td>
                  <td>{p.label}</td>
                  <td>
                    {#if p.owning_team === 'platform'}
                      <span style="color:#888; font-size:12px">system</span>
                    {:else}
                      <span class="mono">{p.owning_team}</span>
                    {/if}
                  </td>
                  <td class="num">{p.version}</td>
                  <td><code class="mono" style="font-size:11px">{p.frontend_url}</code></td>
                </tr>
              {/each}
            </tbody>
          </table>
      </Section>
    {/each}
  </div>
</div>
