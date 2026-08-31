<script lang="ts">
  // /it/registry/:slug — port of
  // apps/web/src/admin/WorkflowDetailPage.tsx.

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import { formatDate } from '@boss/web-kit/ui/date';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import type { WorkflowSpec, StepSpec } from './workflowTypes';
  import { href, navigate } from '../router';
  import StepDag from '../jobs/StepDag.svelte';
  import { workflowToDag } from '../jobs/workflowToDag';
  import { startDesignJob } from './designJob';
  import { session } from '@boss/web-kit/session/session.svelte';
  import { appToday } from '@boss/web-kit/sim-clock';

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready'; spec: WorkflowSpec; versions: ReadonlyArray<WorkflowSpec> };

  type Props = { kindSlug: string };
  let { kindSlug }: Props = $props();

  let loadState: LoadState = $state<LoadState>({ kind: 'loading' });
  let action = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let compareVersion = $state<number | null>(null);
  let ownerId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : '',
  );

  // Edit / new version (D6): author the next version *through* a fresh
  // `workflow-design` Job seeded from the active spec — never a direct
  // registry write. The active row + in-flight Jobs are untouched until
  // the new design Job reaches its publish step.
  async function editNewVersion(): Promise<void> {
    if (loadState.kind !== 'ready') return;
    const spec = loadState.spec;
    action = 'edit';
    actionError = null;
    try {
      const jobId = await startDesignJob(
        { ...spec, status: 'draft' },
        ownerId,
        appToday(),
        { title: `Edit ${spec.kind}`, previousVersion: spec.version },
      );
      navigate(href(`/it/registry/authoring/${encodeURIComponent(jobId)}`));
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
      action = null;
    }
  }

  async function load(): Promise<void> {
    try {
      const [specResp, versionsResp] = await Promise.all([
        fetch(`/api/workflows/${encodeURIComponent(kindSlug)}`),
        fetch(`/api/workflows/${encodeURIComponent(kindSlug)}/versions`),
      ]);
      const versions = versionsResp.ok
        ? ((await versionsResp.json()) as WorkflowSpec[])
        : [];
      let spec: WorkflowSpec | null = null;
      if (specResp.ok) {
        spec = (await specResp.json()) as WorkflowSpec;
      } else if (versions.length > 0) {
        spec = versions[versions.length - 1]!;
      } else {
        throw new Error(`HTTP ${specResp.status}: no versions available`);
      }
      loadState = { kind: 'ready', spec, versions };
    } catch (e) {
      loadState = {
        kind: 'error',
        message: e instanceof Error ? e.message : String(e),
      };
    }
  }

  $effect(() => {
    void kindSlug;
    void load();
  });

  async function runAction(verb: 'retire'): Promise<void> {
    action = verb;
    actionError = null;
    try {
      if (verb === 'retire') {
        let inFlight = 0;
        try {
          const r = await fetch(
            `/api/jobs?kind=${encodeURIComponent(kindSlug)}&status=Open&limit=1`,
          );
          if (r.ok) {
            const json = (await r.json()) as { total?: number };
            inFlight = json.total ?? 0;
          }
        } catch {
          // best-effort
        }
        const msg =
          `Retire "${kindSlug}"?\n\n` +
          `${inFlight} in-flight Job${inFlight === 1 ? '' : 's'} will continue to run. ` +
          `No new Jobs of this kind can be created.`;
        if (!window.confirm(msg)) {
          action = null;
          return;
        }
      }
      const r = await fetch(
        `/api/workflows/${encodeURIComponent(kindSlug)}/${verb}`,
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

  function statusChipClass(status: WorkflowSpec['status']): string {
    return status === 'active' ? 'ok' : status === 'retired' ? 'muted' : 'warn';
  }

  // ------- Diff helpers -------
  // v2 steps are a flat list keyed by their stable `title` slug. The
  // diff matches steps across two versions by slug: a slug present on
  // only one side is added/removed; a slug on both whose fields
  // differ is changed.
  type DiffMark = 'unchanged' | 'changed' | 'added' | 'removed';

  function stepFieldsEqual(a: StepSpec, b: StepSpec): boolean {
    return (
      a.kind === b.kind &&
      a.ready_when === b.ready_when &&
      a.title_template === b.title_template &&
      JSON.stringify(a.sign_offs_required ?? []) === JSON.stringify(b.sign_offs_required ?? []) &&
      a.authority_role === b.authority_role &&
      (a.terminal?.outcome ?? null) === (b.terminal?.outcome ?? null) &&
      JSON.stringify(a.metadata_defaults) === JSON.stringify(b.metadata_defaults)
    );
  }

  /// Union of step slugs across the two versions, preserving the
  /// authoring order of the current (B / `spec`) side first, then
  /// appending any slugs that only exist on the other (A) side.
  function slugUnion(a: WorkflowSpec, b: WorkflowSpec): string[] {
    const order: string[] = [];
    const seen = new Set<string>();
    for (const s of b.steps) {
      if (!seen.has(s.title)) {
        seen.add(s.title);
        order.push(s.title);
      }
    }
    for (const s of a.steps) {
      if (!seen.has(s.title)) {
        seen.add(s.title);
        order.push(s.title);
      }
    }
    return order;
  }

  function diffMark(
    step: StepSpec,
    other: WorkflowSpec,
    side: 'A' | 'B',
  ): DiffMark {
    const matching = other.steps.find((s) => s.title === step.title);
    if (!matching) return side === 'A' ? 'removed' : 'added';
    if (!stepFieldsEqual(step, matching)) return 'changed';
    return 'unchanged';
  }

  function diffBackground(mark: DiffMark): string | undefined {
    if (mark === 'added') return '#dcfce7';
    if (mark === 'removed') return '#fee2e2';
    if (mark === 'changed') return '#fef3c7';
    return undefined;
  }
</script>

{#if loadState.kind === 'loading'}
  <div class="catalog theme-exec">
    <p class="empty">Loading…</p>
  </div>
{:else if loadState.kind === 'error'}
  <div class="catalog theme-exec">
    <PageHeader eyebrow="Platform · Job kind" title={kindSlug} subtitle={loadState.message} />
  </div>
{:else}
  {@const spec = loadState.spec}
  {@const versions = loadState.versions}
  {@const compareSpec = compareVersion != null
    ? versions.find((v) => v.version === compareVersion) ?? null
    : null}

  <div class="catalog theme-exec">
    <Breadcrumb to={href('/it/registry')}>
      ← All job kinds
    </Breadcrumb>
    <PageHeader
      eyebrow={`Platform · Job kind · ${spec.category}`}
      title={spec.label}
      subtitle={`${spec.kind} · v${spec.version} · ${spec.status} · owned by ${spec.owning_team}`}
    />

    <div style="padding:0 24px 16px; display:flex; gap:12px; align-items:center">
      <button
        type="button"
        class="wb-btn wb-btn-primary"
        onclick={editNewVersion}
        disabled={action !== null}
        title="Author the next version in the graphical workspace (opens a fresh design Job; the active version + in-flight Jobs are untouched)"
      >
        {action === 'edit' ? 'Opening…' : 'Edit…'}
      </button>
      <button
        type="button"
        class="wb-btn"
        onclick={() => runAction('retire')}
        disabled={spec.status !== 'active' || action !== null}
        title="Flip the active row to retired — no new instances, in-flight Jobs unaffected"
      >
        {action === 'retire' ? 'Retiring…' : 'Retire'}
      </button>
      <button
        type="button"
        class="wb-btn"
        onclick={() => navigate(href(`/it/registry/new?fork=${encodeURIComponent(spec.kind)}`))}
        title="Create a new kind pre-populated from this one"
      >
        Fork…
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
                <td style="color:#888">Subject kinds</td>
                <td>
                  {#each spec.subject_kinds as s (s)}
                    <span class="chip chip-stage chip-stage-muted" style="margin-right:4px">{s}</span>
                  {/each}
                </td>
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

      {#snippet stepCard(step: StepSpec, bg: string | undefined, mark: string | undefined)}
        <div
          class="jd-step jd-step-ds-ready"
          style={bg ? `background:${bg}` : ''}
          title={mark}
        >
          <div class="jd-step-header">
            <span class="jd-step-icon">▶</span>
            <span class="jd-step-title mono">{step.title}</span>
            <span class="jd-step-kind-badge">{step.kind}</span>
            {#if step.terminal}
              <span class="jd-step-terminal">terminal → {step.terminal.outcome}</span>
            {/if}
            {#if (step.sign_offs_required ?? []).length > 0}
              <span class="jd-step-signoff-needed">needs sign-off</span>
            {/if}
          </div>
          <div class="jd-step-details">
            <span class="jd-step-waiting">
              ready_when: <span class="mono">{step.ready_when}</span>
            </span>
            {#if step.title_template}
              <span class="jd-step-waiting">
                title: {step.title_template}
              </span>
            {/if}
            {#if step.authority_role}
              <span class="jd-step-waiting">
                authority: <span class="mono">{step.authority_role}</span>
              </span>
            {/if}
          </div>
        </div>
      {/snippet}

      {#if compareSpec}
        <Section title={`Steps — v${compareSpec.version} vs v${spec.version}`} wide>
            <div style="padding:0 0 8px; display:flex; gap:8px; align-items:center">
              <button type="button" class="wb-btn" onclick={() => (compareVersion = null)}>
                Close diff
              </button>
              <span style="display:inline-flex; gap:12px; color:#666">
                <span style="display:inline-flex; gap:4px; align-items:center; font-size:12px">
                  <span style="width:10px; height:10px; background:#dcfce7; border-radius:2px"></span>
                  added
                </span>
                <span style="display:inline-flex; gap:4px; align-items:center; font-size:12px">
                  <span style="width:10px; height:10px; background:#fee2e2; border-radius:2px"></span>
                  removed
                </span>
                <span style="display:inline-flex; gap:4px; align-items:center; font-size:12px">
                  <span style="width:10px; height:10px; background:#fef3c7; border-radius:2px"></span>
                  changed
                </span>
              </span>
            </div>
            {@const slugKeys = slugUnion(compareSpec, spec)}
            <div style="display:grid; grid-template-columns:1fr 1fr; gap:16px">
              <div>
                <div style="font-size:12px; color:#666; margin-bottom:6px; font-weight:600">
                  v{compareSpec.version} ({compareSpec.status})
                </div>
                <div class="jd-steps">
                  {#each slugKeys as slug (slug)}
                    {@const step = compareSpec.steps.find((s) => s.title === slug)}
                    {#if step}
                      {@const mark = diffMark(step, spec, 'A')}
                      {@render stepCard(step, diffBackground(mark), mark)}
                    {:else}
                      <div class="jd-step jd-step-absent">
                        <span class="mono">{slug}</span> — not present in this version
                      </div>
                    {/if}
                  {/each}
                </div>
              </div>
              <div>
                <div style="font-size:12px; color:#666; margin-bottom:6px; font-weight:600">
                  v{spec.version} ({spec.status})
                </div>
                <div class="jd-steps">
                  {#each slugKeys as slug (slug)}
                    {@const step = spec.steps.find((s) => s.title === slug)}
                    {#if step}
                      {@const mark = diffMark(step, compareSpec, 'B')}
                      {@render stepCard(step, diffBackground(mark), mark)}
                    {:else}
                      <div class="jd-step jd-step-absent">
                        <span class="mono">{slug}</span> — not present in this version
                      </div>
                    {/if}
                  {/each}
                </div>
              </div>
            </div>
        </Section>
      {:else}
        <Section
          title={`Steps (${spec.steps.length} step${spec.steps.length === 1 ? '' : 's'})`}
          wide
        >
            {@const dag = workflowToDag(spec.steps)}
            <StepDag nodes={dag.nodes} edges={dag.edges} />
            <div class="jd-steps">
              {#each spec.steps as step (step.title)}
                {@render stepCard(step, undefined, undefined)}
              {/each}
            </div>
        </Section>
      {/if}

      <Section title={`Version history (${versions.length})`}>
          <table class="data-table data-table-striped">
            <thead>
              <tr>
                <th class="num">Version</th>
                <th>Status</th>
                <th>Owner</th>
                <th>Created</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {#each versions as v (v.version)}
                {@const isCurrent = v.version === spec.version}
                {@const isCompared = v.version === compareVersion}
                <tr>
                  <td class="num">{v.version}</td>
                  <td>
                    <span class="chip chip-stage chip-stage-{statusChipClass(v.status)}">
                      {v.status}
                    </span>
                  </td>
                  <td>{v.owning_team}</td>
                  <td>{new Date(v.created_at).toISOString().slice(0, 19).replace('T', ' ')}</td>
                  <td>
                    {#if isCurrent}
                      <span style="color:#888; font-size:12px">current</span>
                    {:else if isCompared}
                      <button
                        type="button"
                        class="wb-btn"
                        style="font-size:12px; padding:2px 8px"
                        onclick={() => (compareVersion = null)}
                      >
                        Clear
                      </button>
                    {:else}
                      <button
                        type="button"
                        class="wb-btn"
                        style="font-size:12px; padding:2px 8px"
                        onclick={() => (compareVersion = v.version)}
                        title={`Compare v${v.version} with current v${spec.version}`}
                      >
                        Compare
                      </button>
                    {/if}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
      </Section>
    </div>
  </div>
{/if}
