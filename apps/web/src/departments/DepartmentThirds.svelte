<script lang="ts">
  // A department's packets as three thirds — in / working / out — over
  // the one department read (department.ts). Lifted out of
  // DepartmentJobsPage (cc76f755) so a department's OWN surface can
  // carry the same view: /ux/parts made four reads and none was a jobs
  // read, so a warehouse packet could never appear on the warehouse's
  // page (backlog 044dffa1, page audit 63d810aa, 2026-09-23). One
  // component, so the two renders cannot drift apart.
  //
  // NO NUMBER THIS SURFACE MAKES UP. The department's kinds are the
  // server's join (`?department=<code>`); a failed read is a failure,
  // never an empty department; a page smaller than `total` says so.
  import { onMount } from 'svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import { departmentLabel } from '@boss/web-kit/nav';
  import { departments } from '@boss/web-kit/session/departments.svelte';
  import { href, navigate } from '../router';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { rowLink } from '@boss/web-kit/ui/RowLink';
  import { shortId } from '../data/ids';
  import type { Remote } from '../data/remote';
  import { subjectLabel, subjectPath, type Job } from '../jobs/types';
  import {
    OUT_WINDOW_DAYS,
    PAGE,
    THIRD_LABEL,
    loadDepartment,
    thirds,
    waitingAt,
    type JobsPage,
    type Third,
  } from './department';

  let { code } = $props<{ code: string }>();

  let page = $state<Remote<JobsPage>>({ kind: 'loading' });

  async function refresh(c: string): Promise<void> {
    page = { kind: 'loading' };
    page = await loadDepartment(c);
  }

  // Re-read when the tab changes department: the component is reused
  // across /ux/departments/<code> routes, so the prop moves under it.
  $effect(() => {
    void refresh(code);
  });

  onMount(() => {
    const t = setInterval(() => void refresh(code), 60_000);
    return () => clearInterval(t);
  });

  const label = $derived(departmentLabel(code, departments()));
  const rows = $derived<ReadonlyArray<Job>>(page.kind === 'ready' ? page.data.rows : []);
  const split = $derived(thirds(rows));
  const truncated = $derived(page.kind === 'ready' && page.data.total > page.data.rows.length);

  const THIRDS: ReadonlyArray<Readonly<{ id: Third; note: string }>> = [
    { id: 'in', note: 'open, nothing done yet — standing at the first step' },
    { id: 'working', note: 'a step is active, or one is done and the next waits' },
    { id: 'out', note: `closed in the last ${OUT_WINDOW_DAYS} days` },
  ];
</script>

{#if page.kind === 'loading'}
  <p class="empty">Loading…</p>
{:else if page.kind === 'failed'}
  <!-- `load-failed` is the one marker the outage crawl reads
       (tests/mocked/_routes.ts): a failed read is a failure line,
       never an empty department. -->
  <p class="empty load-failed">Couldn't load this department's jobs: {page.error}</p>
{:else if rows.length === 0}
  <p class="empty">
    No jobs in {label}: no packet of a kind whose workflow declares this department is live or
    closed in the last {OUT_WINDOW_DAYS} days.
  </p>
{:else}
  {#if truncated}
    <p class="empty">
      Showing {rows.length} of {page.data.total} — this read is one page of {PAGE}; the
      thirds below are of the page, not of the department.
    </p>
  {/if}
  {#each THIRDS as t (t.id)}
    {@const list = split[t.id]}
    <section class="list-section">
      <h3>{THIRD_LABEL[t.id]} <span class="mono">({list.length})</span></h3>
      <p class="empty">{t.note}</p>
      {#if list.length > 0}
        {@render table(list)}
      {/if}
    </section>
  {/each}
{/if}

{#snippet table(list: ReadonlyArray<Job>)}
  <!-- The jobs list's own table, column for column, so a department
       reads its packets the way All jobs shows them — plus "Waiting
       at", the step a live packet stands at, because "open" alone let
       a payout sit at its post step for 2.6 days unsaid (4d4dc204). -->
  <table class="data-table data-table-striped">
    <thead>
      <tr>
        <th>ID</th>
        <th>Kind</th>
        <th>Title</th>
        <th>Subject</th>
        <th>Status</th>
        <th>Waiting at</th>
        <th>Priority</th>
        <th>Opened</th>
        <th>Closed</th>
      </tr>
    </thead>
    <tbody>
      {#each list as j (j.id)}
        <tr use:rowLink={{ onActivate: () => navigate(entityHref('job', j.id)), label: j.title }}>
          <td class="mono">
            <Link to={entityHref('job', j.id)}>{shortId(j.id)}</Link>
          </td>
          <td>{j.kind}</td>
          <td>{j.title}</td>
          <td class="mono">
            <Link to={href(subjectPath(j.subject))}>{subjectLabel(j.subject)}</Link>
          </td>
          <td>{j.status}</td>
          <td class="waiting-at">{waitingAt(j)}</td>
          <td>{j.priority}</td>
          <td>{j.opened_on}</td>
          <td>{j.closed_on ?? ''}</td>
        </tr>
      {/each}
    </tbody>
  </table>
{/snippet}
