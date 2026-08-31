<script lang="ts">
  // /admin/step-plugins/:slug — port of
  // apps/web/src/admin/StepPluginDetailPage.tsx.

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import { formatDate } from '@boss/web-kit/ui/date';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import type { StepPluginSpec } from './stepPluginTypes';
  import type { WorkflowStatus } from '../../workflows/workflowTypes';
  import { href } from '../../router';

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready'; spec: StepPluginSpec; versions: ReadonlyArray<StepPluginSpec> };

  type Props = { pluginSlug: string };
  let { pluginSlug }: Props = $props();

  let loadState: LoadState = $state<LoadState>({ kind: 'loading' });
  let action = $state<string | null>(null);
  let actionError = $state<string | null>(null);

  async function load(): Promise<void> {
    try {
      const [specResp, versionsResp] = await Promise.all([
        fetch(`/api/jobs/step-plugins/${encodeURIComponent(pluginSlug)}`),
        fetch(`/api/jobs/step-plugins/${encodeURIComponent(pluginSlug)}/versions`),
      ]);
      const versions = versionsResp.ok
        ? ((await versionsResp.json()) as StepPluginSpec[])
        : [];
      let spec: StepPluginSpec | null = null;
      if (specResp.ok) {
        spec = (await specResp.json()) as StepPluginSpec;
      } else if (versions.length > 0) {
        spec = versions[versions.length - 1]!;
      } else {
        throw new Error(`HTTP ${specResp.status}: no versions available`);
      }
      loadState = { kind: 'ready', spec, versions };
    } catch (e) {
      loadState = { kind: 'error', message: e instanceof Error ? e.message : String(e) };
    }
  }

  $effect(() => {
    void pluginSlug;
    void load();
  });

  async function runAction(verb: 'publish' | 'retire'): Promise<void> {
    action = verb;
    actionError = null;
    try {
      if (verb === 'retire') {
        let inFlight = 0;
        try {
          const r = await fetch(
            `/api/jobs/step-plugins/${encodeURIComponent(pluginSlug)}/in-flight-count`,
          );
          if (r.ok) {
            const json = (await r.json()) as { in_flight?: number };
            inFlight = json.in_flight ?? 0;
          }
        } catch {
          // best-effort
        }
        const msg =
          `Retire plugin "${pluginSlug}"?\n\n` +
          `${inFlight} in-flight Step${inFlight === 1 ? '' : 's'} will keep ` +
          `rendering the current bundle. No new Steps of this kind can be created.`;
        if (!window.confirm(msg)) {
          action = null;
          return;
        }
      }
      const r = await fetch(
        `/api/jobs/step-plugins/${encodeURIComponent(pluginSlug)}/${verb}`,
        { method: 'POST' },
      );
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
      await load();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      action = null;
    }
  }

  function statusChipClass(status: WorkflowStatus): string {
    return status === 'active' ? 'ok' : status === 'retired' ? 'muted' : 'warn';
  }
</script>

{#if loadState.kind === 'loading'}
  <div class="catalog theme-exec">
    <p class="empty">Loading…</p>
  </div>
{:else if loadState.kind === 'error'}
  <div class="catalog theme-exec">
    <PageHeader eyebrow="Platform · Step plugin" title={pluginSlug} subtitle={loadState.message} />
  </div>
{:else}
  {@const spec = loadState.spec}
  {@const versions = loadState.versions}
  {@const hasDraft = versions.some((v) => v.status === 'draft')}
  <div class="catalog theme-exec">
    <Breadcrumb to={href('/it/registry/step-plugins')}>
      ← All step plugins
    </Breadcrumb>
    <PageHeader
      eyebrow={`Platform · Step plugin · ${spec.category}`}
      title={spec.label}
      subtitle={`${spec.kind} · v${spec.version} · ${spec.status} · owned by ${spec.owning_team}`}
    />

    <div style="padding:0 24px 16px; display:flex; gap:12px; align-items:center">
      <button
        type="button"
        class="wb-btn"
        onclick={() => runAction('publish')}
        disabled={!hasDraft || action !== null}
        title={hasDraft ? 'Promote the latest draft to active' : 'No draft to publish'}
      >
        {action === 'publish' ? 'Publishing…' : 'Publish draft'}
      </button>
      <button
        type="button"
        class="wb-btn"
        onclick={() => runAction('retire')}
        disabled={spec.status !== 'active' || action !== null}
        title="Flip the active row to retired — in-flight Jobs unaffected"
      >
        {action === 'retire' ? 'Retiring…' : 'Retire'}
      </button>
      {#if actionError}
        <span style="color:#dc2626; font-size:13px">{actionError}</span>
      {/if}
    </div>

    <div class="tab-grid">
      <Section title="Spec">
          <table class="data-table">
            <tbody>
              <tr>
                <td style="color:#888; width:160px">Kind</td>
                <td><span class="mono">{spec.kind}</span></td>
              </tr>
              <tr><td style="color:#888">Label</td><td>{spec.label}</td></tr>
              <tr><td style="color:#888">Category</td><td>{spec.category}</td></tr>
              <tr>
                <td style="color:#888">Status</td>
                <td>
                  <span class="chip chip-stage chip-stage-{statusChipClass(spec.status)}">
                    {spec.status}
                  </span>
                </td>
              </tr>
              <tr><td style="color:#888">Version</td><td>{spec.version}</td></tr>
              <tr>
                <td style="color:#888">Frontend bundle</td>
                <td><code class="mono" style="font-size:12px">{spec.frontend_url}</code></td>
              </tr>
              <tr><td style="color:#888">Owner</td><td>{spec.owning_team}</td></tr>
              <tr>
                <td style="color:#888">Authoring Job</td>
                <td>
                  {#if spec.authoring_job_id}
                    <EntityLink kind="job" id={spec.authoring_job_id} />
                  {:else}
                    <span style="color:#888">—</span>
                  {/if}
                </td>
              </tr>
              <tr>
                <td style="color:#888">Created</td>
                <td>{formatDate(spec.created_at)}</td>
              </tr>
              {#if spec.description}
                <tr><td style="color:#888">Description</td><td>{spec.description}</td></tr>
              {/if}
            </tbody>
          </table>
      </Section>

      <Section title="Metadata schema">
          <pre
            class="mono"
            style="font-size:12px; padding:8px; background:#f5f5f4; border-radius:4px; overflow:auto; max-height:240px"
          >{JSON.stringify(spec.metadata_schema, null, 2)}</pre>
      </Section>

      <Section title={`Version history (${versions.length})`}>
          <table class="data-table data-table-striped">
            <thead>
              <tr>
                <th class="num">Version</th>
                <th>Status</th>
                <th>Owner</th>
                <th>Created</th>
              </tr>
            </thead>
            <tbody>
              {#each versions as v (v.version)}
                <tr>
                  <td class="num">{v.version}</td>
                  <td>
                    <span class="chip chip-stage chip-stage-{statusChipClass(v.status)}">
                      {v.status}
                    </span>
                  </td>
                  <td>{v.owning_team}</td>
                  <td>{new Date(v.created_at).toISOString().slice(0, 19).replace('T', ' ')}</td>
                </tr>
              {/each}
            </tbody>
          </table>
      </Section>
    </div>
  </div>
{/if}
