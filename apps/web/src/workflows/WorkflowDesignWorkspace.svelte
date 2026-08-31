<script lang="ts">
  // /it/registry/authoring/:jobId — the graphical authoring surface
  // for a `workflow-design` Job (decision D6). The working spec lives in
  // the design Job's publish-step `metadata.workflow_spec`; edits persist
  // there (debounced) as ordinary STEP_UPDATED events — no `workflows`
  // draft rows. The terminal `workflow-publish` step is the single
  // registry write + `jobs.kind.published` audit fact.
  //
  // Workflow: author → validate (gated on the live dry-run being clean)
  // → approve (workflow-approver sign-off) → publish.

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import StepAuthoringSurface from './StepAuthoringSurface.svelte';
  import type { WorkflowSpec } from './workflowTypes';
  import type { Job, Step, StepStatus } from '../jobs/types';
  import { validateDraft } from './liveLint';
  import { href, navigate } from '../router';
  import {
    loadDesignJob,
    findStep,
    readSpec,
    persistSpec,
    completeStep,
    signOff,
    initialSpec,
    PUBLISH_STEP_KIND,
    APPROVE_ROLE,
  } from './designJob';

  type Props = { jobId: string };
  let { jobId }: Props = $props();

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready' };
  let loadState = $state<LoadState>({ kind: 'loading' });

  let job = $state<Job | null>(null);
  let spec = $state<WorkflowSpec | null>(null);
  let initialized = false;

  let acting = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let savedAt = $state<string | null>(null);

  let steps = $derived<ReadonlyArray<Step>>(job?.steps ?? []);
  let slug = $derived(job?.subject.id ?? '');
  let authorStep = $derived(steps.find((s) => s.title === 'author'));
  let validateStep = $derived(steps.find((s) => s.title === 'validate'));
  let approveStep = $derived(steps.find((s) => s.title === 'approve'));
  let publishStep = $derived(steps.find((s) => s.kind === PUBLISH_STEP_KIND));
  let previousVersion = $derived(
    publishStep?.metadata?.['previous_kind_version'] as number | undefined,
  );

  function actionable(step: Step | undefined): boolean {
    return step != null && (step.status === 'ready' || step.status === 'active');
  }
  function done(step: Step | undefined): boolean {
    return step != null && (step.status === 'completed' || step.status === 'skipped');
  }

  async function refresh(): Promise<void> {
    const j = await loadDesignJob(jobId);
    job = j;
    if (!initialized) {
      const ps = findStep(j.steps ?? [], PUBLISH_STEP_KIND);
      spec = readSpec(ps) ?? initialSpec(j.subject.id, j.subject.id, '', []);
      initialized = true;
    }
  }

  $effect(() => {
    void jobId;
    // Re-seed from scratch whenever the design Job changes — otherwise the
    // `initialized` guard would keep a stale spec from the prior Job.
    initialized = false;
    loadState = { kind: 'loading' };
    void (async () => {
      try {
        await refresh();
        loadState = { kind: 'ready' };
      } catch (e) {
        loadState = {
          kind: 'error',
          message: e instanceof Error ? e.message : String(e),
        };
      }
    })();
  });

  // --- Debounced persist of the working spec onto the publish step ---
  let persistTimer: ReturnType<typeof setTimeout> | null = null;
  function schedulePersist(): void {
    if (persistTimer) clearTimeout(persistTimer);
    persistTimer = setTimeout(() => void doPersist(), 600);
  }
  async function doPersist(): Promise<void> {
    if (persistTimer) {
      clearTimeout(persistTimer);
      persistTimer = null;
    }
    if (!publishStep || !spec) return;
    try {
      await persistSpec(jobId, publishStep, spec, previousVersion);
      savedAt = new Date().toLocaleTimeString();
      actionError = null;
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }
  function editSpec(next: WorkflowSpec): void {
    spec = next;
    schedulePersist();
  }

  // --- Spec-field edits ---
  let subjectKindOptions = $state<string[]>([
    'asset', 'account', 'employee', 'vendor', 'campaign', 'purchase_order', 'custom',
  ]);
  let categoryOptions = $state<string[]>([]);
  $effect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [sk, cats] = await Promise.all([
          fetch('/api/subject-kinds'),
          fetch('/api/workflows'),
        ]);
        if (cancelled) return;
        if (sk.ok) {
          const rows = (await sk.json()) as Array<{ kind: string }>;
          subjectKindOptions = [...new Set([
            ...rows.map((x) => x.kind),
            'asset', 'account', 'employee', 'vendor', 'campaign', 'purchase_order', 'custom',
          ])].sort();
        }
        if (cats.ok) {
          const kinds = (await cats.json()) as Array<{ category?: string }>;
          categoryOptions = [...new Set(kinds.map((k) => k.category).filter((c): c is string => !!c))].sort();
        }
      } catch {
        // keep defaults
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function toggleSubject(s: string): void {
    if (!spec) return;
    const has = spec.subject_kinds.includes(s);
    editSpec({
      ...spec,
      subject_kinds: has
        ? spec.subject_kinds.filter((x) => x !== s)
        : [...spec.subject_kinds, s],
    });
  }

  // --- Workflow actions ---
  async function run(label: string, fn: () => Promise<void>): Promise<void> {
    acting = label;
    actionError = null;
    try {
      await fn();
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      acting = null;
    }
  }

  function markAuthored(): void {
    if (!authorStep) return;
    void run('author', () => completeStep(jobId, authorStep.id));
  }

  function validateAndAdvance(): void {
    if (!validateStep || !spec) return;
    void run('validate', async () => {
      await doPersist();
      const res = await validateDraft(spec!.kind || slug, spec!.steps);
      if (!res.ok) {
        throw new Error(
          `Spec isn't viable yet: ${res.problems.length} problem(s) flagged on the graph above. Fix them, then validate.`,
        );
      }
      await completeStep(jobId, validateStep.id);
    });
  }

  function approve(): void {
    if (!approveStep) return;
    void run('approve', async () => {
      await signOff(jobId, approveStep.id, APPROVE_ROLE);
      await completeStep(jobId, approveStep.id);
    });
  }

  async function publish(): Promise<void> {
    if (!publishStep || !spec) return;
    acting = 'publish';
    actionError = null;
    try {
      await doPersist();
      const res = await validateDraft(spec.kind || slug, spec.steps);
      if (!res.ok) {
        throw new Error(
          `Spec isn't viable: ${res.problems.length} problem(s) on the graph. Fix before publishing.`,
        );
      }
      await completeStep(jobId, publishStep.id);
      navigate(href(`/it/registry/${encodeURIComponent(slug)}`));
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
      acting = null;
    }
  }

  function chipClass(status: StepStatus): string {
    return status === 'completed'
      ? 'ok'
      : status === 'ready' || status === 'active'
        ? 'warn'
        : 'muted';
  }

  const RAIL: ReadonlyArray<{ title: string; label: string }> = [
    { title: 'author', label: 'Author' },
    { title: 'validate', label: 'Validate' },
    { title: 'approve', label: 'Approve' },
    { title: 'publish', label: 'Publish' },
  ];
</script>

<div class="catalog theme-exec">
  <Breadcrumb to={href('/it/registry')}>← All job kinds</Breadcrumb>

  {#if loadState.kind === 'loading'}
    <p class="empty">Loading…</p>
  {:else if loadState.kind === 'error'}
    <PageHeader eyebrow="Platform · Job kind" title="Authoring" subtitle={loadState.message} />
  {:else if spec}
    <PageHeader
      eyebrow="Platform · Job kind · authoring"
      title={`Authoring ${slug}`}
      subtitle={`Design Job ${jobId.slice(0, 8)} · the spec lives on this Job until the publish step writes it to the registry`}
    />

    <Section title="Workflow">
      <div class="wf-rail">
        {#each RAIL as r (r.title)}
          {@const step = steps.find((s) => s.title === r.title)}
          <div class="wf-step">
            <span class="chip chip-stage chip-stage-{step ? chipClass(step.status) : 'muted'}">
              {r.label}{step ? ` · ${step.status}` : ''}
            </span>
          </div>
        {/each}
      </div>
      <div class="wf-actions">
        <button
          type="button"
          class="wb-btn"
          onclick={markAuthored}
          disabled={!actionable(authorStep) || acting !== null}
          title="Mark the spec as authored and advance"
        >
          {acting === 'author' ? 'Working…' : '1 · Mark authored'}
        </button>
        <button
          type="button"
          class="wb-btn"
          onclick={validateAndAdvance}
          disabled={!actionable(validateStep) || acting !== null}
          title="Run the dry-run lint; advance only if the spec is viable"
        >
          {acting === 'validate' ? 'Validating…' : '2 · Validate & advance'}
        </button>
        <button
          type="button"
          class="wb-btn"
          onclick={approve}
          disabled={!actionable(approveStep) || acting !== null}
          title={`Stamp the ${APPROVE_ROLE} sign-off and advance`}
        >
          {acting === 'approve' ? 'Approving…' : `3 · Approve (${APPROVE_ROLE})`}
        </button>
        <button
          type="button"
          class="wb-btn wb-btn-primary"
          onclick={publish}
          disabled={!actionable(publishStep) || acting !== null}
          title="Complete the publish step — writes the kind to the registry and emits jobs.kind.published"
        >
          {acting === 'publish' ? 'Publishing…' : '4 · Publish'}
        </button>
      </div>
      <div class="wf-meta">
        {#if savedAt}<span class="wf-saved">draft saved {savedAt}</span>{/if}
        {#if done(publishStep)}<span class="wf-saved">published</span>{/if}
        {#if actionError}<span class="wf-error">{actionError}</span>{/if}
      </div>
    </Section>

    <Section title="Spec">
      <div style="display:grid; gap:12px; max-width:800px">
        <div>
          <div style="font-size:12px; color:#666; margin-bottom:2px">
            Kind slug <span style="color:#888"> — identity; fixed (it is this Job's subject)</span>
          </div>
          <input value={slug} class="mono" disabled style="padding:6px; font-size:13px; width:100%; background:#f3f4f6; color:#666" />
        </div>
        <div>
          <div style="font-size:12px; color:#666; margin-bottom:2px">Label</div>
          <input
            value={spec.label}
            oninput={(e) => editSpec({ ...spec!, label: (e.target as HTMLInputElement).value })}
            placeholder="Warranty Rework"
            style="padding:6px; font-size:13px; width:100%"
          />
        </div>
        <div>
          <div style="font-size:12px; color:#666; margin-bottom:2px">Category</div>
          <input
            list="ws-category-options"
            value={spec.category}
            oninput={(e) => editSpec({ ...spec!, category: (e.target as HTMLInputElement).value })}
            placeholder="production / sales / procurement / …"
            style="padding:6px; font-size:13px"
          />
          <datalist id="ws-category-options">
            {#each categoryOptions as c (c)}<option value={c}></option>{/each}
          </datalist>
        </div>
        <div>
          <div style="font-size:12px; color:#666; margin-bottom:2px">
            Subject kinds <span style="color:#888"> — what each Job of this kind is about</span>
          </div>
          <div style="display:flex; gap:8px; flex-wrap:wrap">
            {#each subjectKindOptions as s (s)}
              <label style="font-size:13px; display:flex; gap:4px; align-items:center">
                <input type="checkbox" checked={spec.subject_kinds.includes(s)} onchange={() => toggleSubject(s)} />
                {s}
              </label>
            {/each}
          </div>
        </div>
        <div>
          <div style="font-size:12px; color:#666; margin-bottom:2px">Description <span style="color:#888"> — optional</span></div>
          <textarea
            value={spec.description ?? ''}
            oninput={(e) => editSpec({ ...spec!, description: (e.target as HTMLTextAreaElement).value || null })}
            rows="3"
            style="padding:6px; font-size:13px; width:100%"
          ></textarea>
        </div>
      </div>
    </Section>

    <Section title="Steps" wide>
      <StepAuthoringSurface
        steps={spec.steps}
        kindSlug={slug}
        onChange={(s) => editSpec({ ...spec!, steps: s })}
      />
    </Section>
  {/if}
</div>

<style>
  .wf-rail {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    margin-bottom: 12px;
  }
  .wf-step::after {
    content: '→';
    color: #cbd5e1;
    margin-left: 8px;
  }
  .wf-step:last-child::after {
    content: '';
  }
  .wf-actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    align-items: center;
  }
  .wf-meta {
    margin-top: 8px;
    display: flex;
    gap: 12px;
    align-items: center;
    min-height: 18px;
  }
  .wf-saved {
    font-size: 12px;
    color: #16a34a;
  }
  .wf-error {
    font-size: 13px;
    color: #dc2626;
  }
</style>
