<script lang="ts">
  // /it/registry/rules — list the ACTIVE dispatcher rules, each linking
  // to its editor. Authoring sibling of the /it/registry/dispatcher cascade viz.
  // Models the step-plugins list page (PageHeader/Section + data-table +
  // load/error state). Writes flow through ./ruleAuthoring.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import { listActiveRules, type DispatcherRule } from './ruleAuthoring';
  import { href } from '../router';

  let rules = $state<ReadonlyArray<DispatcherRule>>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function load(): Promise<void> {
    loading = true;
    try {
      rules = await listActiveRules();
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

  let sorted = $derived([...rules].sort((a, b) => a.name.localeCompare(b.name)));
</script>

<div class="catalog theme-exec">
  <Breadcrumb to={href('/it/registry/dispatcher')}>← Dispatcher cascade</Breadcrumb>
  <PageHeader
    eyebrow="Platform · Dispatcher rules"
    title="Dispatcher rules"
    subtitle={loading
      ? 'Loading…'
      : `${rules.length} active rule${rules.length === 1 ? '' : 's'} — the side-effect wiring boss-dispatcher runs`}
  />

  <div style="padding:0 24px 16px; display:flex; gap:12px; align-items:center">
    <Link to={href('/it/registry/rules/new')} className="wb-btn wb-btn-primary">
      + New rule
    </Link>
  </div>

  {#if error}
    <p class="empty" style="color:#dc2626; padding:0 24px">Failed to load: {error}</p>
  {/if}

  {#if rules.length === 0 && !loading && !error}
    <p class="empty" style="padding:0 24px">
      No active dispatcher rules. Create one with
      <Link to={href('/it/registry/rules/new')}>+ New rule</Link>.
    </p>
  {/if}

  {#if rules.length > 0}
    <div class="tab-grid">
      <Section title="Active rules" wide>
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>Rule</th>
              <th>On event</th>
              <th class="num">Do steps</th>
              <th class="num">Version</th>
            </tr>
          </thead>
          <tbody>
            {#each sorted as r (r.name)}
              <tr>
                <td>
                  <Link to={href(`/it/registry/rules/${encodeURIComponent(r.name)}`)}>
                    <span class="mono">{r.name}</span>
                  </Link>
                </td>
                <td><code class="mono" style="font-size:12px">{r.on_event}</code></td>
                <td class="num">{r.do.length}</td>
                <td class="num">{r.version}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </Section>
    </div>
  {/if}
</div>
