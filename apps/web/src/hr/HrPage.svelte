<script lang="ts">
  // HR admin — port of apps/web/src/hr/HrPage.tsx.
  //
  // Five tabs: Overview, Workflows (onboarding/offboarding task
  // tracking with POST mutations), Requisitions (placeholder),
  // Certifications, Headcount.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { appNow } from '@boss/web-kit/sim-clock';
  import {
    humanizeClassCode,
    type Department,
    type Employee,
  } from '../people/types';
  import { tenureYears, expiringCerts } from '../people/utils';
  import {
    workflowSurfaces,
    type WorkflowSpec,
  } from '../workflows/workflowTypes';
  import { fetchRemote } from '../data/remote';
  import {
    fetchEmployeeTasks,
    fetchStepProgress,
    hrJobRows,
    type TasksRead,
  } from './hr-tasks';
  import { href } from '../router';

  type Tab = 'overview' | 'requisitions' | 'certs' | 'headcount' | 'workflows';

  const TABS: ReadonlyArray<{ id: Tab; label: string }> = [
    { id: 'overview', label: 'Overview' },
    { id: 'workflows', label: 'Workflows' },
    { id: 'requisitions', label: 'Requisitions' },
    { id: 'certs', label: 'Certifications' },
    { id: 'headcount', label: 'Headcount' },
  ];

  let roster = $state<Employee[]>([]);
  /// Non-null when the roster load failed — rendered instead of the
  /// zeroed-out overview, so an outage never reads as "0 active
  /// employees" (packet 3fba9c35, the false-empty sweep).
  let rosterFailed = $state<string | null>(null);
  let loading = $state(true);
  let tab = $state<Tab>('overview');

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const r = await fetch('/api/people');
        if (r.ok) {
          const body = (await r.json()) as Employee[];
          if (!cancelled) {
            roster = body;
            rosterFailed = null;
          }
        } else {
          if (!cancelled) rosterFailed = `HTTP ${r.status}`;
        }
      } catch (e) {
        if (!cancelled) rosterFailed = e instanceof Error ? e.message : String(e);
      }
      if (!cancelled) loading = false;
    })();
    return () => {
      cancelled = true;
    };
  });

  let active = $derived(roster.filter((e) => e.status === 'active'));
  let onLeave = $derived(roster.filter((e) => e.status === 'on-leave'));
  let expiring90 = $derived(expiringCerts(90, roster));
  let expiring30 = $derived(expiringCerts(30, roster));
  let avgTenure = $derived(
    active.length > 0
      ? (active.reduce((s, e) => s + tenureYears(e), 0) / active.length).toFixed(1)
      : '0',
  );

  let byDept = $derived.by(() => {
    const m = new Map<Department, { active: number; onLeave: number; openReqs: number }>();
    for (const e of roster) {
      if (!e.department) continue;
      const entry = m.get(e.department) ?? { active: 0, onLeave: 0, openReqs: 0 };
      if (e.status === 'active') entry.active++;
      if (e.status === 'on-leave') entry.onLeave++;
      m.set(e.department, entry);
    }
    return [...m.entries()].sort((a, b) => b[1].active - a[1].active);
  });

  // ------------------------------------------------------------
  // Workflows tab — HR workflows driven through the canonical
  // Job/Step abstractions (#101). Pre-#101 this tab POSTed to
  // /api/people/{id}/onboard (a bespoke endpoint that updated
  // Employee.status + wrote employee_changes directly) and listed
  // /api/people/workflows (a separate aggregation surface). Both
  // bypassed the Workflow / Step / authority_role / audit_log
  // machinery that every other workflow in BOSS rides on.
  // Post-#101: 'Start workflow' opens an HR Workflow via the
  // canonical /jobs?new=1 deep-link; 'Active workflows' lists open
  // Jobs of those kinds. The bespoke endpoints stay (no breakage
  // of operator-baseline scripts) but the SPA stops driving them.
  //
  // Which Workflows are HR workflows is DATA, not code: a Workflow
  // declares `metadata.surfaces ⊇ ['hr']` to appear here. The page
  // discovers them from /api/workflows so it stays tenant-agnostic
  // (no tenant Workflow slugs baked in).
  // ------------------------------------------------------------

  /// `total_tasks` / `done_tasks` are NULL when the Job's step read
  /// failed. They used to be zeroed by a `.catch(() => [])`, which the
  /// progress column then drew as "0/0 tasks (0%)" — a statement about
  /// how far along a person's onboarding is, made by a read that never
  /// happened (backlog a704c5eb).
  type ActiveWorkflow = {
    employee_id: string;
    employee_name: string;
    workflow: string;
    job_id: string;
    total_tasks: number | null;
    done_tasks: number | null;
  };

  const CATEGORY_LABEL: Record<string, string> = {
    'it-setup': 'IT Setup',
    'hr-paperwork': 'HR Paperwork',
    training: 'Training',
    access: 'Access',
    equipment: 'Equipment',
    'knowledge-transfer': 'Knowledge Transfer',
    'asset-return': 'Asset Return',
    'exit-interview': 'Exit Interview',
  };

  // HR Workflows discovered from the registry. A Workflow belongs
  // here when its `metadata.surfaces` includes 'hr'. We additionally
  // require subject_kinds ⊇ {employee} (HR Jobs are about an
  // Employee), but `surfaces:'hr'` is the primary signal. `{ kind,
  // label }` is everything the workflow tab needs: `kind` drives the
  // open-Jobs fetch + the Job-creation deep-link; `label` is the
  // display string for the chip + the Start button.
  type HrKind = { kind: string; label: string };

  let hrKinds = $state<HrKind[]>([]);

  let workflows = $state<ActiveWorkflow[]>([]);
  let selectedEmp = $state<string | null>(null);
  /// The task read as a discriminated union, not a list. There is no
  /// fourth option: `ready` (with a possibly-empty list), `failed`, or
  /// `no-job`, and the template has to branch to render anything — so a
  /// failed read can no longer borrow the empty state's words.
  let tasks = $state<TasksRead | null>(null);
  let tasksLoading = $state(false);
  let workflowsLoading = $state(true);
  let startTarget = $state('');

  let workflowsApiAvailable = $state<boolean | null>(null);
  /// Non-null when the registry or jobs reads behind the workflows
  /// tab FAILED — rendered instead of "No HR workflows / No active
  /// workflows", which are claims only successful reads get to make
  /// (packet 3fba9c35, the false-empty sweep).
  let workflowsFailed = $state<string | null>(null);

  async function fetchHrKinds(): Promise<void> {
    // /api/workflows is the canonical Workflow list. The HR
    // workflows are the kinds whose `surfaces` hint includes 'hr'
    // (and that are about an Employee). Discovering them keeps the
    // SPA tenant-agnostic — no brewery slugs baked into HR.
    try {
      const r = await fetch('/api/workflows');
      if (!r.ok) {
        workflowsFailed = `workflows registry: HTTP ${r.status}`;
        return;
      }
      const all = (await r.json()) as WorkflowSpec[];
      hrKinds = all
        .filter(
          (k) =>
            workflowSurfaces(k).includes('hr') &&
            k.subject_kinds.includes('employee'),
        )
        .map((k) => ({ kind: k.kind, label: k.label }));
      workflowsFailed = null;
    } catch (e) {
      workflowsFailed = e instanceof Error ? e.message : String(e);
    }
  }

  async function fetchWorkflows(): Promise<void> {
    // #101 — Active workflows = open Jobs of the discovered HR
    // kinds, grouped by Subject (Employee). We query
    // /api/jobs?kind={k}&status=open for each kind, then count
    // steps via /api/jobs/{id}/steps.
    try {
      const results: ActiveWorkflow[] = [];
      for (const { kind, label } of hrKinds) {
        // `hrJobRows` owns the envelope and the subject read, and it
        // THROWS on a shape it does not recognise — so a wrong key
        // lands here as a failure instead of as an empty department.
        // See ./hr-tasks for the two it was getting wrong.
        const res = await fetchRemote(
          `/api/jobs?kind=${encodeURIComponent(kind)}&status=open&limit=200`,
          hrJobRows,
        );
        if (res.kind === 'failed') {
          // A failed kind is a failed list — skipping it would render
          // the remainder as if it were everything.
          workflowsFailed = `${kind} jobs: ${res.error}`;
          continue;
        }
        for (const j of res.data) {
          // `null` when the step read failed — the progress column says
          // "unknown" rather than drawing a 0% bar over a read that
          // never landed.
          const progress = await fetchStepProgress(j.id);
          const empMatch = roster.find((e) => e.id === j.employeeId);
          results.push({
            employee_id: j.employeeId,
            employee_name: empMatch?.name ?? j.employeeId,
            workflow: label,
            job_id: j.id,
            total_tasks: progress?.total ?? null,
            done_tasks: progress?.done ?? null,
          });
        }
      }
      workflows = results;
      workflowsApiAvailable = true;
    } catch {
      workflowsApiAvailable = false;
    }
    workflowsLoading = false;
  }

  $effect(() => {
    if (tab === 'workflows') {
      workflowsLoading = true;
      void (async () => {
        await fetchHrKinds();
        await fetchWorkflows();
      })();
    }
  });

  async function loadTasks(empId: string): Promise<void> {
    // #101 — Tasks = Steps of the employee's open HR Job. Pick
    // the most recent matching Job and fetch its Steps. Lower
    // resolution than the prior /api/people/{id}/tasks endpoint
    // (which surfaced the per-employee task aggregation across
    // all workflows) but accurate against the Job model. A
    // future enhancement could merge multiple Jobs' steps.
    //
    // The read itself lives in ./hr-tasks — one `TasksRead`, three
    // outcomes kept apart. See that module for why.
    selectedEmp = empId;
    tasksLoading = true;
    tasks = await fetchEmployeeTasks(empId, workflows);
    tasksLoading = false;
  }

  async function updateTask(taskId: string, status: string): Promise<void> {
    // #101 — Step transitions go through PUT /api/jobs/{job}/steps/{step}.
    const w = workflows.find((x) => x.employee_id === selectedEmp);
    if (!w) return;
    await fetch(`/api/jobs/${w.job_id}/steps/${taskId}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ status }),
    });
    if (selectedEmp) await loadTasks(selectedEmp);
    await fetchWorkflows();
  }

  function startWorkflow(kind: string): void {
    if (!startTarget) return;
    // #101 — Route to the canonical Job-creation flow. JobsList
    // picks the kind, prepopulates the Subject, and the operator
    // confirms / overrides before the Job opens. Same path
    // operators use for every other workflow in BOSS. `kind` is a
    // discovered HR Workflow (surfaces ⊇ ['hr']).
    const url = `/jobs?new=1&kind=${encodeURIComponent(kind)}&subject_kind=employee&subject_id=${encodeURIComponent(startTarget)}`;
    window.location.href = url;
  }

  let activeRoster = $derived(
    roster.filter((e) => e.status === 'active' || e.status === 'on-leave'),
  );
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="HR admin"
    title={`${active.length} active employees`}
    subtitle={`${onLeave.length} on leave · 0 open reqs (0 headcount) · ${expiring90.length} certs expiring in 90d`}
  />

  <nav class="tabs" role="tablist">
    {#each TABS as t (t.id)}
      <button
        type="button"
        role="tab"
        aria-selected={tab === t.id}
        class="tab {tab === t.id ? 'tab-active' : ''}"
        onclick={() => (tab = t.id)}
      >
        {t.label}
      </button>
    {/each}
  </nav>

  <div class="tab-panel" style="padding:0 32px 32px">
    {#if loading}
      <p class="empty">Loading…</p>
    {:else if rosterFailed && tab !== 'workflows' && tab !== 'requisitions'}
      <!-- Overview, certs and headcount all derive from the roster —
           with the roster fetch failed their zeros would be fiction. -->
      <p class="empty load-failed" role="alert">
        Couldn't load the roster — {rosterFailed}
      </p>
    {:else if tab === 'overview'}
      <div class="tab-grid">
        <Section title="At a glance">
            <dl class="kv">
              <dt>Total headcount</dt><dd class="num">{roster.length}</dd>
              <dt>Active</dt><dd class="num">{active.length}</dd>
              <dt>On leave</dt><dd class="num">{onLeave.length}</dd>
              <dt>Contractors</dt><dd class="num">{roster.filter((e) => e.employment_type === 'contractor').length}</dd>
              <dt>Avg tenure</dt><dd>{avgTenure} years</dd>
              <dt>Open requisitions</dt><dd class="num">0</dd>
            </dl>
        </Section>

        <Section title="Urgent">
            {#if expiring30.length === 0}
              <p class="empty">Nothing urgent today.</p>
            {:else}
              <div style="margin-bottom:12px">
                <h4 style="font-size:13px; font-weight:600; color:#dc2626; margin:0 0 4px">
                  {expiring30.length} cert{expiring30.length > 1 ? 's' : ''} expiring in 30 days
                </h4>
                {#each expiring30.slice(0, 5) as { employee, cert } (`${employee.id}-${cert.name}`)}
                  <div style="font-size:13px">
                    <Link to={entityHref('employee', employee.id)}>
                      {employee.name}
                    </Link> — {cert.name} ({cert.expires_on})
                  </div>
                {/each}
              </div>
            {/if}
        </Section>
      </div>
    {:else if tab === 'workflows'}
      <div>
        <Section title="Start Workflow">
            {#if workflowsFailed && hrKinds.length === 0}
              <p class="load-failed" role="alert" style="font-size:13px">
                Couldn't load HR workflows — {workflowsFailed}
              </p>
            {:else if hrKinds.length === 0}
              <p style="color:#78716c; font-size:13px">
                No HR workflows are published in this deployment.
                Workflows appear here once they declare
                <code>metadata.surfaces ⊇ ["hr"]</code>.
              </p>
            {:else}
              <div style="display:flex; gap:8px; align-items:center; flex-wrap:wrap">
                <select bind:value={startTarget} class="hr-select" style="min-width:200px">
                  <option value="">Select employee...</option>
                  {#each activeRoster as e (e.id)}
                    <option value={e.id}>{e.name} ({e.id})</option>
                  {/each}
                </select>
                {#each hrKinds as k (k.kind)}
                  <button
                    class="hr-action-btn"
                    onclick={() => startWorkflow(k.kind)}
                    disabled={!startTarget}
                  >
                    Start {k.label}
                  </button>
                {/each}
              </div>
            {/if}
        </Section>

        <Section title="Active Workflows">
            {#if workflowsLoading}
              <p style="color:#78716c; font-size:13px">Loading...</p>
            {:else if workflowsFailed}
              <p class="load-failed" role="alert" style="font-size:13px">
                Couldn't load active workflows — {workflowsFailed}
              </p>
            {:else if workflowsApiAvailable === false}
              <p style="color:#78716c; font-size:13px">
                Active-workflows list is not yet wired in this deployment.
                The per-employee <code>onboard</code> / <code>offboard</code>
                writes above work, but the cross-employee aggregation endpoint
                (<code>GET /api/people/workflows</code>) hasn't been
                implemented yet.
              </p>
            {:else if workflows.length === 0}
              <p style="color:#78716c; font-size:13px">No active workflows.</p>
            {:else}
              <table class="data-table">
                <thead>
                  <tr>
                    <th>Employee</th>
                    <th>Workflow</th>
                    <th>Progress</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {#each workflows as w (`${w.employee_id}-${w.workflow}`)}
                    {@const counted = w.total_tasks !== null && w.done_tasks !== null}
                    {@const pct =
                      w.total_tasks !== null && w.done_tasks !== null && w.total_tasks > 0
                        ? Math.round((w.done_tasks / w.total_tasks) * 100)
                        : 0}
                    <tr>
                      <td>
                        <Link to={entityHref('employee', w.employee_id)}>
                          {w.employee_name}
                        </Link>
                      </td>
                      <td>
                        <span class="chip">
                          {w.workflow}
                        </span>
                      </td>
                      <td>
                        {#if counted}
                          <div class="hr-progress">
                            <div class="hr-progress-bar" style={`width:${pct}%`}></div>
                          </div>
                          <span style="font-size:11px; color:#78716c">
                            {w.done_tasks}/{w.total_tasks} tasks ({pct}%)
                          </span>
                        {:else}
                          <span class="load-failed" style="font-size:11px">
                            Progress unknown — this Job's steps could not be read.
                          </span>
                        {/if}
                      </td>
                      <td>
                        <button class="hr-detail-btn" onclick={() => loadTasks(w.employee_id)}>
                          View tasks
                        </button>
                      </td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            {/if}
        </Section>

        <!-- Three outcomes, three different things on screen. The old
             guard was `selectedEmp && tasks.length > 0`, which hid the
             section for a failed read exactly as it hid it for an
             employee with no tasks — so an outage read as a finished
             onboarding. -->
        {#if selectedEmp && tasks !== null}
          {@const empName = roster.find((e) => e.id === selectedEmp)?.name ?? selectedEmp}
          <Section title={`Tasks — ${empName}`}>
            {#if tasksLoading}
              <p style="color:#78716c; font-size:13px">Loading…</p>
            {:else if tasks.kind === 'failed'}
              <p class="load-failed" role="alert" style="font-size:13px">
                Couldn't read {empName}'s onboarding steps — {tasks.error}.
                This is a failed read, not an empty onboarding: the tasks
                below this line are unknown, not absent.
              </p>
            {:else if tasks.kind === 'no-job'}
              <p style="color:#78716c; font-size:13px">
                {empName} has no open HR workflow in the list above, so
                there are no steps to show. Start one from
                <strong>Start Workflow</strong> above.
              </p>
            {:else if tasks.data.length === 0}
              <p style="color:#78716c; font-size:13px">
                This workflow has no steps — the Job was read and it is
                genuinely empty.
              </p>
            {:else}
              <table class="data-table">
                <thead>
                  <tr>
                    <th>Category</th>
                    <th>Task</th>
                    <th>Status</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {#each tasks.data as t (t.id)}
                    <tr>
                      <td><span class="chip">{CATEGORY_LABEL[t.category] ?? t.category}</span></td>
                      <td>{t.task}</td>
                      <td><span class="chip chip-task-{t.status}">{t.status}</span></td>
                      <td>
                        {#if t.status !== 'completed'}
                          <button class="hr-done-btn" onclick={() => updateTask(t.id, 'completed')}>
                            Mark done
                          </button>
                        {/if}
                      </td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            {/if}
          </Section>
        {/if}
      </div>
    {:else if tab === 'requisitions'}
      <div class="tab-grid">
        <Section title="Requisitions" wide>
            <p class="empty">
              Requisition data will be available once the requisitions API is implemented.
            </p>
        </Section>
      </div>
    {:else if tab === 'certs'}
      <div class="tab-grid">
        <Section title={`Expiring in 90 days (${expiring90.length})`} wide>
            {#if expiring90.length === 0}
              <p class="empty">No certifications expiring in the next 90 days.</p>
            {:else}
              <table class="data-table data-table-striped">
                <thead>
                  <tr>
                    <th>Employee</th>
                    <th>Certification</th>
                    <th>Issuer</th>
                    <th>Expires</th>
                    <th>Days left</th>
                  </tr>
                </thead>
                <tbody>
                  {#each expiring90 as { employee, cert } (`${employee.id}-${cert.name}`)}
                    {@const daysLeft = cert.expires_on
                      ? Math.ceil(
                          (new Date(cert.expires_on).getTime() - appNow().getTime()) /
                            (1000 * 60 * 60 * 24),
                        )
                      : null}
                    <tr>
                      <td>
                        <Link to={entityHref('employee', employee.id)}>
                          {employee.name}
                        </Link>
                      </td>
                      <td>{cert.name}</td>
                      <td>{cert.issuing_body}</td>
                      <td>{cert.expires_on ?? '—'}</td>
                      <td class="num">
                        {#if daysLeft !== null && daysLeft <= 30}
                          <span style="color:#dc2626; font-weight:600">{daysLeft}d</span>
                        {:else}
                          <span>{daysLeft}d</span>
                        {/if}
                      </td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            {/if}
        </Section>
      </div>
    {:else if tab === 'headcount'}
      <div class="tab-grid">
        <Section title="Headcount by department" wide>
            <table class="data-table data-table-striped">
              <thead>
                <tr>
                  <th>Department</th>
                  <th class="num">Active</th>
                  <th class="num">On leave</th>
                  <th class="num">Open reqs</th>
                  <th class="num">Target</th>
                </tr>
              </thead>
              <tbody>
                {#each byDept as [dept, counts] (dept)}
                  <tr>
                    <td>{humanizeClassCode(dept)}</td>
                    <td class="num">{counts.active}</td>
                    <td class="num">{counts.onLeave || '—'}</td>
                    <td class="num">{counts.openReqs || '—'}</td>
                    <td class="num">{counts.active + counts.openReqs}</td>
                  </tr>
                {/each}
                <tr style="font-weight:600">
                  <td>Total</td>
                  <td class="num">{active.length}</td>
                  <td class="num">{onLeave.length}</td>
                  <td class="num">0</td>
                  <td class="num">{active.length}</td>
                </tr>
              </tbody>
            </table>
        </Section>
      </div>
    {/if}
  </div>
</div>
