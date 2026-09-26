<script lang="ts">
  // REAL PACKETS MOVING ON THE MAP (design e765b3fc §3, car M2 on feedback
  // 84cba7e2) — behind the flight `it-map-live`. One `<g>` laid inside the
  // transit map's SVG, in its coordinates: a dot per recorded move along
  // its route over 6 s, a ping where it lands, a train as a short bar with
  // a pip per car aboard, a hand-off as the old identity fading where the
  // new one sets off, and an undeclared route drawn dashed red under the
  // dot that took it.
  //
  // ONE EVENTSOURCE, GET /api/yard/moves/stream (car M1). The browser
  // resumes it with Last-Event-ID = the record's seq, so a reconnect is a
  // range read on the server, not a replay here; a `resync` frame means
  // "take the current placement, nothing moved" and draws nothing. A
  // stream the browser gives up on (a refusal, a 503) falls back to
  // GET /api/yard/moves?since=<seq> every 10 s, and a feed neither can
  // read is SAID on the map — a silent layer would read as a quiet yard.
  //
  // THE LOOP RUNS ONLY WHILE SOMETHING IS DRAWN: requestAnimationFrame,
  // started by a move and stopped when the last dot has landed and the
  // last ping swelled — an idle map costs no frames. A hidden tab draws
  // nothing and runs no frames: a move only counts, and on return each
  // station pings once with its count, "n moves while away". Dots are
  // positioned by direct attribute writes, not by re-rendering, which is
  // what keeps forty of them inside the 2 ms a frame is allowed
  // (`data-frame-ms`, the flight's headless signal).
  //
  // The rules are live-motion.ts, unit-pinned; the lines are whatever
  // `geometry` hands in (live-geometry.ts). Colours are --map-* tokens
  // only (map-palette.test.ts).
  import { onMount } from 'svelte';
  import {
    EMPTY,
    admit,
    busy,
    dotAt,
    dotTitle,
    moreOf,
    parseMove,
    parseResync,
    returned,
    step,
    type Geometry,
    type Motion,
    type Move,
  } from './live-motion';

  type Props = Readonly<{
    geometry: Geometry;
    /** prefers-reduced-motion: a move is a still count flash, no transit. */
    reduced: boolean;
  }>;
  let { geometry, reduced }: Props = $props();

  // The two paths are written where they are fetched, whole: the
  // gateway's `every_api_path_the_web_fetches_is_routed` reads a
  // `const X = '/api/…'` as a base with more after it.
  const POLL_MS = 10_000;

  let motion = $state.raw<Motion>(EMPTY);
  /** What the feed is doing — said on the map when it is not live. */
  let feed = $state<'connecting' | 'live' | 'polling' | 'down'>('connecting');
  let feedWhy = $state('');
  let awayNote = $state<string | null>(null);
  let root = $state<SVGGElement | undefined>(undefined);

  /** The dots' elements, for the frame loop's direct writes. */
  const els = new Map<number, SVGGElement>();
  function track(el: SVGGElement, id: number) {
    els.set(id, el);
    return { destroy: () => els.delete(id) };
  }

  let raf = 0;
  let frames = 0;
  let frameMs = 0;
  const hidden = (): boolean => typeof document !== 'undefined' && document.hidden;

  function frame(): void {
    raf = 0;
    if (hidden()) return;
    const t0 = performance.now();
    const next = step(motion, t0);
    if (next !== motion) motion = next;
    for (const d of next.dots) {
      const el = els.get(d.id);
      if (el === undefined) continue;
      const p = dotAt(d, t0);
      el.setAttribute('transform', `translate(${p.x.toFixed(1)} ${p.y.toFixed(1)})`);
      if (d.off) el.setAttribute('opacity', p.opacity.toFixed(2));
    }
    const spent = performance.now() - t0;
    frameMs = frames === 0 ? spent : frameMs * 0.9 + spent * 0.1;
    frames += 1;
    root?.setAttribute('data-frame-ms', frameMs.toFixed(3));
    root?.setAttribute('data-frames', String(frames));
    if (busy(next)) raf = requestAnimationFrame(frame);
  }
  function run(): void {
    if (raf === 0 && !hidden() && busy(motion)) raf = requestAnimationFrame(frame);
  }

  let seq: number | null = null;
  function take(mv: Move): void {
    if (seq !== null && mv.seq <= seq) return;
    seq = mv.seq;
    motion = admit(motion, mv, geometry, { now: performance.now(), hidden: hidden(), reduced });
    run();
  }

  onMount(() => {
    let es: EventSource | null = null;
    let poll: ReturnType<typeof setInterval> | null = null;
    let gone = false;

    async function pollOnce(): Promise<void> {
      try {
        const r = await fetch(seq === null ? '/api/yard/moves?limit=1' : `/api/yard/moves?since=${seq}`);
        if (gone) return;
        if (!r.ok) {
          feed = 'down';
          feedWhy = `${r.status} ${(await r.text()).slice(0, 200)}`.trim();
          return;
        }
        const body: unknown = await r.json();
        const o = body !== null && typeof body === 'object' ? (body as Record<string, unknown>) : null;
        if (o === null || !Array.isArray(o.moves) || typeof o.latest_seq !== 'number') {
          feed = 'down';
          feedWhy = 'the moves record answered a shape this page cannot read';
          return;
        }
        feed = 'polling';
        if (seq === null) {
          // A fresh read is a baseline, like the stream's resync: nothing moved.
          seq = o.latest_seq;
          return;
        }
        for (const raw of o.moves) {
          const mv = parseMove(JSON.stringify(raw));
          if (mv !== null) take(mv);
        }
      } catch (e) {
        if (gone) return;
        feed = 'down';
        feedWhy = e instanceof Error ? e.message : String(e);
      }
    }

    es = new EventSource('/api/yard/moves/stream');
    es.addEventListener('open', () => (feed = 'live'));
    es.addEventListener('resync', (e) => {
      const r = parseResync((e as MessageEvent<string>).data);
      if (r !== null) seq = r.seq;
      feed = 'live';
    });
    es.addEventListener('move', (e) => {
      const mv = parseMove((e as MessageEvent<string>).data);
      if (mv !== null) take(mv);
    });
    es.addEventListener('error', () => {
      if (es === null || es.readyState !== EventSource.CLOSED || poll !== null) return;
      // The browser gave the stream up and will not retry: read the
      // record instead, from where the stream left off.
      feed = 'polling';
      void pollOnce();
      poll = setInterval(() => void pollOnce(), POLL_MS);
    });

    function onVisibility(): void {
      if (hidden()) {
        if (raf !== 0) cancelAnimationFrame(raf);
        raf = 0;
        return;
      }
      const back = returned(motion, geometry, performance.now());
      motion = back.motion;
      awayNote = back.away > 0 ? `${back.away} move${back.away === 1 ? '' : 's'} while away` : awayNote;
      run();
    }
    document.addEventListener('visibilitychange', onVisibility);

    return () => {
      gone = true;
      es?.close();
      if (poll !== null) clearInterval(poll);
      if (raf !== 0) cancelAnimationFrame(raf);
      document.removeEventListener('visibilitychange', onVisibility);
    };
  });

  const more = $derived(moreOf(motion));
  const awayTotal = $derived(Object.values(motion.away).reduce((n, k) => n + k, 0));
</script>

<g class="live" bind:this={root} data-live data-feed={feed} data-dots={motion.dots.length}
  data-pings={motion.pings.length} data-away={awayTotal} data-motion={reduced ? 'reduced' : 'moving'}>
  {#each motion.dots as d (d.id)}
    {#if d.undeclared}
      <!-- A route no derived route supports: dashed red, under its dot. -->
      <path d={d.d} class="undeclared" data-undeclared-route="{d.move.from ?? '∅'}→{d.move.to ?? '∅'}" />
    {/if}
    {#if d.handoff !== null}
      <!-- The identity it continues fades where it stood. -->
      <circle cx={d.handoff.x} cy={d.handoff.y} r="7" class="handoff" data-handoff={d.move.handoff_from} />
    {/if}
  {/each}
  {#each motion.dots as d (d.id)}
    {@const p = dotAt(d, d.start)}
    <g class="dot line-{d.line}" use:track={d.id} data-dot={d.id} data-packet={d.move.packet}
      data-route="{d.move.from ?? '∅'}→{d.move.to ?? '∅'}" data-off={d.off ? 'true' : undefined}
      transform="translate({p.x.toFixed(1)} {p.y.toFixed(1)})">
      <title>{dotTitle(d.move)}</title>
      {#if d.pips > 0}
        {@const w = Math.max(18, Math.min(d.pips, 6) * 6 + 6)}
        <rect x={-w / 2} y="-5" width={w} height="10" rx="2" class="train-bar" data-train-pips={d.pips} />
        {#each Array.from({ length: Math.min(d.pips, 6) }) as _, i (i)}
          <circle cx={-w / 2 + 6 + i * 6} cy="0" r="2" class="pip" />
        {/each}
      {:else}
        <circle r="5" class="mark" />
      {/if}
    </g>
  {/each}
  {#each motion.pings as p (p.key)}
    <g class="ping-at line-{p.line}" data-ping={p.station} data-k={p.k}>
      <circle cx={p.at.x} cy={p.at.y} r="14" class="ping" class:still={p.still || reduced} />
      {#if p.k > 1}
        <text x={p.at.x + 16} y={p.at.y - 14} class="burst">×{p.k}</text>
      {/if}
    </g>
  {/each}
  {#each more as m (m.key)}
    <text x={m.at.x} y={m.at.y - 10} text-anchor="middle" class="more" data-motion-more={m.key}>+{m.n}</text>
  {/each}
  {#if feed === 'down'}
    <text x="12" y="22" class="feed-down" data-feed-down>the moves feed cannot be read — nothing drawn moving is live: {feedWhy}</text>
  {:else if awayNote !== null}
    <text x="12" y="22" class="away" data-away-note>{awayNote}</text>
  {/if}
</g>

<style>
  .live { pointer-events: none; }
  .dot { pointer-events: auto; cursor: default; }
  .line-delivery { --live: var(--map-line-delivery); }
  .line-publish { --live: var(--map-line-publish); }
  .line-siding { --live: var(--map-line-siding); }
  .line-tenant { --live: var(--map-line-tenant); }
  .mark { fill: var(--live); stroke: var(--map-surface); stroke-width: 2; }
  .train-bar { fill: var(--map-surface); stroke: var(--live); stroke-width: 2; }
  .pip { fill: var(--map-ink); }
  .undeclared { fill: none; stroke: var(--map-bad-edge); stroke-width: 3; stroke-dasharray: 6 5; stroke-linecap: round; }
  .handoff { fill: var(--map-muted); animation: fade 1.2s ease-out forwards; }
  .ping { fill: none; stroke: var(--live); stroke-width: 3; transform-box: fill-box; transform-origin: center;
    animation: swell 0.7s ease-out forwards; }
  .ping.still { animation: none; stroke-width: 4; }
  .burst, .more { font-family: var(--font-mono); font-size: 10px; font-variant-numeric: tabular-nums; fill: var(--map-ink); }
  .more { fill: var(--map-muted); }
  .away { font-family: var(--font-mono); font-size: 11px; fill: var(--map-muted); }
  .feed-down { font-family: var(--font-mono); font-size: 11px; fill: var(--map-bad-ink); }
  @keyframes swell { from { transform: scale(0.6); opacity: 1; } to { transform: scale(1.6); opacity: 0; } }
  @keyframes fade { from { opacity: 0.9; } to { opacity: 0; } }
  @media (prefers-reduced-motion: reduce) { .ping, .handoff { animation: none; } }
</style>
