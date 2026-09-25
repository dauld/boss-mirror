<script lang="ts">
  // The IT family tab strip (2026-08-31 consolidation, 1f6d55e0):
  // Operate / Registry / Design each carry their family as tabs on
  // one surface instead of sidebar rows. One component, data-driven —
  // adding a tab is a row here, not a new sidebar entry (the sidebar
  // holds exactly six IT rows and this file is why it can).
  import { href, navigate } from '../router';

  export type ItTabGroup = 'operate' | 'registry' | 'design';

  const GROUPS: Readonly<Record<ItTabGroup, ReadonlyArray<{ label: string; path: string }>>> = {
    // Car N3 of design e765b3fc (2026-09-25) took five tabs off this
    // strip, each now a selection's panel on the Department Map: Yard
    // status (the track, dock and garage stations), Conductor (the
    // track), Receiving Yard and Marshalling Yard (those two stations),
    // and on Design, Feedback and Backlog (receiving; the backlog on
    // marshalling too). What stays is desk work, not a station.
    operate: [
      { label: 'Incidents', path: '/it/operate' },
      { label: 'Audit Log', path: '/it/operate/audit' },
      { label: 'Performance', path: '/it/operate/perf' },
      { label: 'Atlas', path: '/it/operate/atlas' },
      { label: 'Bottlenecks', path: '/it/operate/bottlenecks' },
    ],
    registry: [
      { label: 'Workflows', path: '/it/registry' },
      { label: 'Dispatcher', path: '/it/registry/dispatcher' },
      // The authoring list of the rules the cascade draws. Rendered bare
      // until 2026-09-24, reachable only through the cascade's "Edit
      // rules →" link, and 0 of 784 surface-opens reached it (0a98d93f).
      { label: 'Rules', path: '/it/registry/rules' },
      { label: 'Step plugins', path: '/it/registry/step-plugins' },
      { label: 'Policy', path: '/it/registry/policy' },
      { label: 'Subjects', path: '/it/registry/subjects' },
      { label: 'Drift', path: '/it/registry/drift' },
    ],
    design: [
      { label: 'Reviews', path: '/it/design' },
      { label: 'Experiments', path: '/it/design/experiments' },
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
