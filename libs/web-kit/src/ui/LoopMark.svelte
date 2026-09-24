<script lang="ts">
  // The Algedonic Ales mark — Design Language v1.0 §05 MOTIF.
  //
  // Two arcs chasing each other around a center point: the feedback loop
  // the brewery is named for. The SIGNAL arc is the sensing half, the FOG
  // arc the adjusting half. That duality is the whole logo, so both arcs
  // are required — a single-arc version reads as a broken ring, not a loop.
  //
  // `ring` draws the enclosing circle, which makes it a badge (nav, login,
  // favicon). Without it you get the bare loop, which the spec calls for as
  // a bullet, list marker, loading spinner, and 404 art.
  //
  // Colors come from tokens rather than the spec's literals so the mark
  // tracks the palette instead of pinning two hexes in a second place.
  let {
    size = 40,
    ring = true,
    spin = false,
    title = '',
    band = false,
  }: Readonly<{
    size?: number;
    ring?: boolean;
    /// Rotates the arcs — for genuine loading states only, not decoration.
    spin?: boolean;
    /// Sets an accessible name. Empty (the default) marks the mark
    /// decorative and hides it from assistive tech, which is correct
    /// wherever a text wordmark sits beside it.
    title?: string;
    /// Drawn on an enamel band (the chrome bar, backlog 7eb59678 car 2):
    /// the adjusting arc is night ink, which would vanish into the band,
    /// so there it is set in the band's white instead.
    band?: boolean;
  }> = $props();
</script>

<svg
  class="loop-mark"
  class:spin
  width={size}
  height={size}
  viewBox="0 0 100 100"
  role={title ? 'img' : 'presentation'}
  aria-hidden={title ? undefined : 'true'}
  aria-label={title || undefined}
>
  {#if ring}
    <circle cx="50" cy="50" r="46" fill="none" stroke="var(--signal)" stroke-width="4" />
  {/if}
  <g class="arcs">
    <path
      d="M 50 22 A 28 28 0 0 1 76 61"
      fill="none"
      stroke="var(--signal)"
      stroke-width="5"
      stroke-linecap="round"
    />
    <path
      d="M 50 78 A 28 28 0 0 1 24 39"
      fill="none"
      stroke={band ? 'var(--on-band)' : 'var(--fog)'}
      stroke-width="5"
      stroke-linecap="round"
    />
  </g>
  <circle cx="50" cy="50" r="5" fill="var(--signal)" />
</svg>

<style>
  .loop-mark {
    display: block;
    flex: none;
  }

  /* Only the arcs turn. The ring and the center dot are the fixed frame
     the loop runs inside — spinning them would read as a plain spinner
     and lose the mark. */
  .spin .arcs {
    transform-origin: 50% 50%;
    animation: loop-mark-spin 1.4s linear infinite;
  }

  @keyframes loop-mark-spin {
    to {
      transform: rotate(360deg);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .spin .arcs {
      animation: none;
    }
  }
</style>
