<script lang="ts">
  // THE MOTION LAYER over the IT world map (design 31bade8f, decided by
  // David 2026-09-24; car M2 on backlog d220022f). ONE canvas laid over
  // the SVG world, in the same world coordinates: the SVG keeps the
  // territories, the words and every click target, so the map stays
  // accessible and testable, and this draws only what moves — the rate
  // replay's tokens, the piles standing at each rail's head, and the
  // one ring when a region crosses into troubled (decision 10).
  //
  // EVERY NUMBER IS THE SERVER'S. The arithmetic is world-motion.ts,
  // unit-pinned; this owns the frames and the strokes. A rail the server
  // judges still (troubled, not flowing, nothing crossing) stops
  // emitting and its tokens finish their trip; an unknown one never
  // moves; a pile changes only when a read reports a new `waiting`.
  //
  // THE LOOP RUNS ONLY WHILE SOMETHING MOVES: requestAnimationFrame,
  // stopped while the page is hidden, while paused, under reduced
  // motion (static density, drawn once), when the map has missed its
  // reads (the page greys it), and when every rail stands still with
  // nothing left in flight. Tokens live in one array per rail — no DOM
  // node per token — and each rail's tokens fill as ONE path, which is
  // what keeps 500 of them inside the 2 ms a frame is allowed.
  //
  // Behind the flight `it-map-motion` (design c4c2a607): WorldMap mounts
  // this only when the flight is on for the viewer.
  import { onMount, untrack } from 'svelte';
  import type { Border, Borders } from './borders';
  import type { Regions } from './regions';
  import { WORLD, type Rail, type Territory, territoryOf } from './world';
  import {
    DEFAULT_COMPRESSION,
    RING_MS,
    TWEEN_MS,
    PILE_SQUARE,
    advance,
    capacityOf,
    emitPerSec,
    newlyTroubled,
    pathPoints,
    pileBox,
    pileOf,
    pileSquares,
    pointAt,
    prefill,
    railStill,
    ringAt,
    squareAlpha,
    squareAt,
    walk,
    type Flow,
    type PileClass,
  } from './world-motion';

  type RailIn = Readonly<{ key: string; rail: Rail; from: Territory }>;
  type Props = Readonly<{
    rails: ReadonlyArray<RailIn>;
    borders: Borders | null;
    regions: Regions;
    /** ×N, or 0 for paused. */
    compression: number;
    reduced: boolean;
    /** Past three missed reads: nothing moves, and the page greys. */
    stale: boolean;
  }>;
  let { rails, borders, regions, compression, reduced, stale }: Props = $props();

  let canvas = $state<HTMLCanvasElement | undefined>(undefined);
  let hidden = $state(typeof document !== 'undefined' && document.hidden);

  const byBorder = $derived(new Map<string, Border>((borders?.borders ?? []).map((b) => [`${b.from}→${b.to}`, b])));
  const walked = $derived(new Map(rails.map((r) => [r.key, walk(pathPoints(r.rail.d))] as const)));
  /** Each rail as this frame draws it: the server's row, its stillness,
   *  its emission at the chosen compression (the static density reads
   *  the default while paused), its pile and where that stands. */
  const plan = $derived(
    rails.map((r) => {
      const b = byBorder.get(r.key);
      const w = walked.get(r.key)!;
      const box = pileBox(r.rail, r.from);
      return {
        key: r.key,
        w,
        still: railStill(b),
        e: emitPerSec(b?.rate.current ?? null, compression > 0 ? compression : DEFAULT_COMPRESSION, w.length),
        box,
        pile: pileOf(b),
        squares: pileSquares(pileOf(b), capacityOf(box)),
      };
    }),
  );

  // The animation's own state — presentation only, never drawn as a
  // number. Replaced whole on every step (world-motion's `advance`).
  let flows = new Map<string, Flow>();
  /** A pile's last change: the squares before and when the read moved it. */
  let piles = new Map<string, Readonly<{ before: ReadonlyArray<PileClass>; at: number }>>();
  /** The squares the last read drew, per rail. */
  let lastSquares = new Map<string, ReadonlyArray<PileClass>>();
  let rings: ReadonlyArray<Readonly<{ box: Territory; at: number }>> = [];
  /** How many rings this page has rung — the meter the spec reads. */
  let rung = 0;
  let states: ReadonlyMap<string, Regions['regions'][number]['state']> | null = null;

  let raf = 0;
  let lastT = 0;
  let frameMs = 0;
  let scale = 1;
  let dpr = 1;
  let colours = { accent: '', ink: '', muted: '', bad: '', bg: '', mono: 'monospace' };

  const mode = $derived(stale ? 'stale' : hidden ? 'hidden' : reduced ? 'reduced' : compression === 0 ? 'paused' : 'moving');
  const inFlight = (): number => [...flows.values()].reduce((n, f) => n + f.tokens.length, 0);

  // A new compression or a reduced-motion change re-stands every rail
  // in its steady state (decision 2: it never starts empty and fills).
  $effect(() => {
    void compression;
    void reduced;
    untrack(() => {
      flows = new Map(plan.map((p) => [p.key, p.still === null ? prefill(p.w.length, p.e) : prefill(0, p.e)]));
      kick();
    });
  });

  // A new read: rails the map has not drawn yet stand in their steady
  // state, and a pile whose squares changed records what it was, so the
  // change is drawn between the two reads' counts and nowhere else.
  $effect(() => {
    const now = performance.now();
    const current = plan;
    untrack(() => {
      flows = new Map(
        current.map((p) => [p.key, flows.get(p.key) ?? (p.still === null ? prefill(p.w.length, p.e) : prefill(0, p.e))]),
      );
      piles = new Map(
        current.map((p) => {
          const was = lastSquares.get(p.key);
          const moved = was !== undefined && was.join() !== p.squares.squares.join();
          const tween = moved && !reduced ? { before: was, at: now } : undefined;
          return [p.key, tween ?? piles.get(p.key) ?? { before: p.squares.squares, at: Number.NEGATIVE_INFINITY }];
        }),
      );
      lastSquares = new Map(current.map((p) => [p.key, p.squares.squares]));
      kick();
    });
  });

  // THE ONE RING (Q2): a region crossing INTO troubled rings once from
  // its outline, then is still. Never a loop, and none under reduced
  // motion — the red outline and the words carry it there.
  $effect(() => {
    const next = new Map(regions.regions.map((r) => [r.name, r.state] as const));
    untrack(() => {
      const now = performance.now();
      const fresh = reduced
        ? []
        : newlyTroubled(states, next).flatMap((name) => {
            const t = territoryOf(name);
            return t ? [{ box: t, at: now }] : [];
          });
      states = next;
      rings = [...rings.filter((r) => now - r.at < RING_MS), ...fresh];
      rung += fresh.length;
      if (fresh.length > 0) kick();
    });
  });

  $effect(() => {
    // Stop the moment the map goes stale or hidden; resume after.
    if (stale || hidden) stop();
    else untrack(kick);
  });

  function stop(): void {
    if (raf !== 0) cancelAnimationFrame(raf);
    raf = 0;
    lastT = 0;
  }

  function kick(): void {
    if (raf === 0 && canvas !== undefined && !stale && !hidden) raf = requestAnimationFrame(frame);
  }

  /** Is anything left to move? Tokens only while running and unreduced;
   *  a tween or a ring in any mode but stale. */
  function moving(now: number): boolean {
    const tween = [...piles.values()].some((p) => now - p.at < TWEEN_MS);
    const ring = rings.some((r) => now - r.at < RING_MS);
    if (tween || ring) return true;
    if (reduced || compression === 0) return false;
    return inFlight() > 0 || plan.some((p) => p.still === null && p.e.per > 0);
  }

  function frame(t: number): void {
    raf = 0;
    if (canvas === undefined || stale || hidden) return;
    const dt = lastT === 0 ? 0 : Math.min(0.1, (t - lastT) / 1000);
    lastT = t;
    if (!reduced && compression > 0 && dt > 0) {
      flows = new Map(
        plan.map((p) => [p.key, advance(flows.get(p.key) ?? { tokens: [], acc: 0.5 }, dt, p.w.length, p.e, p.still === null)]),
      );
    }
    const t0 = performance.now();
    draw(t0);
    frameMs = frameMs === 0 ? performance.now() - t0 : frameMs * 0.9 + (performance.now() - t0) * 0.1;
    if (moving(t0)) raf = requestAnimationFrame(frame);
    else lastT = 0;
  }

  function draw(now: number): void {
    const ctx = canvas?.getContext('2d');
    if (!canvas || !ctx) return;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.setTransform(scale * dpr, 0, 0, scale * dpr, 0, 0);
    plan.forEach((p) => drawPile(ctx, p, now));
    plan.forEach((p) => drawTokens(ctx, p));
    drawRings(ctx, now);
  }

  type Planned = (typeof plan)[number];

  function drawTokens(ctx: CanvasRenderingContext2D, p: Planned): void {
    // Reduced motion: the rate as static dots at the spacing a moving
    // stream would have, so density still reads (decision 9). Unknown
    // and still rails draw none — stillness is the signal.
    const tokens = reduced ? (p.still === null ? prefill(p.w.length, p.e).tokens : []) : (flows.get(p.key)?.tokens ?? []);
    if (tokens.length === 0) return;
    const big = tokens[0]!.k > 1;
    const r = big ? 3.4 : 2.8;
    // A halo in the ground's colour first, so a token reads against the
    // rail's own dark stroke it travels on.
    ctx.fillStyle = colours.bg;
    ctx.beginPath();
    tokens.forEach((tk) => {
      const at = pointAt(p.w, tk.d);
      ctx.moveTo(at.x + r + 1.2, at.y);
      ctx.arc(at.x, at.y, r + 1.2, 0, Math.PI * 2);
    });
    ctx.fill();
    ctx.fillStyle = colours.accent;
    ctx.beginPath();
    tokens.forEach((tk) => {
      const at = pointAt(p.w, tk.d);
      ctx.moveTo(at.x + r, at.y);
      ctx.arc(at.x, at.y, r, 0, Math.PI * 2);
    });
    ctx.fill();
    if (!big) return;
    // One token for k crossings: larger, with a hollow centre.
    ctx.fillStyle = colours.bg;
    ctx.beginPath();
    tokens.forEach((tk) => {
      const at = pointAt(p.w, tk.d);
      ctx.moveTo(at.x + 1.3, at.y);
      ctx.arc(at.x, at.y, 1.3, 0, Math.PI * 2);
    });
    ctx.fill();
  }

  function drawPile(ctx: CanvasRenderingContext2D, p: Planned, now: number): void {
    const box = p.box;
    if (p.pile.kind === 'unknown') {
      // Uncounted: a dashed grey "?" — never an empty pile.
      ctx.save();
      ctx.setLineDash([2, 2]);
      ctx.strokeStyle = colours.muted;
      ctx.lineWidth = 1;
      ctx.strokeRect(box.x + 0.5, box.y + 0.5, 18, 11);
      ctx.fillStyle = colours.muted;
      ctx.font = `9px ${colours.mono}`;
      ctx.fillText('?', box.x + 6, box.y + 9);
      ctx.restore();
      return;
    }
    const after = p.squares.squares;
    const hist = piles.get(p.key);
    const before = hist?.before ?? after;
    const age = hist === undefined ? Number.POSITIVE_INFINITY : now - hist.at;
    const n = Math.max(before.length, after.length);
    Array.from({ length: n }).forEach((_, i) => {
      const alpha = reduced ? (i < after.length ? 1 : 0) : squareAlpha(i, before.length, after.length, age);
      if (alpha <= 0) return;
      const cls = i < after.length ? after[i]! : before[i]!;
      const at = squareAt(box, i);
      // An arrival drops in from above its cell; nothing ever rides off.
      const y = at.y - (i >= before.length ? (1 - alpha) * 4 : 0);
      ctx.globalAlpha = alpha;
      if (cls === 'person') {
        ctx.strokeStyle = colours.ink;
        ctx.lineWidth = 1;
        ctx.strokeRect(at.x + 0.5, y + 0.5, PILE_SQUARE - 1, PILE_SQUARE - 1);
      } else {
        ctx.fillStyle = cls === 'stuck' ? colours.bad : cls === 'machine' ? colours.ink : colours.muted;
        ctx.fillRect(at.x, y, PILE_SQUARE, PILE_SQUARE);
      }
    });
    ctx.globalAlpha = 1;
    if (p.squares.more > 0) {
      ctx.fillStyle = colours.ink;
      ctx.font = `9px ${colours.mono}`;
      ctx.textAlign = 'right';
      ctx.fillText(`+${p.squares.more}`, box.more.x, box.more.y);
      ctx.textAlign = 'left';
    }
  }

  function drawRings(ctx: CanvasRenderingContext2D, now: number): void {
    rings.forEach((r) => {
      const at = ringAt(now - r.at);
      if (at === null) return;
      ctx.globalAlpha = at.alpha;
      ctx.strokeStyle = colours.bad;
      ctx.lineWidth = 2;
      ctx.strokeRect(r.box.x - at.grow, r.box.y - at.grow, r.box.w + 2 * at.grow, r.box.h + 2 * at.grow);
    });
    ctx.globalAlpha = 1;
  }

  function resize(): void {
    if (!canvas) return;
    const w = canvas.clientWidth;
    const h = canvas.clientHeight;
    dpr = window.devicePixelRatio || 1;
    scale = w / WORLD.width;
    canvas.width = Math.max(1, Math.round(w * dpr));
    canvas.height = Math.max(1, Math.round(h * dpr));
    // A resize clears the canvas: draw it again even when nothing moves.
    stop();
    kick();
  }

  onMount(() => {
    if (!canvas) return;
    // The palette lives once, in styles.css's --map-* block (42f66fb3):
    // the canvas reads the same tokens the SVG strokes with.
    const css = getComputedStyle(canvas);
    const token = (name: string): string => css.getPropertyValue(name).trim();
    colours = {
      accent: token('--map-accent'),
      ink: token('--map-ink'),
      muted: token('--map-muted'),
      bad: token('--map-bad-edge'),
      bg: token('--map-bg'),
      mono: token('--font-mono') || 'monospace',
    };
    const observer = new ResizeObserver(resize);
    observer.observe(canvas);
    resize();
    const onVisibility = () => {
      hidden = document.hidden;
    };
    document.addEventListener('visibilitychange', onVisibility);
    // The meter a reader (and the mocked spec) can read: tokens in
    // flight and the draw cost per frame, twice a second.
    const meter = setInterval(() => {
      canvas?.setAttribute('data-tokens', String(reduced ? 0 : inFlight()));
      canvas?.setAttribute('data-frame-ms', frameMs.toFixed(3));
      canvas?.setAttribute('data-rings', String(rung));
    }, 500);
    return () => {
      stop();
      observer.disconnect();
      document.removeEventListener('visibilitychange', onVisibility);
      clearInterval(meter);
    };
  });
</script>

<canvas bind:this={canvas} class="motion-layer" data-motion={mode} aria-hidden="true"></canvas>

<style>
  /* Laid exactly over the SVG world, which keeps every click. */
  .motion-layer {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
  }
</style>
