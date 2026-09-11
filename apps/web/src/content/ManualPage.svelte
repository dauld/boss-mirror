<script lang="ts">
  // /manual — port of apps/web/src/content/ManualPage.tsx.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { formatDate } from '@boss/web-kit/ui/date';
  import RichBody from './RichBody.svelte';
  import type { ManualSection } from './types';
  import { href, navigate } from '../router';

  type Props = { slug: string | null };
  let { slug }: Props = $props();

  type TreeNode = {
    section: ManualSection;
    children: TreeNode[];
  };

  let sections = $state<ManualSection[]>([]);
  /// Non-null when the tree load failed — rendered instead of the
  /// empty state, so an outage never reads as "no sections yet"
  /// (packet 3fba9c35, the false-empty sweep).
  let sectionsFailed = $state<string | null>(null);
  let sectionsLoading = $state(true);
  let collapsed = $state<Set<string>>(new Set());

  let active = $state<ManualSection | null>(null);
  let activeLoading = $state(false);
  let activeNotFound = $state(false);
  let empNames = $state<Map<string, string>>(new Map());

  const COLLAPSED_KEY = 'boss.manual.collapsed';

  $effect(() => {
    try {
      const raw = localStorage.getItem(COLLAPSED_KEY);
      if (raw) {
        const arr = JSON.parse(raw) as unknown;
        if (Array.isArray(arr)) {
          collapsed = new Set(arr.filter((x) => typeof x === 'string') as string[]);
        }
      }
    } catch {
      // ignore
    }
  });

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/content/manual');
        if (r.ok) {
          const body = (await r.json()) as ManualSection[];
          if (!cancelled) {
            sections = body;
            sectionsFailed = null;
          }
        } else {
          if (!cancelled) sectionsFailed = `HTTP ${r.status}`;
        }
      } catch (e) {
        if (!cancelled) sectionsFailed = e instanceof Error ? e.message : String(e);
      }
      if (!cancelled) sectionsLoading = false;
      try {
        const r = await fetch('/api/people');
        if (r.ok) {
          const body = (await r.json()) as Array<{ id: string; name: string }>;
          const m = new Map<string, string>();
          for (const e of body) m.set(e.id, e.name);
          if (!cancelled) empNames = m;
        }
      } catch {
        // ignore
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  $effect(() => {
    const s = slug;
    if (!s) {
      active = null;
      activeLoading = false;
      activeNotFound = false;
      return;
    }
    let cancelled = false;
    activeLoading = true;
    activeNotFound = false;
    (async () => {
      try {
        const r = await fetch(`/api/content/manual/${s}`);
        if (r.status === 404) {
          if (!cancelled) {
            active = null;
            activeNotFound = true;
          }
        } else if (r.ok) {
          const body = (await r.json()) as ManualSection;
          if (!cancelled) active = body;
        }
      } catch {
        // ignore
      }
      if (!cancelled) activeLoading = false;
    })();
    return () => {
      cancelled = true;
    };
  });

  function buildTree(arr: ReadonlyArray<ManualSection>): TreeNode[] {
    const childrenOf = new Map<string | null, ManualSection[]>();
    for (const s of arr) {
      const key = s.parent_slug ?? null;
      const bucket = childrenOf.get(key) ?? [];
      bucket.push(s);
      childrenOf.set(key, bucket);
    }
    function build(parent: string | null): TreeNode[] {
      const kids = (childrenOf.get(parent) ?? []).slice();
      kids.sort(
        (a, b) => a.sort_order - b.sort_order || a.title.localeCompare(b.title),
      );
      return kids.map((s) => ({ section: s, children: build(s.slug) }));
    }
    return build(null);
  }

  let tree = $derived(buildTree(sections));

  function saveCollapsed(set: ReadonlySet<string>): void {
    try {
      localStorage.setItem(COLLAPSED_KEY, JSON.stringify(Array.from(set)));
    } catch {
      // ignore
    }
  }

  function toggle(s: string): void {
    const next = new Set(collapsed);
    if (next.has(s)) next.delete(s);
    else next.add(s);
    collapsed = next;
    saveCollapsed(next);
  }

  // Auto-expand ancestors of the active slug.
  $effect(() => {
    const s = slug;
    if (!s) return;
    const ancestors: string[] = [];
    const parts = s.split('/');
    for (let i = 1; i < parts.length; i++) {
      ancestors.push(parts.slice(0, i).join('/'));
    }
    if (ancestors.some((a) => collapsed.has(a))) {
      const next = new Set(collapsed);
      for (const a of ancestors) next.delete(a);
      collapsed = next;
      saveCollapsed(next);
    }
  });
</script>

<div class="theme-exec" style="padding:0 32px 32px">
  <PageHeader
    eyebrow="Know"
    title="Company manual"
    subtitle="Authored by HR. Every change is versioned."
  />
  <div class="manual-layout">
    <aside class="manual-tree">
      {#if sectionsLoading}
        <div class="manual-placeholder">Loading tree…</div>
      {:else if sectionsFailed}
        <div class="manual-placeholder load-failed" role="alert">
          Couldn't load the manual — {sectionsFailed}
        </div>
      {:else if tree.length === 0}
        <div class="manual-placeholder">No sections yet.</div>
      {:else}
        {#each tree as node (node.section.slug)}
          {@render treeNode(node, 0)}
        {/each}
      {/if}
    </aside>
    <section class="manual-content">
      {#if !slug}
        <div class="manual-placeholder">
          Select a section from the tree to start reading.
        </div>
      {:else if activeLoading}
        <div class="manual-placeholder">Loading…</div>
      {:else if activeNotFound}
        <div class="manual-placeholder">
          Section <code>{slug}</code> not found, or you don't have access to it.
        </div>
      {:else if !active}
        <div class="manual-placeholder">Unable to load section.</div>
      {:else}
        <article class="manual-article">
          <header>
            <div class="manual-article-eyebrow">{active.slug}</div>
            <h2>{active.title}</h2>
            <div class="manual-article-meta">
              Version {active.current_version} · updated
              {formatDate(active.updated_at)}
            </div>
          </header>
          <div class="manual-article-body">
            <RichBody body={active.body} employeeNames={empNames} />
          </div>
        </article>
      {/if}
    </section>
  </div>
</div>

{#snippet treeNode(node: TreeNode, depth: number)}
  {@const isActive = slug === node.section.slug}
  {@const hasChildren = node.children.length > 0}
  {@const isCollapsed = collapsed.has(node.section.slug)}
  <div
    class="manual-tree-node{isActive ? ' manual-tree-active' : ''}"
    style={`padding-left:${10 + depth * 14}px`}
  >
    <button
      type="button"
      class="manual-tree-toggle"
      aria-label={hasChildren ? (isCollapsed ? 'Expand' : 'Collapse') : undefined}
      onclick={(e) => {
        e.stopPropagation();
        if (hasChildren) toggle(node.section.slug);
      }}
    >
      {hasChildren ? (isCollapsed ? '▸' : '▾') : ''}
    </button>
    <a
      class="manual-tree-label"
      href={href(`/ux/manual/${node.section.slug}`)}
      onclick={(e) => {
        e.preventDefault();
        navigate(href(`/ux/manual/${node.section.slug}`));
      }}
    >
      {node.section.title}
    </a>
  </div>
  {#if !isCollapsed}
    {#each node.children as c (c.section.slug)}
      {@render treeNode(c, depth + 1)}
    {/each}
  {/if}
{/snippet}
