<script lang="ts">
  // Roster list — port of apps/web/src/people/PeopleList.tsx.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import FilterGroup from '@boss/web-kit/ui/FilterGroup.svelte';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import SearchInput from '@boss/web-kit/ui/SearchInput.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import StatusChip from '@boss/web-kit/ui/StatusChip.svelte';
  import SortHeader from '@boss/web-kit/ui/SortHeader.svelte';
  import { createSortState } from '@boss/web-kit/ui/sort-state.svelte';
  import OrgTreeNode from './OrgTreeNode.svelte';
  import {
    employmentTone,
    humanizeClassCode,
    type Department,
    type Employee,
    type EmploymentStatus,
  } from './types';
  import { expiringCerts, tenureYears } from './utils';
  import { href } from '../router';

  type DeptFilter = Department | 'all';
  type StatusFilter = EmploymentStatus | 'all';

  let roster = $state<Employee[]>([]);
  /// Non-null when the roster load failed — rendered instead of the
  /// empty state, so an outage never reads as "no employees" (packet
  /// 3fba9c35, the false-empty sweep).
  let loadFailed = $state<string | null>(null);
  let loading = $state(true);
  let dept = $state<DeptFilter>('all');
  let status = $state<StatusFilter>('active');
  let query = $state('');

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const r = await fetch('/api/people');
        if (!r.ok) throw new Error(`people HTTP ${r.status}`);
        const body = (await r.json()) as Employee[];
        if (!cancelled) {
          roster = body;
          loadFailed = null;
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

  let activeRoster = $derived(roster.filter((e) => e.status === 'active'));

  let headcountByDept = $derived.by(() => {
    const m = new Map<Department, number>();
    for (const e of activeRoster)
      if (e.department) m.set(e.department, (m.get(e.department) ?? 0) + 1);
    return m;
  });

  let expiring90 = $derived(expiringCerts(90, roster));

  let visible = $derived(
    roster.filter((e) => {
      if (status !== 'all' && e.status !== status) return false;
      if (dept !== 'all' && e.department !== dept) return false;
      if (query) {
        const q = query.toLowerCase();
        const hay = `${e.id} ${e.name} ${e.email} ${humanizeClassCode(e.role)}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    }),
  );

  // Every column clickable (CAR-4). The department accessor keeps the
  // page's historic compound order — department, then name within it —
  // so the landing view is unchanged and the Department column sorts
  // the way a roster reads.
  type SortKey =
    | 'id'
    | 'name'
    | 'role'
    | 'dept'
    | 'tenure'
    | 'skills'
    | 'location'
    | 'status';
  const DESC_FIRST: ReadonlyArray<SortKey> = ['tenure', 'skills'];
  const sort = createSortState<SortKey>({ key: 'dept', dir: 'asc' }, (k) =>
    DESC_FIRST.includes(k) ? 'desc' : 'asc',
  );
  let sortedVisible = $derived(
    sort.sorted(visible, {
      id: (e) => e.id,
      name: (e) => e.name,
      role: (e) => humanizeClassCode(e.role),
      dept: (e) => `${e.department ?? ''} ${e.name ?? ''}`,
      tenure: (e) => tenureYears(e),
      skills: (e) => e.skills.length,
      location: (e) => e.location,
      status: (e) => e.status,
    }),
  );

  let DEPTS = $derived(
    Array.from(
      new Set(
        activeRoster
          .map((e) => e.department)
          .filter((d): d is Department => d !== null),
      ),
    ).sort(),
  );

  // Tree view — group employees by manager_id so the hierarchy
  // can render as nested cards. Roots are employees with no
  // manager (the CEO and anyone whose manager isn't in the
  // visible roster).
  type ViewMode = 'list' | 'tree';
  let viewMode = $state<ViewMode>('list');

  let activeById = $derived(new Map(activeRoster.map((e) => [e.id, e])));
  let childrenByManager = $derived.by(() => {
    const m = new Map<string, Employee[]>();
    for (const e of activeRoster) {
      if (!e.manager_id) continue;
      if (!activeById.has(e.manager_id)) continue;
      const bucket = m.get(e.manager_id) ?? [];
      bucket.push(e);
      m.set(e.manager_id, bucket);
    }
    for (const arr of m.values()) {
      arr.sort((a, b) => (a.name ?? "").localeCompare(b.name ?? ""));
    }
    return m;
  });
  let treeRoots = $derived(
    activeRoster
      .filter((e) => !e.manager_id || !activeById.has(e.manager_id))
      .sort((a, b) => (a.name ?? "").localeCompare(b.name ?? "")),
  );
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="People"
    title={`${activeRoster.length} active employees`}
    subtitle={`${expiring90.length} certifications expiring in 90 days`}
  />

  <div class="catalog-layout">
    <aside class="catalog-filters">
      <FilterGroup label="View">
          <FilterButton active={viewMode === 'list'} onclick={() => (viewMode = 'list')}>
            List
          </FilterButton>
          <FilterButton active={viewMode === 'tree'} onclick={() => (viewMode = 'tree')}>
            Hierarchy
          </FilterButton>
      </FilterGroup>

      <FilterGroup label="Search">
          <SearchInput bind:value={query} placeholder="Name, email, role…" />
      </FilterGroup>

      <FilterGroup label="Status">
          <FilterButton active={status === 'active'} onclick={() => (status = 'active')}>
              Active ({roster.filter((e) => e.status === 'active').length})
          </FilterButton>
          <FilterButton active={status === 'on-leave'} onclick={() => (status = 'on-leave')}>
              On leave ({roster.filter((e) => e.status === 'on-leave').length})
          </FilterButton>
          <FilterButton active={status === 'all'} onclick={() => (status = 'all')}>
            All ({roster.length})
          </FilterButton>
      </FilterGroup>

      <FilterGroup label="Department">
          <FilterButton active={dept === 'all'} onclick={() => (dept = 'all')}>
            All ({activeRoster.length})
          </FilterButton>
          {#each DEPTS as d (d)}
            <FilterButton active={dept === d} onclick={() => (dept = d)}>
                {humanizeClassCode(d)} ({headcountByDept.get(d) ?? 0})
            </FilterButton>
          {/each}
      </FilterGroup>
    </aside>

    <section class="list-section">
      {#if loading}
        <p class="empty">Loading…</p>
      {:else if loadFailed}
        <p class="empty load-failed" role="alert">
          Couldn't load the roster — {loadFailed}
        </p>
      {:else if viewMode === 'tree'}
        {#if treeRoots.length === 0}
          <p class="empty">No leadership rooted org chart yet.</p>
        {:else}
          <ul class="org-tree">
            {#each treeRoots as root (root.id)}
              <li>
                <OrgTreeNode
                  employee={root}
                  childrenByManager={childrenByManager}
                />
              </li>
            {/each}
          </ul>
        {/if}
      {:else if visible.length === 0}
        <p class="empty">No employees match those filters.</p>
      {:else}
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <SortHeader {sort} key="id">BOSS ID</SortHeader>
              <SortHeader {sort} key="name">Name</SortHeader>
              <SortHeader {sort} key="role">Role</SortHeader>
              <SortHeader {sort} key="dept">Department</SortHeader>
              <SortHeader {sort} key="tenure" num={true}>Tenure</SortHeader>
              <SortHeader {sort} key="skills" num={true}>Skills</SortHeader>
              <SortHeader {sort} key="location">Location</SortHeader>
              <SortHeader {sort} key="status">Status</SortHeader>
            </tr>
          </thead>
          <tbody>
            {#each sortedVisible as e (e.id)}
              <tr class="data-table-row-link">
                <td class="mono">
                  <Link to={entityHref('employee', e.id)}>
                    {e.id}
                  </Link>
                </td>
                <td>{e.name}</td>
                <td class="prose-cell">{humanizeClassCode(e.role)}</td>
                <td>{humanizeClassCode(e.department)}</td>
                <td class="num">{tenureYears(e).toFixed(1)}y</td>
                <td class="num">{e.skills.length}</td>
                <td>{e.location}</td>
                <td><StatusChip value={e.status ?? 'unknown'} tone={employmentTone(e.status)} /></td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  </div>
</div>

<style>
  .org-tree {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .org-tree > li {
    margin: 0;
  }
</style>
