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
      { label: 'Conductor', path: '/it/operate/conductor' },
      { label: 'Audit Log', path: '/it/operate/audit' },
      { label: 'Performance', path: '/it/operate/perf' },
      { label: 'Atlas', path: '/it/operate/atlas' },
      { label: 'Bottlenecks', path: '/it/operate/bottlenecks' },
      // The two queue boards are REGIONS of the world since car 4 of
      // design d2154293 — the tab walks to the zoomed territory, not
      // to a page of its own.
      { label: 'Receiving Yard', path: '/it/yard/receiving' },
      { label: 'Marshalling Yard', path: '/it/yard/marshalling' },
    ],
    registry: [
      { label: 'Workflows', path: '/it/registry' },
      { label: 'Dispatcher', path: '/it/registry/dispatcher' },
      { label: 'Step plugins', path: '/it/registry/step-plugins' },
      { label: 'Policy', path: '/it/registry/policy' },
      { label: 'Subjects', path: '/it/registry/subjects' },
      { label: 'Drift', path: '/it/registry/drift' },
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
    border-bottom: 1px solid var(--border);
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
    background: var(--ink-raised);
  }
  .it-tabs a.active {
    border-color: var(--border);
    background: var(--ink);
    font-weight: 600;
  }
</style>
