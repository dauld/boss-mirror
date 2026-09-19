<script lang="ts">
  // THE IT SYSTEM MAP — the /it landing since design 0524fc95 (decided
  // 2026-09-19; this is car 2, the page). Eight region cards in map
  // order — dock, gates, track, shed, arrivals, garage, receiving,
  // marshalling — read from ONE endpoint, /api/yard/regions (car 1):
  // the count, the clear / busy / troubled state, one sentence of why,
  // and the trend (this window against the previous, in the unit).
  // Every number here is the server's; the page derives nothing. It
  // used to be the other way: yard.ts read six endpoints and derived
  // every number the Train Yard showed, boss orient derived the same
  // numbers again, and nothing held the two equal.
  //
  // EVERY CARD IS A DOOR TO ITS FLOOR, and the floors are the panels
  // that already exist: the six yard regions open the Train Yard
  // (YardPage.svelte) at /it/yard/<region>, focused on that region's
  // panel — the dock and its lanes, the bays, the rail map on the
  // track, the shed's proof lanes, the arrivals, the garage; receiving
  // and marshalling open their own pages. Nothing is deleted; every
  // panel is one click deeper. The sidebar rows still open the floors
  // directly.
  //
  // NO NEW STYLING (the visual redesign is pending): the cards are the
  // yard's own classes — its panel, its lamp dots, its trouble badge —
  // in the yard's own grid. A troubled card looks troubled the way a
  // troubled train does.
  //
  // Polling stays as the yard's: a 10s tick. Reads go through
  // fetchRemote, so an outage renders a failure line (`load-failed`),
  // never an empty map that reads as a calm one.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { navigate } from '@boss/web-kit/nav';
  import type { Remote } from '../../data/remote';
  import { countText, fetchRegions, floorHref, lampOf, trendText, type Regions } from './regions';

  let regions = $state<Remote<Regions>>({ kind: 'loading' });
  let readAt = $state<number | null>(null);

  onMount(() => {
    let cancelled = false;
    async function tick() {
      const r = await fetchRegions();
      if (cancelled) return;
      regions = r;
      readAt = Date.now();
    }
    void tick();
    const t = setInterval(tick, 10_000);
    return () => {
      cancelled = true;
      clearInterval(t);
    };
  });

  const clock = (ms: number) =>
    new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });

  function open(e: MouseEvent, href: string): void {
    e.preventDefault();
    navigate(href);
  }
</script>

<div class="theme-exec yard-root">
  <PageHeader
    eyebrow="IT · Forge line"
    title="The train yard"
    subtitle="Eight regions, each a door to its floor — the count, whether it is clear, busy or troubled, and this window against the last"
  />

  {#if regions.kind === 'loading'}
    <div class="yard-empty">Reading the regions…</div>
  {:else if regions.kind === 'failed'}
    <!-- A failed read is said, in the one class the outage crawl reads:
         a map that could not be read must not look like a clear one. -->
    <div class="yard-empty load-failed">The regions cannot be read — {regions.error}</div>
  {:else}
    <div class="yard-map" aria-label="the system map">
      {#each regions.data.regions as r (r.name)}
        <!-- A link, not a button: the floor is a route, and Back returns
             here. The state rides on the card as data so a troubled card
             is one attribute away for a test or a reader; `why` is the
             tooltip on every card and printed on a troubled one, because
             a verdict must name what failed. -->
        <a
          class="yard-panel yard-region"
          data-region={r.name}
          data-state={r.state}
          href={floorHref(r.name)}
          title={r.why}
          onclick={(e) => open(e, floorHref(r.name))}>
          <h2 class="yard-panel-h">{r.name}</h2>
          <div class="yard-entity-title">{countText(r)}</div>
          <div class="yard-entity-sub">
            <span class="yard-lamp-dot {lampOf(r.state)}"></span>
            {#if r.state === 'troubled'}
              <span class="yard-trouble">troubled</span>
            {:else}
              {r.state}
            {/if}
          </div>
          <div class="yard-stamp">{r.trend.metric} · {trendText(r.trend)}</div>
          {#if r.state === 'troubled'}
            <div class="yard-why">{r.why}</div>
          {/if}
        </a>
      {/each}
    </div>
    <div class="yard-flow">
      window {regions.data.window_hours}h against the {regions.data.window_hours}h before{readAt !== null ? ` · read ${clock(readAt)}` : ''}
    </div>
  {/if}
</div>

<style>
  /* The yard's classes, as YardPage.svelte declares them (Svelte scopes
     a component's styles, so the map carries its own copy of the ones
     it uses — same names, same tokens, nothing new). */
  .yard-root { padding: 0 32px 32px; }
  .yard-empty { color: var(--static, #78716c); padding: 12px 0; font-size: 14px; }
  .yard-map { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: var(--s3, 12px); margin-top: var(--s3, 12px); }
  .yard-panel { background: var(--ink, #12161c); border: 1px solid var(--hairline, #2a3138); padding: var(--s4, 16px); min-width: 0; }
  .yard-region { display: block; color: inherit; text-decoration: none; min-height: 120px; }
  .yard-region:hover, .yard-region:focus-visible { border-color: var(--signal, #5fd4a8); }
  /* The alerts strip's trouble border, on a troubled card. */
  .yard-region[data-state='troubled'] { border-color: color-mix(in srgb, var(--err, #e2685c) 60%, var(--hairline, #2a3138)); }
  .yard-region[data-state='busy'] { border-color: color-mix(in srgb, var(--warn, #d9a441) 55%, var(--hairline, #2a3138)); }
  .yard-panel-h { margin: 0; font-size: 11px; letter-spacing: var(--ls-label, 0.1em); text-transform: uppercase; color: var(--static, #7a838c); font-weight: 600; }
  .yard-entity-title { font-size: 18px; font-weight: 600; margin: var(--s2, 8px) 0 var(--s1, 4px); text-wrap: balance; overflow-wrap: anywhere; }
  .yard-entity-sub { color: var(--static, #7a838c); font-size: 13px; margin-bottom: var(--s3, 12px); overflow-wrap: anywhere; }
  .yard-lamp-dot { display: inline-block; width: 8px; height: 8px; border-radius: 50%; background: var(--static, #7a838c); flex: none; margin-right: 6px; position: relative; top: -1px; }
  .yard-lamp-dot.ok { background: var(--ok, #4fb98a); box-shadow: 0 0 6px var(--ok, #4fb98a); }
  .yard-lamp-dot.warn { background: var(--warn, #d9a441); box-shadow: 0 0 6px var(--warn, #d9a441); }
  .yard-lamp-dot.err { background: var(--err, #e2685c); box-shadow: 0 0 7px var(--err, #e2685c); animation: yard-blink 1s steps(2) infinite; }
  .yard-trouble {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
    letter-spacing: 0.04em;
    padding: 1px 6px;
    border: 1px solid var(--err, #b91c1c);
    color: var(--err, #b91c1c);
    border-radius: 2px;
    text-transform: uppercase;
  }
  .yard-stamp { font-family: var(--font-mono, ui-monospace, monospace); font-size: 12px;
    color: var(--static, #7A838C); font-variant-numeric: tabular-nums; overflow-wrap: anywhere; }
  .yard-why { font-size: 12.5px; margin-top: var(--s2, 8px); color: var(--fog, #e8ecef); overflow-wrap: anywhere; }
  .yard-flow { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: var(--ls-nav, 0.14em); color: var(--static, #7A838C);
    border-top: 1px solid var(--hairline, #2A3138); margin-top: 28px; padding-top: 12px; }
  @keyframes yard-blink { 50% { opacity: 0.25; } }
  /* The yard's deck folds to one column when narrow; the map folds to two. */
  @media (max-width: 1000px) { .yard-map { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
  @media (prefers-reduced-motion: reduce) { .yard-lamp-dot.err { animation: none; } }
</style>
