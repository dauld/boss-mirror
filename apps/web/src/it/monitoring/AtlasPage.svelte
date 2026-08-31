<script lang="ts">
  // System Atlas — flow map of the tenant's actual Workflow graph.
  //
  // v1 was a static hand-placed device-lifecycle SVG, accurate only
  // for the used-device-shop tenant. v2 (this version) reads
  // `/api/workflows` and lays out the active Workflows by category,
  // each node showing its step count and a click-through to the
  // workflows KB. Tenant-aware out of the box: brewery sees brewing
  // / sales / procurement / finance flows; used-device-shop sees the
  // refurb / sale / service flows that v1 hand-encoded.
  //
  // For the layout: SVG canvas, one row per category (sorted), nodes
  // wrap to the next line at the canvas width. The node stays the
  // same shape as v1 so the legend + styles carry over.

  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { href, navigate } from '../../router';
  import StageRunsCard from './StageRunsCard.svelte';
  import { fetchStations, withLoad } from './stationMap';
  import type { MapState, StationNode, StationLoad } from './stationMap';

  // The station and stage-run series folded in from the retired map
  // and flow pages (1f6d55e0 Q3, David: "Fold them") — their graph
  // RENDERINGS duplicated this page; these data series did not.
  import {
    atlasLayout,
    atlasColorFor,
    NODE_W,
    NODE_H,
    type AtlasSpec,
  } from '../../jobs/atlas-layout';

  type AtlasState =
    | { kind: 'loading' }
    | { kind: 'ready'; specs: ReadonlyArray<AtlasSpec> }
    | { kind: 'error'; message: string };

  let atlas = $state<AtlasState>({ kind: 'loading' });

  onMount(async () => {
    try {
      const r = await fetch('/api/workflows');
      if (!r.ok) {
        atlas = { kind: 'error', message: `HTTP ${r.status}` };
        return;
      }
      const specs = (await r.json()) as ReadonlyArray<AtlasSpec>;
      atlas = { kind: 'ready', specs };
    } catch (e) {
      atlas = { kind: 'error', message: e instanceof Error ? e.message : String(e) };
    }
  });

  // Only the canvas width differs from the landing-page atlas; node
  // size, category order, and colours live in the shared engine.
  const CANVAS_W = 1410;

  let layoutResult = $derived.by(() =>
    atlas.kind === 'ready' ? atlasLayout(atlas.specs, CANVAS_W) : null,
  );

  function go(e: MouseEvent, target: string): void {
    e.preventDefault();
    navigate(href(target));
  }

  // Stations: registry rows + one shared load pass (the retired map's
  // one-pass replacement for 55 per-node queue fetches). A failed
  // read renders as a failure, never as an empty registry.
  let stations: MapState = $state({ kind: 'loading' });
  let stationNodes: ReadonlyArray<StationNode> = $state([]);
  onMount(async () => {
    const st = await fetchStations();
    stations = st;
    if (st.kind !== 'ready') return;
    stationNodes = st.nodes;
    try {
      const r = await fetch('/api/stations/load');
      const body: unknown = await r.json().catch(() => null);
      const rows = Array.isArray(body)
        ? (body as StationLoad[])
        : ((body as { data?: StationLoad[] } | null)?.data ?? []);
      stationNodes = withLoad(st.nodes, rows);
    } catch {
      /* load is an enrichment; the registry list stands without it */
    }
  });
</script>

<div class="atlas">
  <PageHeader
    eyebrow="System Model · System atlas"
    title="Operating model flows"
    subtitle="Every Workflow this tenant publishes, grouped by category. Data-driven from /api/workflows."
  />

  <Section title="Workflow map" wide>
    {#if atlas.kind === 'loading'}
      <p class="empty">Loading Workflows…</p>
    {:else if atlas.kind === 'error'}
      <p class="empty">Couldn't load /api/workflows: {atlas.message}</p>
    {:else if atlas.specs.length === 0}
      <p class="empty">
        This tenant has no published Workflows. Author one at
        <a href={href('/it/registry')}>/it/registry</a>
        to populate the atlas.
      </p>
    {:else if layoutResult}
      <div class="atlas-canvas-wrap">
        <svg
          viewBox={`0 0 ${CANVAS_W} ${layoutResult.height}`}
          class="atlas-canvas"
          role="img"
          aria-label="Tenant Workflow flow diagram"
          preserveAspectRatio="xMinYMid meet"
        >
          {#each layoutResult.rows as row (row.category)}
            <!-- Row label on the left margin -->
            <text
              x={20}
              y={row.y + NODE_H / 2 + 4}
              class="track-label"
            >{row.category}</text>
            {#each row.nodes as n (n.kind)}
              {@const c = atlasColorFor(n.category)}
              <a
                href={href(`/it/registry`)}
                onclick={(e) => go(e, `/it/registry`)}
                class="node-link"
              >
                <g class="node-v2" transform={`translate(${n.x}, ${n.y})`}>
                  <rect
                    width={NODE_W}
                    height={NODE_H}
                    rx="6"
                    ry="6"
                    style={`fill: white; stroke: ${c.stroke}; stroke-width: 1.5`}
                  />
                  <text
                    class="node-label"
                    x={NODE_W / 2}
                    y={26}
                    text-anchor="middle"
                  >{n.label}</text>
                  <text
                    class="node-sub"
                    x={NODE_W / 2}
                    y={48}
                    text-anchor="middle"
                  >{n.step_count} step{n.step_count === 1 ? '' : 's'}</text>
                </g>
              </a>
            {/each}
          {/each}
        </svg>
      </div>
    {/if}
  </Section>

  <Section title="Stations" wide>
    {#if stations.kind === 'loading'}
      <p class="atlas-note">Reading the station registry…</p>
    {:else if stations.kind === 'unavailable'}
      <p class="atlas-note">The station registry has not reached this deployment.</p>
    {:else if stations.kind === 'error'}
      <p class="atlas-note atlas-fail">
        Station read FAILED — this is an outage of the read, not an empty registry.
      </p>
    {:else}
      <table class="atlas-stations">
        <thead>
          <tr><th>Station</th><th>Discipline</th><th>Depth</th><th>Oldest</th><th></th></tr>
        </thead>
        <tbody>
          {#each stationNodes as n (n.name)}
            <tr>
              <td>{n.title}</td>
              <td>{n.discipline}</td>
              <td>{n.depth ?? '—'}</td>
              <td>{n.oldestAgeDays == null ? '—' : `${n.oldestAgeDays}d`}</td>
              <td>{n.overLimit ? 'over limit' : ''}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </Section>

  <Section title="Stage runs" wide>
    <StageRunsCard />
  </Section>

  <Section title="How to read this">
      <p class="prose">
        Each box is a <strong>Workflow</strong> the tenant has published —
        a typed unit of coordinated work, with a fixed step graph
        registered in <code>workflows</code>. Rows group by
        <code>category</code>; the count below each label is the
        number of Steps in the Workflow's graph. Click any node to
        jump to the <a href={href('/it/registry')}>Workflows</a> KB
        for full step-graph + metadata.
      </p>
      <p class="atlas-note">
        Live in-flight Job counts per kind are a deliberate TODO —
        the shape of the tenant's operating model matters more
        than the numbers at first pass.
      </p>
  </Section>
</div>

<style>
  .atlas-canvas-wrap {
    overflow-x: auto;
    padding: 8px 0;
  }
  .atlas-canvas {
    width: 100%;
    min-width: 1000px;
    height: auto;
    display: block;
  }

  .track-label {
    font-size: 13px;
    font-weight: 600;
    fill: var(--muted, #64748b);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  /* v2 atlas — node + interactivity styles. v1 device/account
     legend swatches removed alongside the static SVG (b. 4be08d5). */
  .node-v2 rect {
    transition: filter 0.12s ease;
  }
  .node-link:hover .node-v2 rect {
    filter: brightness(0.97);
  }
  .node-link {
    cursor: pointer;
    text-decoration: none;
  }
  .node-label {
    font-size: 14px;
    font-weight: 600;
    fill: #0f172a;
  }
  .node-sub {
    font-size: 11px;
    fill: var(--muted, #64748b);
  }

  .atlas-note {
    margin: 8px 0 0;
    font-size: 12px;
    color: var(--muted, #64748b);
    max-width: 72ch;
  }
  .atlas-stations { width: 100%; border-collapse: collapse; font-size: 0.85rem; }
  .atlas-stations th, .atlas-stations td { text-align: left; padding: 0.3rem 0.6rem; border-bottom: 1px solid var(--border, #d5d2ca); }
  .atlas-fail { color: var(--danger, #b3402a); font-weight: 600; }
</style>
