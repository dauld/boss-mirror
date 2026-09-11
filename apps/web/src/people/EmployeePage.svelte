<script lang="ts">
  // Employee detail — port of apps/web/src/people/EmployeePage.tsx.

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import Meta from '@boss/web-kit/ui/Meta.svelte';
  import { appNow } from '@boss/web-kit/sim-clock';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import StatusChip from '@boss/web-kit/ui/StatusChip.svelte';
  import FileAttachments from '../content/FileAttachments.svelte';
  import CalendarFeedSection from './CalendarFeedSection.svelte';
  import { employmentTone, humanizeClassCode, type Employee } from './types';
  import { directReports, tenureYears } from './utils';
  import { href } from '../router';

  let { empId } = $props<{ empId: string }>();

  let employee = $state<Employee | null>(null);
  let allEmployees = $state<Employee[]>([]);
  /// Non-null when the record fetch FAILED (5xx / network) — rendered
  /// instead of "Employee not found", which is a claim only a
  /// successful lookup gets to make (packet 3fba9c35).
  let loadFailed = $state<string | null>(null);
  let loading = $state(true);

  $effect(() => {
    const id = empId;
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const [eResp, rosterResp] = await Promise.all([
          fetch(`/api/people/${encodeURIComponent(id)}`),
          fetch('/api/people'),
        ]);
        if (!cancelled) {
          if (eResp.ok) {
            employee = (await eResp.json()) as Employee;
            loadFailed = null;
          } else if (eResp.status === 404) {
            employee = null;
            loadFailed = null;
          } else {
            employee = null;
            loadFailed = `HTTP ${eResp.status}`;
          }
          allEmployees = rosterResp.ok ? ((await rosterResp.json()) as Employee[]) : [];
          loading = false;
        }
      } catch (e) {
        if (!cancelled) {
          loadFailed = e instanceof Error ? e.message : String(e);
          loading = false;
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let employeeById = $derived.by(() => {
    const m = new Map<string, Employee>();
    for (const e of allEmployees) m.set(e.id, e);
    return m;
  });

  let reports = $derived(
    employee ? directReports(empId, allEmployees) : [],
  );
  let tenure = $derived(employee ? tenureYears(employee) : 0);

  let chain = $derived.by(() => {
    if (!employee) return [] as Employee[];
    const list: Employee[] = [];
    let current = employee.manager_id
      ? employeeById.get(employee.manager_id)
      : undefined;
    while (current) {
      list.push(current);
      current = current.manager_id
        ? employeeById.get(current.manager_id)
        : undefined;
    }
    return list;
  });

  type CertState = 'ok' | 'expiring' | 'critical';
  function certState(expiresOn: string): CertState {
    const days =
      (new Date(expiresOn).getTime() - appNow().getTime()) /
      (1000 * 60 * 60 * 24);
    if (days < 30) return 'critical';
    if (days < 90) return 'expiring';
    return 'ok';
  }
  function certLabel(s: CertState): string {
    return s === 'critical' ? '< 30d' : s === 'expiring' ? '< 90d' : 'valid';
  }

  function isFieldServiceRole(role: Employee['role']): boolean {
    return role === 'service-tech' || role === 'service-mgr';
  }
</script>

{#if loading}
  <div class="catalog theme-exec">
    <p class="empty">Loading employee…</p>
  </div>
{:else if loadFailed}
  <div class="catalog theme-exec">
    <p class="empty load-failed" role="alert">
      Couldn't load this employee — {loadFailed}
    </p>
  </div>
{:else if !employee}
  <div class="catalog theme-exec">
    <div class="exec-header">
      <h1 class="exec-title">Employee not found</h1>
    </div>
    <p class="empty">No employee with id <code>{empId}</code>.</p>
  </div>
{:else}
  {@const e = employee}
  <div class="detail-page theme-exec">
    <Breadcrumb to={href('/ux/people')}>
      ← All employees
    </Breadcrumb>

    <header class="detail-hero">
      <div>
        <div class="detail-eyebrow">
          <EntityLink kind="employee" id={e.id} /> · {humanizeClassCode(e.department)} ·
          <StatusChip value={e.status ?? 'unknown'} tone={employmentTone(e.status)} />
        </div>
        <h1 class="detail-title">{e.name}</h1>
        <div class="detail-tagline">{humanizeClassCode(e.role)} · {e.email}</div>
        <div class="detail-meta">
          <Meta label="Tenure">{tenure.toFixed(1)} years</Meta>
          <Meta label="Skill level">
              {e.skill_level !== null ? `${e.skill_level}/5` : '—'}
          </Meta>
          <Meta label="Direct reports">{reports.length}</Meta>
          <Meta label="Location">{e.location}</Meta>
        </div>
      </div>
    </header>

    <div class="subject-actions">
      <a
        class="action-btn"
        href={href(`/ux/jobs?new=1&subject_kind=employee&subject_id=${encodeURIComponent(e.id)}`)}
      >
        + Create a Job for this employee
      </a>
    </div>

    <div class="tab-grid">
      <Section title="Profile">
          <dl class="kv">
            <dt>BOSS ID</dt><dd><EntityLink kind="employee" id={e.id} /></dd>
            <dt>Email</dt><dd>{e.email}</dd>
            <dt>Hire date</dt><dd>{e.hire_date}</dd>
            <dt>Employment type</dt><dd>{e.employment_type ? e.employment_type.replace(/-/g, ' ') : '—'}</dd>
            <dt>Location</dt><dd>{e.location}</dd>
            <dt>Status</dt><dd><StatusChip value={e.status ?? 'unknown'} tone={employmentTone(e.status)} /></dd>
          </dl>
      </Section>

      <Section title="Reporting chain">
          {#if chain.length === 0}
            <p class="empty">No manager — reports to board.</p>
          {:else}
            <ol class="checklist" style="padding-left:0; list-style:none">
              {#each chain as m (m.id)}
                <li>
                  →
                  <Link to={entityHref('employee', m.id)}>
                    {m.name}
                  </Link>
                  <span style="color:#78716c"> · {humanizeClassCode(m.role)}</span>
                </li>
              {/each}
            </ol>
          {/if}
      </Section>

      {#if reports.length > 0}
        <Section
          title={`Team (${reports.length} direct report${reports.length === 1 ? '' : 's'})`}
          wide
        >
            <table class="data-table">
              <thead>
                <tr><th>ID</th><th>Name</th><th>Role</th><th>Tenure</th></tr>
              </thead>
              <tbody>
                {#each reports as r (r.id)}
                  <tr>
                    <td class="mono"><EntityLink kind="employee" id={r.id} /></td>
                    <td>
                      <EntityLink kind="employee" id={r.id} label={r.name} mono={false} />
                    </td>
                    <td class="prose-cell">{humanizeClassCode(r.role)}</td>
                    <td class="num">{tenureYears(r).toFixed(1)}y</td>
                  </tr>
                {/each}
              </tbody>
            </table>
        </Section>
      {/if}

      {#if e.skills.length > 0}
        <Section title="Skills" wide>
            <div class="chips">
              {#each e.skills as s (s)}
                <span class="chip">{s}</span>
              {/each}
            </div>
        </Section>
      {/if}

      {#if e.certifications.length > 0}
        <Section title="Certifications" wide>
            <table class="data-table">
              <thead>
                <tr>
                  <th>Certification</th>
                  <th>Issuer</th>
                  <th>Issued</th>
                  <th>Expires</th>
                  <th>Status</th>
                </tr>
              </thead>
              <tbody>
                {#each e.certifications as c, i (i)}
                  {@const st = c.expires_on ? certState(c.expires_on) : 'ok'}
                  <tr>
                    <td>{c.name}</td>
                    <td class="prose-cell">{c.issuing_body}</td>
                    <td>{c.issued_on}</td>
                    <td>{c.expires_on ?? 'does not expire'}</td>
                    <td>
                      <span class="chip chip-cert chip-cert-{st}">
                        {certLabel(st)}
                      </span>
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
        </Section>
      {/if}

      <Section title="Job assignments" wide>
          <p class="prose">
            View this employee's owned jobs in the
            <Link to={href(`/ux/jobs?owner_id=${encodeURIComponent(e.id)}&status=`)}>
              Jobs list
            </Link>.
          </p>
      </Section>

      {#if isFieldServiceRole(e.role)}
        <CalendarFeedSection empId={e.id} />
      {/if}

      <Section title="Attachments" wide>
        <FileAttachments targetKind="subject" targetId={e.id} />
      </Section>
    </div>
  </div>
{/if}
