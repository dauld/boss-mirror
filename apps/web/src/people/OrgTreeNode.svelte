<script lang="ts">
  // Recursive node for the org-chart tree view. Renders one
  // employee as a card and nests their direct reports below.
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { humanizeClassCode, type Employee } from './types';
  import { href } from '../router';
  import { entityHref } from '@boss/web-kit/ui/entity-href';

  type Props = {
    employee: Employee;
    childrenByManager: Map<string, Employee[]>;
    depth?: number;
  };

  let { employee, childrenByManager, depth = 0 }: Props = $props();

  let directs = $derived(childrenByManager.get(employee.id) ?? []);
</script>

<div class="org-node" style:--depth={depth}>
  <div class="org-card">
    <Link to={entityHref('employee', employee.id)}>
      {employee.name}
    </Link>
    <div class="org-role">{humanizeClassCode(employee.role)}</div>
    {#if directs.length > 0}
      <div class="org-meta">
        {directs.length} report{directs.length === 1 ? '' : 's'}
      </div>
    {/if}
  </div>

  {#if directs.length > 0}
    <ul class="org-children">
      {#each directs as child (child.id)}
        <li>
          <svelte:self
            employee={child}
            {childrenByManager}
            depth={depth + 1}
          />
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .org-node {
    --indent: calc(var(--depth, 0) * 1.5rem);
  }

  .org-card {
    display: inline-flex;
    flex-direction: column;
    gap: 0.125rem;
    padding: 0.5rem 0.85rem;
    border: 1px solid var(--hairline);
    border-radius: 0.5rem;
    background: var(--ink);
    min-width: 14rem;
  }

  .org-role {
    font-size: 0.85rem;
    color: var(--static);
  }

  .org-meta {
    font-size: 0.75rem;
    color: var(--static);
  }

  .org-children {
    list-style: none;
    margin: 0.4rem 0 0 0;
    padding: 0 0 0 1.25rem;
    border-left: 1px dashed var(--hairline);
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .org-children > li {
    margin: 0;
  }
</style>
