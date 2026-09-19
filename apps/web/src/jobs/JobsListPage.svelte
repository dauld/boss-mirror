<script lang="ts">
  // Phase-1 port of apps/web/src/jobs/JobsListPage.tsx.
  //
  // Accepts an initialKind / initialKindPrefix / initialStatus set of
  // props so the Work bucket pre-saved queues (Service queue, Sales
  // pipeline, Refurb queue) can mount this component with a kind
  // pre-filter. Same pattern as the React app.

  import { navigate, href } from '../router';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import { shortId } from '../data/ids';
  import { subjectLabel, subjectPath, type Job } from './types';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { session } from '@boss/web-kit/session/session.svelte';
  import WriteGate from '@boss/web-kit/ui/WriteGate.svelte';
  import { appToday } from '@boss/web-kit/sim-clock';
  import { registeredAdHoc } from './adHoc';

  let userId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : '',
  );

  let {
    initialKind = '',
    initialKindPrefix = '',
    initialStatus = 'open',
    initialOwnerId = '',
    initialSubjectKind = '',
    initialSubjectId = '',
    pageTitle,
    eyebrow = 'Work',
    initialNewJobOpen = false,
    initialNewJobSubjectKind = '',
    initialNewJobSubjectId = '',
  } = $props<{
    initialKind?: string;
    initialKindPrefix?: string;
    initialStatus?: string;
    // #93: list-filter props. owner_id filters by Job.owner_id;
    // subjectKind+subjectId filter by Job.subject_kind+subject_id.
    initialOwnerId?: string;
    initialSubjectKind?: string;
    initialSubjectId?: string;
    pageTitle?: string;
    eyebrow?: string;
    // Deep-link params from /jobs?new=1&subject_kind=…&subject_id=…
    // (Phase 3 of create-Job UX; populated when a Subject detail
    // page sends the user here pre-filled).
    initialNewJobOpen?: boolean;
    initialNewJobSubjectKind?: string;
    initialNewJobSubjectId?: string;
  }>();

  let kind = $state(initialKind);
  let status = $state(initialStatus);
  // Operator-typed subject-id override. Falls back to initialSubjectId
  // when the page was opened with a pre-filter (e.g. drilled in from an
  // Account detail page); typing here narrows the result set further.
  let subjectIdFilter = $state(initialSubjectId);
  let jobs = $state<Job[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let total = $state(0);

  // Auto-load kinds for the filter dropdown on mount; no user
  // interaction required.
  $effect(() => {
    void loadKinds();
  });

  $effect(() => {
    const k = kind;
    const kp = initialKindPrefix;
    const s = status;
    const o = initialOwnerId;
    const sk = initialSubjectKind;
    const si = subjectIdFilter;
    let cancelled = false;
    loading = true;

    const params = new URLSearchParams();
    if (k) params.set('kind', k);
    if (kp) params.set('kind_prefix', kp);
    if (s) params.set('status', s);
    if (o) params.set('owner_id', o);
    if (si) params.set('subject_id', si);
    params.set('limit', '200');

    (async () => {
      try {
        const resp = await fetch(`/api/jobs?${params}`);
        if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
        const body = (await resp.json()) as { data: Job[]; total: number };
        if (!cancelled) {
          jobs = body.data ?? [];
          total = body.total ?? 0;
          loading = false;
          error = null;
        }
      } catch (e) {
        if (!cancelled) {
          error = e instanceof Error ? e.message : String(e);
          loading = false;
        }
      }
    })();

    return () => {
      cancelled = true;
    };
  });

  const STATUS_OPTIONS = [
    { v: 'open', l: 'Open' },
    { v: 'blocked', l: 'Blocked' },
    { v: 'pending-sign-off', l: 'Pending sign-off' },
    { v: 'closed', l: 'Closed' },
    { v: '', l: 'All' },
  ];

  const titleFor = $derived(
    pageTitle ??
      (kind ? `${kind} jobs` : initialKindPrefix ? `${initialKindPrefix} jobs` : 'All jobs'),
  );

  // --- New Job creation ---
  // Two entry points: "Start a new Job" pops the picker with no
  // kind preselected; "Create Ad Hoc" preselects the `ad-hoc`
  // Workflow (both shipped tenants register one — brewery +
  // device-shop seeds, under operations, accepting every platform
  // Subject kind; #92). The registry is data, though, so the button
  // renders only when the loaded registry carries the row: with no
  // row the click preselected a kind the select could not show and
  // changed nothing visible (backlog a399613d, interaction crawl
  // f2b8a01c). Both share the same inline form — no modal library,
  // just a collapsible section under the page header so the surface
  // mirrors the rest of the catalog UI.

  type StepSpecRow = {
    kind: string;
    title_template: string;
    sign_offs_required?: string[];
    authority_role?: string | null;
  };
  type WorkflowRow = {
    kind: string;
    label: string;
    description?: string | null;
    category?: string | null;
    subject_kinds: string[];
    steps?: StepSpecRow[] | null;
  };
  type SubjectOption = { id: string; label: string };
  type SubjectOptionsState = {
    options: SubjectOption[];
    capped: boolean;
    total: number;
  };
  type Owner = { id: string; name: string; role?: string };

  let newJobOpen = $state(false);
  let kinds = $state<WorkflowRow[]>([]);
  /** The registered ad-hoc Workflow, once the registry has loaded;
   *  null until then and null for a tenant that registers none. */
  const adHoc = $derived(registeredAdHoc(kinds));
  let kindsLoading = $state(false);
  /** A FAILED registry read is held here, not retried (backlog
   *  06038ed8). The guard in loadKinds() is read inside the mount
   *  effect below, so kinds/kindsLoading are effect dependencies: a
   *  failure used to reset kindsLoading, re-trigger the effect and
   *  fetch again — 554 /api/workflows reads from one mocked mount,
   *  measured 2026-09-19. Holding the failure ends the loop; the
   *  operator gestures that need the registry ask again explicitly. */
  let kindsError = $state<string | null>(null);
  let owners = $state<Owner[]>([]);
  let formKind = $state('');
  let formSubjectKind = $state('');
  let formSubjectId = $state('');
  let formOwnerId = $state('');
  let formTitle = $state('');
  let formError = $state<string | null>(null);
  let formSubmitting = $state(false);

  // Keyed by `subject_kind`. Fetched lazily when a kind is picked
  // and reused on subsequent picks. Empty list means "no
  // autocomplete available" (e.g. custom).
  let subjectOptions = $state<Record<string, SubjectOptionsState>>({});

  // Submit is enabled only when the user has picked the kind +
  // subject_kind and entered a non-empty subject id. Without this,
  // the user can click Create and get a "Subject id is required"
  // error after the fact — small annoyance the disabled state
  // prevents.
  const canSubmit = $derived(
    !!formKind && !!formSubjectKind && formSubjectId.trim().length > 0,
  );

  const allowedSubjectKinds = $derived.by(() => {
    const spec = kinds.find((k) => k.kind === formKind);
    if (spec) return spec.subject_kinds;
    // No kind picked yet — if the user came in via a deep-link
    // with a subject_kind, surface that as the only option so the
    // subject_kind select isn't empty. Otherwise show every
    // subject_kind that any kind in the registry references.
    if (formSubjectKind) return [formSubjectKind];
    const all = new Set<string>();
    for (const k of kinds) for (const sk of k.subject_kinds) all.add(sk);
    return Array.from(all);
  });
  const selectedKindSpec = $derived(
    kinds.find((k) => k.kind === formKind) ?? null,
  );
  const currentSubjectOptionsState = $derived<SubjectOptionsState>(
    (formSubjectKind && subjectOptions[formSubjectKind]) || {
      options: [],
      capped: false,
      total: 0,
    },
  );
  const currentSubjectOptions = $derived(currentSubjectOptionsState.options);

  // When the user lands via a Subject-page deep-link
  // (?subject_kind=account&subject_id=…), filter the Workflow picker
  // to only the kinds that accept that subject_kind. Without this,
  // a user looking at "wholesale-keg-order" is shown alongside
  // "morning-brew" even though morning-brew can't take an account
  // subject.
  const visibleKinds = $derived(
    formSubjectKind
      ? kinds.filter((k) => k.subject_kinds.includes(formSubjectKind))
      : kinds,
  );

  /// `retry: true` is an operator gesture (focusing the Kind filter,
  /// opening the new-job form) clearing a held failure and asking
  /// again. The mount effect never passes it.
  async function loadKinds(opts?: { retry?: boolean }) {
    if (opts?.retry) kindsError = null;
    if (kinds.length > 0 || kindsLoading || kindsError !== null) return;
    kindsLoading = true;
    try {
      const resp = await fetch('/api/workflows');
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      // /api/workflows returns a plain array of WorkflowSpec rows;
      // we keep `description` + `category` so the form can preview
      // what's about to happen.
      kinds = (await resp.json()) as WorkflowRow[];
    } catch (e) {
      kindsError = e instanceof Error ? e.message : String(e);
      formError = kindsError;
    } finally {
      kindsLoading = false;
    }
  }

  // Per-subject_kind list endpoints. Each returns either an array
  // or a `{data: [...]}` envelope; the loader normalises both. The
  // mapping reflects the actual service URLs in the dev-server +
  // gateway proxy table.
  const SUBJECT_LIST_URLS: Record<string, string> = {
    account: '/api/people/accounts',
    vendor: '/api/inventory/vendors',
    employee: '/api/people',
    location: '/api/locations',
    system: '/api/assets?limit=500',
    purchase_order: '/api/inventory/purchase-orders?limit=500',
  };

  async function loadSubjectOptions(kind: string): Promise<void> {
    if (subjectOptions[kind]) return;
    const url = SUBJECT_LIST_URLS[kind];
    if (!url) {
      // No autocomplete for this subject_kind (e.g. custom,
      // campaign). Fall through to free-text input.
      subjectOptions = {
        ...subjectOptions,
        [kind]: { options: [], capped: false, total: 0 },
      };
      return;
    }
    try {
      const r = await fetch(url);
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const body = await r.json();
      const rows: Array<Record<string, unknown>> = Array.isArray(body)
        ? body
        : ((body as { data?: Array<Record<string, unknown>> }).data ?? []);
      // For envelope responses, `total` is the DB-wide count behind
      // the (capped) `rows`. Surfacing it lets the form hint warn
      // when the autocomplete is incomplete.
      const total =
        !Array.isArray(body) && typeof (body as { total?: unknown }).total === 'number'
          ? ((body as { total: number }).total)
          : rows.length;
      const opts = rows
        .map((row) => {
          const id =
            row['id'] ?? '';
          const name = row['name'] ?? row['serial'] ?? row['label'] ?? '';
          return id ? { id: String(id), label: String(name || id) } : null;
        })
        .filter((opt): opt is SubjectOption => opt !== null);
      subjectOptions = {
        ...subjectOptions,
        [kind]: { options: opts, capped: total > opts.length, total },
      };
    } catch (e) {
      // Failure leaves `subjectOptions[kind]` undefined and the
      // form falls back to a plain text input — the user can
      // still type a known id by hand.
      formError =
        formError ?? (e instanceof Error ? e.message : String(e));
    }
  }

  async function loadOwners(): Promise<void> {
    if (owners.length > 0) return;
    try {
      const r = await fetch('/api/people');
      if (!r.ok) return;
      const body = (await r.json()) as Owner[];
      owners = body;
    } catch {
      // Empty list = the form's owner picker shows just "Unassigned".
    }
  }

  function openNewJob(opts?: {
    kind?: string;
    subjectKind?: string;
    subjectId?: string;
  }) {
    newJobOpen = true;
    formKind = opts?.kind ?? '';
    formSubjectKind = opts?.subjectKind ?? '';
    formSubjectId = opts?.subjectId ?? '';
    formTitle = '';
    formError = null;
    // Default the owner to the current persona — it's almost always
    // who's about to run the new Job. The user can override before
    // submit.
    formOwnerId = userId ?? '';
    // Opening the form is a gesture too, and the form is unusable
    // without the registry — but it is reached from the deep-link
    // effect as well, which is why that effect guards on newJobOpen.
    void loadKinds({ retry: true });
    void loadOwners();
    // If the deep-link picked a subject_kind, prime its
    // autocomplete list right away so the input is useful by the
    // time the form animates in.
    if (opts?.subjectKind) {
      void loadSubjectOptions(opts.subjectKind);
    }
  }

  // Auto-open the form on mount when the deep-link params are
  // present. The Subject detail pages send users here via
  // /jobs?new=1&subject_kind=account&subject_id=acc-bigseed-0001;
  // landing on the page with the form already populated is the
  // whole point of the deep-link.
  $effect(() => {
    if (initialNewJobOpen && !newJobOpen) {
      openNewJob({
        subjectKind: initialNewJobSubjectKind,
        subjectId: initialNewJobSubjectId,
      });
    }
  });

  // When kind changes, default subject_kind to the first allowed
  // (so the form renders a usable input even before the user
  // touches it). Also kicks off the per-kind subject autocomplete
  // fetch so the datalist populates by the time the user lands on
  // the input.
  $effect(() => {
    const first = allowedSubjectKinds[0];
    if (first && !allowedSubjectKinds.includes(formSubjectKind)) {
      formSubjectKind = first;
    }
    if (formSubjectKind) {
      void loadSubjectOptions(formSubjectKind);
    }
  });

  // Build the Subject payload. The Custom variant carries
  // {custom_kind, ref_id}; every other variant uses the plain
  // {id} plus its tag.
  function buildSubject(): Record<string, unknown> | null {
    if (!formSubjectKind || !formSubjectId.trim()) return null;
    const id = formSubjectId.trim();
    if (formSubjectKind === 'asset') {
      return { subject_kind: 'asset', id };
    }
    if (formSubjectKind === 'custom') {
      return { subject_kind: 'custom', custom_kind: 'custom', ref_id: id };
    }
    return { subject_kind: formSubjectKind, id };
  }

  async function submitNewJob(e: Event) {
    e.preventDefault();
    if (formSubmitting) return;
    formError = null;
    const subject = buildSubject();
    if (!formKind) {
      formError = 'Pick a job kind';
      return;
    }
    if (!subject) {
      formError = 'Subject id is required';
      return;
    }
    const today = appToday();
    // Default title: prefer the Workflow's human label + the
    // Subject's name (resolved from autocomplete options) over the
    // raw slugs. Falls back to the slug shape when we don't know
    // the labels (custom subject, list endpoint failed, etc.).
    const kindLabel = selectedKindSpec?.label ?? formKind;
    const subjectName =
      currentSubjectOptions.find((o) => o.id === formSubjectId.trim())?.label ??
      formSubjectId.trim();
    const body = {
      kind: formKind,
      subject,
      title:
        formTitle.trim() ||
        `${kindLabel} — ${subjectName}`,
      owner_id: formOwnerId,
      status: 'open',
      priority: 'standard',
      opened_on: today,
      metadata: {},
      tags: [],
    };
    formSubmitting = true;
    try {
      const resp = await fetch('/api/jobs', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!resp.ok) {
        const text = await resp.text();
        throw new Error(`HTTP ${resp.status}: ${text}`);
      }
      const created = (await resp.json()) as { id: string };
      navigate(entityHref('job', created.id));
    } catch (e) {
      formError = e instanceof Error ? e.message : String(e);
    } finally {
      formSubmitting = false;
    }
  }
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow={eyebrow}
    title={titleFor}
    subtitle={`${total.toLocaleString()} ${status || 'any-status'}`}
    motif="hops"
  />

  <!-- Filters: narrow the list down without leaving the page. The
       same state powers the API query so the "All jobs" page can
       drill into any kind / status combination operators care about. -->
  <div class="job-filters">
    <label class="job-filter">
      <span>Kind</span>
      <select bind:value={kind} onfocus={() => void loadKinds({ retry: true })}>
        <option value="">All kinds</option>
        {#each kinds.slice().sort((a, b) => a.kind.localeCompare(b.kind)) as k (k.kind)}
          <option value={k.kind}>{k.kind}</option>
        {/each}
      </select>
    </label>
    <label class="job-filter">
      <span>Status</span>
      <select bind:value={status}>
        {#each STATUS_OPTIONS as opt (opt.v)}
          <option value={opt.v}>{opt.l}</option>
        {/each}
      </select>
    </label>
    <label class="job-filter">
      <span>Subject id</span>
      <input
        type="text"
        placeholder="e.g. acc-bigseed-0012"
        bind:value={subjectIdFilter}
      />
    </label>
    {#if kind || status !== initialStatus || subjectIdFilter}
      <button
        type="button"
        class="job-filter-clear"
        onclick={() => { kind = ''; status = initialStatus; subjectIdFilter = ''; }}
        title="Clear all filters"
      >
        Clear ✕
      </button>
    {/if}
  </div>

  <div class="job-actions">
    <!-- Admission is a write: a guest sees the entry buttons disabled
         (readonly gate) rather than a composer whose POST 403s. -->
    <WriteGate>
      <button type="button" class="btn-primary" onclick={() => openNewJob()}>
        Start a new Job
      </button>
      {#if adHoc}
        <button type="button" class="btn-secondary" onclick={() => openNewJob({ kind: adHoc.kind })}>
          Create Ad Hoc Job
        </button>
      {/if}
    </WriteGate>
  </div>

  {#if newJobOpen}
    <form class="new-job-form" onsubmit={submitNewJob}>
      <div class="form-row">
        <label>
          <span>Kind</span>
          <select bind:value={formKind}>
            <option value="">— select —</option>
            {#each visibleKinds as k (k.kind)}
              <option value={k.kind}>
                {k.label === k.kind ? k.label : `${k.label} (${k.kind})`}
              </option>
            {/each}
          </select>
          {#if formSubjectKind && visibleKinds.length < kinds.length}
            <small class="hint">
              Filtered to kinds that accept a {formSubjectKind} subject
              ({visibleKinds.length} of {kinds.length})
            </small>
          {/if}
        </label>
        <label>
          <span>Subject kind</span>
          <select bind:value={formSubjectKind}>
            <option value="">— select —</option>
            {#each allowedSubjectKinds as sk (sk)}
              <option value={sk}>{sk}</option>
            {/each}
          </select>
        </label>
      </div>

      {#if selectedKindSpec?.description}
        <p class="kind-description">
          {#if selectedKindSpec.category}
            <span class="kind-category">{selectedKindSpec.category}</span>
          {/if}
          {selectedKindSpec.description}
        </p>
      {/if}

      {#if selectedKindSpec?.steps?.length}
        <details class="step-preview" open>
          <summary>
            Step preview · {selectedKindSpec.steps.length} steps
          </summary>
          <ol class="step-preview-list">
            {#each selectedKindSpec.steps as step, idx (idx)}
              <li>
                <span class="step-preview-kind">{step.kind}</span>
                <span class="step-preview-title">{step.title_template}</span>
                {#if (step.sign_offs_required ?? []).length > 0}
                  <span class="step-preview-signoff" title="Sign-off required">
                    ✓ {step.authority_role ?? 'sign-off'}
                  </span>
                {/if}
              </li>
            {/each}
          </ol>
        </details>
      {/if}

      <div class="form-row">
        <label class="grow">
          <span>Subject id</span>
          <input
            type="text"
            bind:value={formSubjectId}
            list="new-job-subject-options"
            placeholder={
              currentSubjectOptions.length > 0
                ? 'Pick from the list or type an id'
                : 'Type the id by hand (no autocomplete for this kind)'
            }
            autocomplete="off"
          />
          <datalist id="new-job-subject-options">
            {#each currentSubjectOptions as opt (opt.id)}
              <option value={opt.id} label={opt.label}></option>
            {/each}
          </datalist>
          {#if currentSubjectOptions.length > 0}
            <small class="hint">
              {#if currentSubjectOptionsState.capped}
                Showing {currentSubjectOptions.length.toLocaleString()} of {currentSubjectOptionsState.total.toLocaleString()} {formSubjectKind} options — type the id by hand if you don't see yours.
              {:else}
                {currentSubjectOptions.length} {formSubjectKind} options
              {/if}
            </small>
          {/if}
        </label>
      </div>

      <div class="form-row">
        <label class="grow">
          <span>Owner</span>
          <select bind:value={formOwnerId}>
            <option value="">— unassigned —</option>
            {#each owners as o (o.id)}
              <option value={o.id}>
                {o.name}{o.role ? ` (${o.role})` : ''}
              </option>
            {/each}
          </select>
        </label>
      </div>

      <div class="form-row">
        <label class="grow">
          <span>Title (optional)</span>
          <input type="text" bind:value={formTitle} placeholder="Defaulted from kind + subject" />
        </label>
      </div>
      {#if formError}
        <p class="form-error">{formError}</p>
      {/if}
      <div class="form-actions">
        <button
          type="submit"
          class="btn-primary"
          disabled={formSubmitting || !canSubmit}
        >
          {formSubmitting ? 'Creating…' : 'Create Job'}
        </button>
        <button
          type="button"
          class="btn-secondary"
          onclick={() => {
            newJobOpen = false;
            // If the user landed via a deep-link
            // (?new=1&subject_kind=…), clear the URL params on
            // cancel so a refresh doesn't re-open the form. Use
            // history.replaceState to avoid pushing a back-button
            // entry for the cancellation.
            if (
              typeof window !== 'undefined' &&
              window.location.search.includes('new=1')
            ) {
              const path = window.location.pathname + window.location.hash;
              window.history.replaceState(null, '', path);
            }
          }}
          disabled={formSubmitting}
        >
          Cancel
        </button>
      </div>
    </form>
  {/if}

  <div class="catalog-layout">
    <aside class="catalog-filters">
      <div class="filter-group">
        <div class="filter-label">Status</div>
        {#each STATUS_OPTIONS as opt (opt.v)}
          <button
            type="button"
            class="filter-button {status === opt.v ? 'filter-button-active' : ''}"
            onclick={() => (status = opt.v)}
          >
            {opt.l}
          </button>
        {/each}
      </div>
    </aside>

    <section class="list-section">
      {#if loading}
        <p class="empty">Loading…</p>
      {:else if error}
        <p class="empty">Couldn't load jobs: {error}</p>
      {:else if jobs.length === 0}
        <p class="empty">No jobs match.</p>
      {:else}
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>ID</th>
              <th>Kind</th>
              <th>Title</th>
              <th>Subject</th>
              <th>Status</th>
              <th>Priority</th>
              <th>Opened</th>
            </tr>
          </thead>
          <tbody>
            {#each jobs as j (j.id)}
              <tr
                class="data-table-row-link"
                onclick={() => navigate(entityHref('job', j.id))}
              >
                <td class="mono">
                  <a
                    href={entityHref('job', j.id)}
                    onclick={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      navigate(entityHref('job', j.id));
                    }}
                  >
                    {shortId(j.id)}
                  </a>
                </td>
                <td>{j.kind}</td>
                <td>{j.title}</td>
                <td class="mono">
                  <a
                    href={href(subjectPath(j.subject))}
                    onclick={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      navigate(href(subjectPath(j.subject)));
                    }}
                  >
                    {subjectLabel(j.subject)}
                  </a>
                </td>
                <td>{j.status}</td>
                <td>{j.priority}</td>
                <td>{j.opened_on}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  </div>
</div>

<style>
  .job-filters {
    display: flex;
    flex-wrap: wrap;
    gap: 12px 16px;
    align-items: end;
    margin-bottom: 16px;
    padding: 12px 16px;
    background: rgba(0, 0, 0, 0.02);
    border: 1px solid rgba(0, 0, 0, 0.08);
    border-radius: 6px;
  }
  .job-filter {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
  }
  .job-filter > span {
    color: rgba(0, 0, 0, 0.55);
    text-transform: uppercase;
    letter-spacing: 0.4px;
    font-size: 10px;
    font-weight: 600;
  }
  .job-filter select,
  .job-filter input {
    padding: 6px 10px;
    font-size: 13px;
    border: 1px solid rgba(0, 0, 0, 0.18);
    border-radius: 4px;
    background: white;
    min-width: 160px;
  }
  .job-filter-clear {
    align-self: end;
    padding: 6px 12px;
    font-size: 12px;
    background: transparent;
    color: rgba(0, 0, 0, 0.6);
    border: 1px solid rgba(0, 0, 0, 0.18);
    border-radius: 4px;
    cursor: pointer;
  }
  .job-filter-clear:hover {
    background: rgba(0, 0, 0, 0.04);
  }
  .job-actions {
    display: flex;
    gap: 12px;
    margin-bottom: 16px;
  }
  /* The readonly gate is a mechanism, not a layout box — hand the
     action row's flex layout down to it so the two buttons keep
     their gap whether or not the gate is disabling them. */
  .job-actions :global(.write-gate) {
    display: flex;
    gap: 12px;
  }
  .btn-primary,
  .btn-secondary {
    padding: 8px 16px;
    border-radius: 6px;
    font: inherit;
    cursor: pointer;
    border: 1.5px solid var(--brew-amber);
  }
  .btn-primary {
    background: var(--brew-amber);
    color: var(--brew-malt);
  }
  .btn-primary:disabled {
    opacity: 0.6;
    cursor: progress;
  }
  .btn-secondary {
    background: transparent;
    color: var(--brew-malt);
  }
  .new-job-form {
    background: var(--brew-amber-bg, rgba(212, 165, 91, 0.08));
    border: 1.5px solid var(--brew-amber);
    border-radius: 8px;
    padding: 16px;
    margin-bottom: 16px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .form-row {
    display: flex;
    gap: 12px;
    flex-wrap: wrap;
  }
  .form-row label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex: 0 0 auto;
  }
  .form-row label.grow {
    flex: 1 1 auto;
  }
  .form-row label span {
    font-size: 12px;
    color: var(--muted, #666);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .form-row input,
  .form-row select {
    padding: 6px 10px;
    border-radius: 4px;
    border: 1px solid var(--border, #ccc);
    background: white;
    font: inherit;
    min-width: 220px;
  }
  .form-error {
    color: #b00020;
    margin: 0;
  }
  .kind-description {
    margin: 0;
    padding: 8px 12px;
    background: var(--brew-amber-bg, rgba(212, 165, 91, 0.05));
    border-left: 3px solid var(--brew-amber);
    border-radius: 2px;
    color: var(--brew-malt, #3d2c1a);
    font-size: 13px;
    line-height: 1.45;
  }
  .kind-category {
    display: inline-block;
    margin-right: 8px;
    padding: 1px 6px;
    border-radius: 3px;
    background: var(--brew-amber);
    color: white;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-weight: 600;
    vertical-align: middle;
  }
  .hint {
    color: var(--muted, #888);
    font-size: 11px;
    margin-top: 4px;
  }
  .step-preview {
    background: rgba(255, 255, 255, 0.6);
    border: 1px dashed var(--brew-amber, #d4a55b);
    border-radius: 6px;
    padding: 8px 12px;
    font-size: 13px;
  }
  .step-preview > summary {
    cursor: pointer;
    font-weight: 500;
    color: var(--brew-malt, #3d2c1a);
    list-style: none;
  }
  .step-preview > summary::-webkit-details-marker { display: none; }
  .step-preview > summary::before {
    content: '▸';
    display: inline-block;
    margin-right: 6px;
    transition: transform 0.15s ease;
  }
  .step-preview[open] > summary::before {
    transform: rotate(90deg);
  }
  .step-preview-list {
    margin: 8px 0 0 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .step-preview-list li {
    display: grid;
    grid-template-columns: 60px 100px 1fr auto;
    gap: 8px;
    align-items: center;
    padding: 4px 6px;
    border-radius: 3px;
  }
  .step-preview-list li:nth-child(odd) {
    background: rgba(212, 165, 91, 0.06);
  }
  .step-preview-tier {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted, #888);
  }
  .step-preview-kind {
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 11px;
    color: var(--brew-malt, #3d2c1a);
    background: rgba(212, 165, 91, 0.18);
    padding: 1px 6px;
    border-radius: 3px;
  }
  .step-preview-title { color: var(--text, #1c1917); }
  .step-preview-signoff {
    font-size: 11px;
    color: #2563eb;
    background: rgba(37, 99, 235, 0.08);
    padding: 1px 6px;
    border-radius: 3px;
  }
  .form-actions {
    display: flex;
    gap: 8px;
  }
</style>
