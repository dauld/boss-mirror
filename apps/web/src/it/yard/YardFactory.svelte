<script lang="ts">
  // The factory floor — the Train Yard as a living production line
  // (David, 2026-09-05: "make it look more like Factorio"). A
  // left-to-right belt of stations; each car is a wagon that rolls,
  // each gate is a machine whose gear turns while it works, each train
  // is a locomotive pulling its consist and chugging toward ARRIVED.
  //
  // Purely a lens over the same read-model the board below renders:
  // `factoryStations` maps it to wagons, and because every wagon keeps
  // a stable id across the 10s poll, a car that changed station slides
  // out of one bay and into the next — the state change told as motion.
  import { fly, scale } from 'svelte/transition';
  import { protocolHue } from './yard';
  import { conductorLine, carsOnLine, type FactoryStation, type Wagon } from './yard-factory';

  const { stations, idle, onOpen } = $props<{
    stations: readonly FactoryStation[];
    idle: boolean;
    onOpen: (id: string) => void;
  }>();

  const open = (w: Wagon) => {
    if (w.packetId) onOpen(w.packetId);
  };

  // The conductor is the floor's personified voice (design 7cdd8047),
  // and the header carries the live production count.
  const says = $derived(conductorLine(stations));
  const onLine = $derived(carsOnLine(stations));
</script>

<section class="factory" aria-label="the factory floor">
  <div class="factory-head">
    <span class="factory-title">THE FACTORY FLOOR</span>
    <span class="factory-run" class:idle
      >{idle ? 'idle — no work on the line' : `${onLine} on the line`}</span>
  </div>

  <!-- The conductor speaks (7cdd8047): a personified line reporting what
       the floor is doing, trouble first. -->
  <div class="conductor">
    <span class="conductor-badge" aria-hidden="true">
      <span class="conductor-cap"></span>
    </span>
    <span class="conductor-says" role="status">{says}</span>
  </div>

  <div class="line">
    {#each stations as st, si (st.key)}
      <div class="station" data-station={st.key}>
        <div class="station-label">{st.label}</div>

        <div class="belt">
          <div class="belt-tread"></div>

          {#if st.machines.length > 0}
            <div class="machines">
              {#each st.machines as m (m.id)}
                <button
                  type="button"
                  class="machine"
                  class:busy={m.busy}
                  disabled={!m.packetId}
                  title={m.busy ? `${m.branch} — ${m.since}` : `${m.label} — idle`}
                  onclick={() => m.packetId && onOpen(m.packetId)}>
                  <span class="gear" class:spin={m.busy}>✳</span>
                  {#if m.busy}
                    <span class="machine-branch">{m.branch}</span>
                  {:else}
                    <span class="machine-idle">idle</span>
                  {/if}
                </button>
              {/each}
            </div>
          {/if}

          <div class="wagons">
            {#each st.wagons as w (w.id)}
              <button
                type="button"
                class="wagon tone-{w.tone}"
                class:sim={w.sim}
                class:loco={w.isTrain}
                style={`--hue:${protocolHue(w.kind)}`}
                title={`${w.label} — ${w.detail}`}
                onclick={() => open(w)}
                in:fly={{ x: -46, duration: 480 }}
                out:fly={{ x: 46, duration: 380 }}>
                {#if w.isTrain}
                  <span class="cab"></span>
                  <span class="wagon-body">
                    <span class="wagon-label">{w.label}</span>
                    <span class="consist">×{w.cars}</span>
                  </span>
                {:else}
                  <span class="wagon-body">
                    <span class="wagon-label">{w.label}</span>
                  </span>
                {/if}
                <span class="wheel wheel-a"></span>
                <span class="wheel wheel-b"></span>
                {#if w.tone === 'arrived'}
                  <span class="spark" in:scale={{ duration: 500 }}>✔</span>
                {/if}
              </button>
            {/each}

            {#if st.wagons.length === 0 && st.machines.length === 0}
              <span class="bay-empty">—</span>
            {/if}
          </div>
        </div>

        {#if si < stations.length - 1}
          <div class="link" aria-hidden="true">
            <span class="chev">›</span>
          </div>
        {/if}
      </div>
    {/each}
  </div>
</section>

<style>
  /* Dark industrial ground, hazard accents — the yard's own tokens
     with a Factorio temperament. */
  .factory {
    border: 1px solid var(--hairline, #2a3138);
    background: var(--card, #12161c);
    border-radius: 0;
    padding: 12px 12px 16px;
    margin: 0 0 22px;
    overflow-x: auto;
  }
  .factory-head {
    display: flex;
    align-items: baseline;
    gap: 12px;
    margin-bottom: 12px;
  }
  .factory-title {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px;
    letter-spacing: 0.16em;
    color: var(--text, #c7ced6);
  }
  .factory-run {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10px;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--signal, #5fd4a8);
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .factory-run::before {
    content: '';
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--signal, #5fd4a8);
    box-shadow: 0 0 6px var(--signal, #5fd4a8);
    animation: pulse 1.6s ease-in-out infinite;
  }
  .factory-run.idle {
    color: var(--static, #7a838c);
  }
  .factory-run.idle::before {
    background: var(--static, #7a838c);
    box-shadow: none;
    animation: none;
  }

  .line {
    display: flex;
    align-items: stretch;
    gap: 0;
    min-width: min-content;
  }
  .station {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 150px;
    flex: 1 1 0;
    position: relative;
  }
  .station-label {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10px;
    letter-spacing: 0.14em;
    color: var(--static, #7a838c);
    text-align: center;
  }

  /* The belt: a dark deck with a moving hazard tread beneath the cars. */
  .belt {
    position: relative;
    min-height: 92px;
    border: 1px solid var(--hairline, #2a3138);
    background: var(--ink, #0e1218);
    padding: 10px 8px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    overflow: hidden;
  }
  .belt-tread {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 8px;
    background: repeating-linear-gradient(
      -45deg,
      var(--warn, #d9a441) 0 8px,
      #0e1218 8px 16px
    );
    opacity: 0.45;
    background-size: 22.6px 8px;
    animation: tread 0.9s linear infinite;
  }

  .wagons {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-content: flex-start;
    position: relative;
    z-index: 1;
  }
  .bay-empty {
    color: var(--static, #7a838c);
    font-size: 13px;
    opacity: 0.5;
    align-self: center;
    margin: auto;
  }

  /* A wagon: a hued body on two wheels, bobbing as if idling on the
     track. The hue is the packet's protocol color (same as its card). */
  .wagon {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 5px 8px 8px;
    border: 1px solid color-mix(in srgb, var(--hue) 55%, #000);
    border-bottom-width: 3px;
    border-radius: 3px;
    background: color-mix(in srgb, var(--hue) 26%, var(--ink, #0e1218));
    color: var(--text, #eef2f6);
    font: inherit;
    cursor: pointer;
    max-width: 130px;
    animation: bob 2.8s ease-in-out infinite, carry 3.6s ease-in-out infinite;
    animation-delay: calc(var(--i, 0) * -0.4s);
  }
  .wagon:hover {
    filter: brightness(1.2);
  }
  .wagon-body {
    display: flex;
    flex-direction: column;
    line-height: 1.15;
    min-width: 0;
  }
  .wagon-label {
    font-size: 11px;
    font-family: var(--font-mono, ui-monospace, monospace);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 96px;
  }
  .consist {
    font-size: 10px;
    color: var(--warn, #d9a441);
    font-variant-numeric: tabular-nums;
  }
  .wheel {
    position: absolute;
    bottom: -4px;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #05070a;
    border: 1.5px solid #3a424c;
  }
  .wheel-a {
    left: 8px;
  }
  .wheel-b {
    right: 8px;
  }

  /* A train is a locomotive: a cab in front of the body, and it chugs. */
  .wagon.loco {
    padding-left: 4px;
    border-color: color-mix(in srgb, var(--hue) 70%, #000);
    background: color-mix(in srgb, var(--hue) 34%, var(--ink, #0e1218));
    max-width: 150px;
  }
  .cab {
    width: 9px;
    height: 20px;
    background: color-mix(in srgb, var(--hue) 60%, #000);
    border-radius: 2px 0 0 2px;
    flex: none;
  }

  /* Tones — the accent tells the car's state, reusing the lamp palette
     the board already speaks: signal green, warn amber, err red. */
  .tone-queued {
    opacity: 0.82;
  }
  .tone-gating {
    border-color: var(--warn, #d9a441);
    box-shadow: 0 0 9px -1px var(--warn, #d9a441);
  }
  .tone-red,
  .tone-blocked {
    border-color: var(--err, #e2685c);
    box-shadow: 0 0 8px -2px var(--err, #e2685c);
  }
  .tone-green,
  .tone-arrived {
    border-color: var(--signal, #5fd4a8);
  }
  .tone-ci {
    border-color: var(--warn, #d9a441);
  }
  .tone-moving {
    border-color: var(--signal, #5fd4a8);
    animation: chug 0.6s ease-in-out infinite;
  }
  .tone-arrived {
    animation: none;
  }
  .wagon.sim {
    border-style: dashed;
    opacity: 0.7;
  }
  .spark {
    position: absolute;
    top: -8px;
    right: -6px;
    font-size: 12px;
    color: var(--signal, #5fd4a8);
    text-shadow: 0 0 6px var(--signal, #5fd4a8);
  }

  /* Gate machines: a bank of bays; a busy one turns its gear and glows. */
  .machines {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    position: relative;
    z-index: 1;
  }
  .machine {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 6px 8px;
    border: 1px solid var(--hairline, #2a3138);
    border-radius: 2px;
    background: #05070a;
    color: var(--static, #7a838c);
    font: inherit;
    font-size: 10px;
    cursor: default;
  }
  .machine.busy {
    border-color: var(--warn, #d9a441);
    color: var(--text, #c7ced6);
    box-shadow: 0 0 10px -3px var(--warn, #d9a441);
    cursor: pointer;
  }
  .gear {
    color: var(--static, #555c65);
    font-size: 13px;
    display: inline-block;
  }
  /* An inserter arm on a working gate — the Factorio touch: a little
     arm that swings over the bay while the machine runs. */
  .machine.busy {
    position: relative;
  }
  .machine.busy::after {
    content: '';
    position: absolute;
    top: -6px;
    right: 10px;
    width: 2px;
    height: 9px;
    background: var(--warn, #d9a441);
    transform-origin: bottom center;
    animation: grab 1.4s ease-in-out infinite;
  }
  .gear.spin {
    color: var(--warn, #d9a441);
    animation: spin 2.2s linear infinite;
  }
  .machine-branch {
    font-family: var(--font-mono, ui-monospace, monospace);
    max-width: 80px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .machine-idle {
    font-style: italic;
    opacity: 0.7;
  }

  /* The couplers between stations. */
  .link {
    position: absolute;
    right: -1px;
    top: 50%;
    transform: translate(50%, 4px);
    z-index: 2;
    pointer-events: none;
  }
  .chev {
    color: var(--static, #4a525b);
    font-size: 16px;
  }

  /* The conductor: a small figure (a cap) with a speech bubble — the
     floor's own voice. Quiet palette; the words carry it. */
  .conductor {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 0 0 12px;
  }
  .conductor-badge {
    position: relative;
    width: 22px;
    height: 22px;
    flex: none;
    border-radius: 50%;
    background: var(--ink, #0e1218);
    border: 1px solid var(--hairline, #2a3138);
  }
  .conductor-cap {
    position: absolute;
    left: 4px;
    right: 4px;
    top: 5px;
    height: 6px;
    background: var(--signal, #5fd4a8);
    border-radius: 2px 2px 0 0;
  }
  .conductor-cap::after {
    content: '';
    position: absolute;
    left: -2px;
    right: -2px;
    bottom: -2px;
    height: 2px;
    background: var(--signal, #5fd4a8);
  }
  .conductor-says {
    position: relative;
    font-size: 12px;
    color: var(--text, #c7ced6);
    background: var(--ink, #0e1218);
    border: 1px solid var(--hairline, #2a3138);
    border-radius: 3px;
    padding: 5px 10px;
    font-style: italic;
  }
  .conductor-says::before {
    content: '';
    position: absolute;
    left: -5px;
    top: 50%;
    transform: translateY(-50%);
    border: 5px solid transparent;
    border-right-color: var(--hairline, #2a3138);
  }

  @keyframes grab {
    0%, 100% { transform: rotate(-18deg); }
    50% { transform: rotate(22deg); }
  }
  /* The belt carries its cars: a small drift downstream and back, so a
     wagon reads as riding a moving belt rather than parked on it. */
  @keyframes carry {
    0%, 100% { transform: translateX(0); }
    50% { transform: translateX(2.5px); }
  }
  @keyframes bob {
    0%,
    100% {
      transform: translateY(0);
    }
    50% {
      transform: translateY(-2px);
    }
  }
  @keyframes chug {
    0%,
    100% {
      transform: translateX(0);
    }
    50% {
      transform: translateX(3px);
    }
  }
  @keyframes tread {
    to {
      background-position: 22.6px 0;
    }
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @keyframes pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.35;
    }
  }

  /* Respect a viewer who asked for less motion: keep the layout, drop
     the loops. */
  @media (prefers-reduced-motion: reduce) {
    .wagon,
    .tone-moving,
    .belt-tread,
    .gear.spin,
    .machine.busy::after,
    .factory-run::before {
      animation: none !important;
    }
  }
</style>
