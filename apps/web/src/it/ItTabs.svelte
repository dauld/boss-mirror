<script lang="ts">
  // The IT family tab strip (2026-08-31 consolidation, 1f6d55e0):
  // Operate / Registry / Design each carry their family as tabs on
  // one surface instead of sidebar rows. One component, data-driven —
  // adding a tab is a row here, not a new sidebar entry (the sidebar
  // holds exactly six IT rows and this file is why it can).
  import { href, navigate } from '../router';

  export type ItTabGroup = 'operate' | 'registry' | 'design';

  const GROUPS: Readonly<Record<ItTabGroup, ReadonlyArray<{ label: string; path: string }>>> = {
    operate: [
      { label: 'Incidents', path: '/it/operate' },
      { label: 'Yard status', path: '/it/operate/yard-status' },
      { label: 'Audit Log', path: '/it/operate/audit' },
      { label: 'Performance', path: '/it/operate/perf' },
      { label: 'Atlas', path: '/it/operate/atlas' },
      { label: 'Bottlenecks', path: '/it/operate/bottlenecks' },
    ],
    registry: [
      { label: 'Workflows', path: '/it/registry' },
      { label: 'Dispatcher', path: '/it/registry/dispatcher' },
      { label: 'Step plugins', path: '/it/registry/step-plugins' },
      { label: 'Policy', path: '/it/registry/policy' },
      { label: 'Subjects', path: '/it/registry/subjects' },
    ],
    design: [
      { label: 'Reviews', path: '/it/design' },
      { label: 'Experiments', path: '/it/design/experiments' },
      { label: 'Feedback', path: '/it/design/feedback' },
      { label: 'Backlog', path: '/it/design/backlog' },
    ],
  };

  let { group, active }: { group: ItTabGroup; active: string } = $props();
  const tabs = GROUPS[group];
</script>

<nav class="it-tabs" aria-label="IT {group}">
  {#each tabs as t (t.path)}
    <a
      href={href(t.path)}
      class:active={t.path === active}
      aria-current={t.path === active ? 'page' : undefined}
      onclick={(e) => {
        e.preventDefault();
        navigate(t.path);
      }}>{t.label}</a
    >
  {/each}
</nav>

<style>
  .it-tabs {
    display: flex;
    gap: 0.25rem;
    padding: 0.5rem 1.25rem 0;
    border-bottom: 1px solid var(--border, #d5d2ca);
    flex-wrap: wrap;
  }
  .it-tabs a {
    padding: 0.35rem 0.75rem;
    border: 1px solid transparent;
    border-bottom: none;
    border-radius: 6px 6px 0 0;
    color: inherit;
    text-decoration: none;
    font-size: 0.85rem;
  }
  .it-tabs a:hover {
    background: var(--surface-2, #efece5);
  }
  .it-tabs a.active {
    border-color: var(--border, #d5d2ca);
    background: var(--surface-1, #faf8f4);
    font-weight: 600;
  }
</style>
