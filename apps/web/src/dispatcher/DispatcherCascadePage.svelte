<!--
  /it/registry/dispatcher — the dispatcher rule cascade.

  Renders the reactive layer the boss-dispatcher runs: trigger event →
  rule → handler(s) → emitted events → (loop back). Loops close where an
  emitted topic re-triggers a rule; jobs-api/external "system" edges that
  re-enter the rule set are drawn distinctly, and the feedback cycles
  (restock, DAG-advance, AR) are highlighted. Data: GET /api/dispatcher/rules.
  Layout dagre LR; render Svelte Flow (same stack as the Workflow graph).
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { SvelteFlow, Background, Controls, MiniMap, MarkerType } from '@xyflow/svelte';
  import type { Node, Edge } from '@xyflow/svelte';
  import '@xyflow/svelte/dist/style.css';
  import dagre from '@dagrejs/dagre';
  import { buildCascade, filterCascadeFromEvents, type Cascade } from './cascadeToGraph';
  import type { DispatcherRules } from './types';
  import { href, navigate } from '../router';

  let data = $state<DispatcherRules | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(true);
  /** selected node id (`evt:` / `rule:` / `hdl:`), for the detail panel. */
  let selected = $state<string | null>(null);
  /** Selected trigger events (on_event topics) to narrow the diagram to
   *  their forward cascade. Empty = the full view. */
  let selectedTriggers = $state<string[]>([]);

  const NODE_W = 240;
  const NODE_H = 54;

  onMount(() => {
    void (async () => {
      try {
        const r = await fetch('/api/dispatcher/rules');
        if (!r.ok) {
          error = `HTTP ${r.status} fetching /api/dispatcher/rules`;
          return;
        }
        const payload = (await r.json()) as DispatcherRules;
        if (payload.error) error = payload.error;
        data = payload;
      } catch (e) {
        error = e instanceof Error ? e.message : String(e);
      } finally {
        loading = false;
      }
    })();
  });

  const fullCascade = $derived<Cascade>(
    data && !data.error ? buildCascade(data) : { nodes: [], edges: [] },
  );
  // Narrow to the selected triggers' forward cascade; empty = the full view.
  const cascade = $derived<Cascade>(filterCascadeFromEvents(fullCascade, selectedTriggers));

  /** Distinct trigger events (topics rules listen for), sorted — the filter
   *  selector's options. */
  const allTriggers = $derived([...new Set((data?.rules ?? []).map((r) => r.on_event))].sort());
  const availableTriggers = $derived(allTriggers.filter((t) => !selectedTriggers.includes(t)));

  function addTrigger(e: Event): void {
    const sel = e.currentTarget as HTMLSelectElement;
    const t = sel.value;
    if (t && !selectedTriggers.includes(t)) selectedTriggers = [...selectedTriggers, t];
    sel.value = '';
  }
  function removeTrigger(t: string): void {
    selectedTriggers = selectedTriggers.filter((x) => x !== t);
  }

  const EDGE_STYLE: Record<string, string> = {
    trigger: 'stroke:#64748b;stroke-width:1.5',
    do: 'stroke:#3b82f6;stroke-width:1.5',
    emit: 'stroke:#10b981;stroke-width:1.5',
    system: 'stroke:#f59e0b;stroke-width:1.5;stroke-dasharray:6 4',
    match: 'stroke:#cbd5e1;stroke-width:1;stroke-dasharray:2 3',
  };

  function buildFlow(c: Cascade, sel: string | null): { nodes: Node[]; edges: Edge[] } {
    const g = new dagre.graphlib.Graph();
    g.setGraph({ rankdir: 'LR', nodesep: 22, ranksep: 90 });
    g.setDefaultEdgeLabel(() => ({}));
    for (const n of c.nodes) g.setNode(n.id, { width: NODE_W, height: NODE_H });
    for (const e of c.edges) g.setEdge(e.source, e.target);
    dagre.layout(g);

    const nodes: Node[] = c.nodes.map((n) => {
      const p = g.node(n.id);
      const classes = ['dx-node', `dx-${n.kind}`];
      if (n.inCycle) classes.push('dx-cycle');
      if (n.id === sel) classes.push('dx-selected');
      return {
        id: n.id,
        position: { x: (p?.x ?? 0) - NODE_W / 2, y: (p?.y ?? 0) - NODE_H / 2 },
        data: { label: n.sublabel ? `${n.label}\n${n.sublabel}` : n.label },
        class: classes.join(' '),
        sourcePosition: 'right',
        targetPosition: 'left',
      } as Node;
    });

    const edges: Edge[] = c.edges.map((e) => ({
      id: e.id,
      source: e.source,
      target: e.target,
      animated: e.inCycle,
      style: e.inCycle ? 'stroke:#dc2626;stroke-width:2.5' : EDGE_STYLE[e.kind],
      label: e.kind === 'system' ? e.label : undefined,
      labelStyle: 'font-size:10px;fill:#92400e',
      markerEnd: { type: MarkerType.ArrowClosed, width: 16, height: 16 },
    }));
    return { nodes, edges };
  }

  const computed = $derived(buildFlow(cascade, selected));
  let nodes = $state.raw<Node[]>([]);
  let edges = $state.raw<Edge[]>([]);
  $effect(() => {
    nodes = computed.nodes;
    edges = computed.edges;
  });

  // Selected-node detail for the side panel.
  const detail = $derived.by(() => {
    if (!selected || !data) return null;
    if (selected.startsWith('rule:')) {
      const name = selected.slice(5);
      const rule = data.rules.find((r) => r.name === name);
      return rule ? { kind: 'rule' as const, rule } : null;
    }
    if (selected.startsWith('hdl:')) {
      const handler = selected.slice(4);
      return { kind: 'handler' as const, handler, emits: data.handler_emits[handler] ?? [] };
    }
    if (selected.startsWith('evt:')) {
      const event = selected.slice(4);
      const triggers = data.rules.filter((r) => r.on_event === event).map((r) => r.name);
      const emittedBy = Object.entries(data.handler_emits)
        .filter(([, list]) => list.includes(event))
        .map(([h]) => h);
      return { kind: 'event' as const, event, triggers, emittedBy };
    }
    return null;
  });

  const counts = $derived({
    rules: data?.rules.length ?? 0,
    handlers: Object.keys(data?.handler_emits ?? {}).length,
    cycleNodes: fullCascade.nodes.filter((n) => n.inCycle).length,
  });
</script>

<div class="dx">
  <header class="dx-head">
    <div>
      <h1>Dispatcher rules — reactive cascade</h1>
      <p class="dx-sub">
        The side-effect wiring the <code>boss-dispatcher</code> runs: a step
        completes or an event fires → a rule matches → handlers run → they emit
        events that re-trigger more rules. Red = a feedback cycle.
      </p>
    </div>
    <div class="dx-head-right">
      {#if data && !error}
        <div class="dx-stats">
          <span>{counts.rules} rules</span>
          <span>{counts.handlers} handlers</span>
          <span class="dx-stat-cycle">{counts.cycleNodes} in cycles</span>
        </div>
      {/if}
      <a
        class="dx-edit-link"
        href={href('/it/registry/rules')}
        onclick={(e) => {
          if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
          e.preventDefault();
          navigate(href('/it/registry/rules'));
        }}
      >
        Edit rules →
      </a>
    </div>
  </header>

  <div class="dx-legend">
    <span class="dx-key dx-event">event</span>
    <span class="dx-key dx-rule">rule</span>
    <span class="dx-key dx-handler">handler</span>
    <span class="dx-edgekey"><i style="background:#10b981"></i>emits</span>
    <span class="dx-edgekey"><i style="background:#f59e0b"></i>system (jobs-api / external)</span>
    <span class="dx-edgekey"><i style="background:#dc2626"></i>feedback cycle</span>
  </div>

  {#if data && !error && allTriggers.length}
    <div class="dx-filter">
      <label class="dx-filter-label">
        Trigger
        <select class="dx-filter-select" onchange={addTrigger}>
          <option value="">filter cascade by trigger event…</option>
          {#each availableTriggers as t (t)}
            <option value={t}>{t}</option>
          {/each}
        </select>
      </label>
      {#each selectedTriggers as t (t)}
        <span class="dx-chip">
          {t}
          <button type="button" class="dx-chip-x" title="remove" onclick={() => removeTrigger(t)}>×</button>
        </span>
      {/each}
      {#if selectedTriggers.length}
        <button type="button" class="dx-clear" onclick={() => (selectedTriggers = [])}>show all</button>
        <span class="dx-filter-note">
          cascade from {selectedTriggers.length} trigger{selectedTriggers.length === 1 ? '' : 's'} ·
          {cascade.nodes.length}/{fullCascade.nodes.length} nodes
        </span>
      {/if}
    </div>
  {/if}

  <div class="dx-body">
    <div class="dx-flow">
      {#if loading}
        <div class="dx-msg">Loading dispatcher rules…</div>
      {:else if error}
        <div class="dx-msg dx-err">Couldn’t load rules: {error}</div>
      {:else if cascade.nodes.length === 0}
        <div class="dx-msg">No dispatcher rules are loaded.</div>
      {:else}
        <!-- key on the filter so a selection re-mounts the flow and
             fitView re-frames the narrowed subgraph. -->
        {#key selectedTriggers.join('|')}
          <SvelteFlow
            bind:nodes
            bind:edges
            fitView
            nodesDraggable
            elementsSelectable
            onnodeclick={({ node }) => (selected = node.id)}
            onpaneclick={() => (selected = null)}
          >
            <Background />
            <Controls showLock={false} />
            <MiniMap pannable zoomable />
          </SvelteFlow>
        {/key}
      {/if}
    </div>

    {#if detail}
      <aside class="dx-panel">
        {#if detail.kind === 'rule'}
          <h2>rule · {detail.rule.name}</h2>
          <dl>
            <dt>on event</dt>
            <dd><code>{detail.rule.on_event}</code></dd>
            {#if detail.rule.when}
              <dt>when</dt>
              <dd><code class="dx-when">{detail.rule.when}</code></dd>
            {/if}
            <dt>do</dt>
            <dd>
              <ol>
                {#each detail.rule.do as step}
                  <li>
                    <code>{step.handler}</code>
                    {#if Object.keys(step.args).length}
                      <ul class="dx-args">
                        {#each Object.entries(step.args) as [k, v]}
                          <li><span class="dx-arg-k">{k}</span> = <code>{v}</code></li>
                        {/each}
                      </ul>
                    {/if}
                  </li>
                {/each}
              </ol>
            </dd>
          </dl>
        {:else if detail.kind === 'handler'}
          <h2>handler · {detail.handler}</h2>
          <dt>emits</dt>
          {#if detail.emits.length}
            <ul>
              {#each detail.emits as e}<li><code>{e}</code></li>{/each}
            </ul>
          {:else}
            <p class="dx-sink">— sink (emits no event)</p>
          {/if}
        {:else}
          <h2>event · {detail.event}</h2>
          <dt>triggers rules</dt>
          {#if detail.triggers.length}
            <ul>{#each detail.triggers as r}<li>{r}</li>{/each}</ul>
          {:else}
            <p class="dx-sink">— (no rule listens for this exact topic)</p>
          {/if}
          <dt>emitted by</dt>
          {#if detail.emittedBy.length}
            <ul>{#each detail.emittedBy as h}<li><code>{h}</code></li>{/each}</ul>
          {:else}
            <p class="dx-sink">— (external / jobs-api origin)</p>
          {/if}
        {/if}
      </aside>
    {/if}
  </div>
</div>

<style>
  .dx {
    display: flex;
    flex-direction: column;
    height: calc(100vh - 64px);
    padding: 16px 20px 0;
    box-sizing: border-box;
  }
  .dx-head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 16px;
  }
  .dx-head h1 {
    font-size: 1.15rem;
    margin: 0 0 4px;
  }
  .dx-sub {
    margin: 0;
    max-width: 70ch;
    color: #475569;
    font-size: 0.85rem;
  }
  .dx-head-right {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 6px;
  }
  .dx-stats {
    display: flex;
    gap: 12px;
    white-space: nowrap;
    font-size: 0.8rem;
    color: #475569;
  }
  .dx-edit-link {
    font-size: 0.8rem;
    color: #2563eb;
    text-decoration: none;
    white-space: nowrap;
  }
  .dx-edit-link:hover {
    text-decoration: underline;
  }
  .dx-stat-cycle {
    color: #dc2626;
    font-weight: 600;
  }
  .dx-legend {
    display: flex;
    flex-wrap: wrap;
    gap: 14px;
    align-items: center;
    margin: 10px 0;
    font-size: 0.75rem;
    color: #475569;
  }
  .dx-filter {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin: 0 0 10px;
    font-size: 0.78rem;
    color: #475569;
  }
  .dx-filter-label {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .dx-filter-select {
    font-size: 0.78rem;
    padding: 3px 6px;
    border: 1px solid #cbd5e1;
    border-radius: 6px;
    max-width: 340px;
  }
  .dx-chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    background: #eff6ff;
    border: 1px solid #bfdbfe;
    border-radius: 999px;
    padding: 2px 4px 2px 10px;
    color: #1e3a8a;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.72rem;
  }
  .dx-chip-x {
    border: none;
    background: none;
    cursor: pointer;
    color: #1e3a8a;
    font-size: 0.95rem;
    line-height: 1;
    padding: 0 3px;
  }
  .dx-chip-x:hover {
    color: #dc2626;
  }
  .dx-clear {
    border: 1px solid #cbd5e1;
    background: #fff;
    border-radius: 6px;
    padding: 2px 8px;
    cursor: pointer;
    font-size: 0.74rem;
    color: #475569;
  }
  .dx-clear:hover {
    background: #f1f5f9;
  }
  .dx-filter-note {
    color: #94a3b8;
  }
  .dx-key {
    padding: 2px 8px;
    border-radius: 6px;
    border: 1.5px solid;
  }
  .dx-edgekey {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .dx-edgekey i {
    width: 16px;
    height: 3px;
    border-radius: 2px;
    display: inline-block;
  }
  .dx-body {
    display: flex;
    gap: 12px;
    flex: 1;
    min-height: 0;
  }
  .dx-flow {
    flex: 1;
    border: 1px solid #e2e8f0;
    border-radius: 8px;
    background: #fafafa;
    min-width: 0;
  }
  .dx-msg {
    display: grid;
    place-items: center;
    height: 100%;
    color: #94a3b8;
    font-size: 0.9rem;
  }
  .dx-err {
    color: #dc2626;
  }
  .dx-panel {
    width: 320px;
    overflow-y: auto;
    border: 1px solid #e2e8f0;
    border-radius: 8px;
    background: #fff;
    padding: 12px 14px;
    font-size: 0.82rem;
  }
  .dx-panel h2 {
    font-size: 0.9rem;
    margin: 0 0 10px;
    word-break: break-all;
  }
  .dx-panel dt {
    font-weight: 600;
    color: #475569;
    margin-top: 8px;
  }
  .dx-panel dd {
    margin: 2px 0 0;
  }
  .dx-panel code {
    background: #f1f5f9;
    padding: 1px 4px;
    border-radius: 4px;
    font-size: 0.78rem;
    word-break: break-all;
  }
  .dx-when {
    display: block;
    white-space: pre-wrap;
  }
  .dx-args {
    margin: 2px 0 6px 0;
    padding-left: 16px;
    color: #64748b;
  }
  .dx-arg-k {
    color: #0f766e;
  }
  .dx-sink {
    color: #94a3b8;
    margin: 2px 0;
  }
  /* Node styling — classes set in buildFlow; :global because nodes render
     inside the Svelte Flow subtree. */
  :global(.dx-node) {
    border-radius: 8px;
    border-width: 1.5px;
    border-style: solid;
    white-space: pre-line;
    font-size: 0.72rem;
    line-height: 1.25;
    text-align: center;
    padding: 5px 8px;
    width: 240px;
    box-sizing: border-box;
  }
  :global(.dx-event) {
    background: #f8fafc;
    border-color: #94a3b8;
  }
  :global(.dx-rule) {
    background: #eff6ff;
    border-color: #3b82f6;
  }
  :global(.dx-handler) {
    background: #ecfdf5;
    border-color: #10b981;
  }
  :global(.dx-cycle) {
    box-shadow: 0 0 0 2px rgba(220, 38, 38, 0.45);
    border-color: #dc2626 !important;
  }
  :global(.dx-selected) {
    box-shadow: 0 0 0 3px rgba(59, 130, 246, 0.6);
  }
  .dx-event {
    background: #f8fafc;
    border-color: #94a3b8;
  }
  .dx-rule {
    background: #eff6ff;
    border-color: #3b82f6;
  }
  .dx-handler {
    background: #ecfdf5;
    border-color: #10b981;
  }
</style>
