<script lang="ts">
  // The yard as a rail map. Scenery is drawn from the scene's shape
  // (one gate shed per bay the policy allows, the mainline under six
  // signals, the dock and garage sidings, the arrivals yard); tokens —
  // wagons and locomotives — are keyed by id and positioned by a CSS
  // transform, so when a wagon's station changes between two polls the
  // browser slides the same node from the old place to the new one.
  // That slide is the state change made visible, and it is the reason
  // a wagon must keep its id across polls (yard-floor.ts).
  //
  // Every machine and every token is a button: clicking (or Enter /
  // Space on) any of them selects it, and the page's entity panel shows
  // its facts and the verbs that apply. The map draws; it never decides.
  import { fade } from 'svelte/transition';
  import { ARRIVALS_DRAWN, STAGES, drawnWagons, type Bay, type Loco, type Scene, type Wagon } from './yard-floor';
  import { clusterLabel, runnerLabel, runnerProgress } from './yard-machines';

  type Props = Readonly<{
    scene: Scene;
    /** The selection key (`car:<id>`, `train:<id>`, `bay:<n>`, a machine). */
    selected: string;
    onselect: (key: string) => void;
  }>;
  let { scene, selected, onselect }: Props = $props();

  // ---- layout: everything hangs off the mainline's y, which drops
  // when the policy allows more than three bays. ----
  const BAY_H = 52;
  /** A wagon body is WAGON_W wide (an eleven-character nameplate at 9px
   *  mono fits), and slots step WAGON_STEP so neighbours never cover it. */
  const WAGON_W = 70;
  const WAGON_STEP = 74;
  const VIEW_W = 1240;
  const STAGE_X: readonly number[] = [620, 700, 780, 860, 940, 1020];
  const bayY = (i: number): number => 70 + i * BAY_H;
  const nBays = $derived(scene.bays.length);
  const mainY = $derived(Math.max(250, 70 + nBays * BAY_H + 24));
  // The arrivals stack starts under its sign and grows the map downward
  // — to the newest ARRIVALS_DRAWN landed wagons; the rest are one plate.
  const ARRIVALS_Y = 66;
  const drawn = $derived(drawnWagons(scene.wagons));
  const stackRows = $derived(Math.ceil(Math.min(drawn.drawn.filter(w => w.station === 'arrivals').length, ARRIVALS_DRAWN) / 2));
  const plateY = $derived(mainY + ARRIVALS_Y + stackRows * 40 + 2);
  const height = $derived(Math.max(mainY + 150, plateY + (drawn.hidden > 0 ? 18 : 8)));
  // The machines the page feeds from outside the yard status, and the
  // clock their elapsed readings run on (the scene's — the server's
  // when the status served).
  const nowMs = $derived(Date.parse(scene.now));
  const runner = $derived(scene.machines.runner);
  const cluster = $derived(scene.machines.cluster);
  const runnerText = $derived(runnerLabel(runner, nowMs));
  const runnerBar = $derived(runnerProgress(runner, nowMs));
  /** The shed is 130 wide; a long reason is cut on the map and whole in
   *  the aria-label and the entity panel. */
  const runnerShort = $derived(runnerText.length > 26 ? `${runnerText.slice(0, 25)}…` : runnerText);
  const limboY = $derived((nBays > 0 ? bayY(nBays - 1) : 70) + 48);
  const locoById = $derived(new Map(scene.locos.map(l => [l.id, l])));

  function locoX(l: Loco): number {
    const a = STAGE_X[l.stage] ?? STAGE_X[STAGE_X.length - 1] ?? 0;
    const b = STAGE_X[l.stage + 1] ?? a;
    return a + (b - a) * Math.max(0, Math.min(1, l.progress));
  }

  function wagonXY(w: Wagon): readonly [number, number] {
    switch (w.station) {
      case 'approach':
        return [34 + w.slot * WAGON_STEP, mainY - 2];
      case 'gate':
        return [240, bayY(w.slot) + 16];
      case 'limbo':
        return [380, limboY - w.slot * 24];
      case 'dock':
        return [412 + w.slot * WAGON_STEP, mainY - 2];
      case 'garage':
        return [436 + w.slot * WAGON_STEP, mainY + 68];
      case 'train': {
        const l = w.trainId ? locoById.get(w.trainId) : undefined;
        return [(l ? locoX(l) : STAGE_X[0] ?? 0) - (WAGON_W + 6) - w.slot * WAGON_STEP, mainY - 2];
      }
      case 'arrivals':
        return [1080 + (w.slot % 2) * WAGON_STEP, mainY + ARRIVALS_Y + Math.floor(w.slot / 2) * 40];
    }
  }

  function bayLabel(b: Bay): string {
    if (!b.busy || b.branch === null) return `bay ${b.index + 1} · idle`;
    const tail = b.stale ? `${b.elapsed ?? '—'} · STALE` : (b.elapsed ?? '—');
    return `bay ${b.index + 1} · ${b.tag ?? b.branch} · ${tail}`;
  }

  // The conductor's clock shows the server's clock, in UTC.
  const clock = $derived.by(() => {
    const ms = Date.parse(scene.now);
    if (Number.isNaN(ms)) return null;
    const d = new Date(ms);
    const ha = (((d.getUTCHours() % 12) + d.getUTCMinutes() / 60) / 12) * 2 * Math.PI;
    const ma = (d.getUTCMinutes() / 60) * 2 * Math.PI;
    return {
      hx: 1120 + 16 * Math.sin(ha),
      hy: 100 - 16 * Math.cos(ha),
      mx: 1120 + 24 * Math.sin(ma),
      my: 100 - 24 * Math.cos(ma),
    };
  });

  const pick = (key: string) => (e: Event) => {
    e.stopPropagation();
    onselect(key);
  };
  const pickKey = (key: string) => (e: KeyboardEvent) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      e.stopPropagation();
      onselect(key);
    }
  };
</script>

<section class="yard" aria-label="the yard map">
  <svg viewBox="0 0 {VIEW_W} {height}" role="img" aria-label="rail map of the ship-a-change line">
    <!-- scenery: the mainline, the turnouts, the signs -->
    <line x1="20" y1={mainY} x2={VIEW_W - 20} y2={mainY} class="tie" />
    <line x1="20" y1={mainY - 4} x2={VIEW_W - 20} y2={mainY - 4} class="rail" />
    <line x1="20" y1={mainY + 4} x2={VIEW_W - 20} y2={mainY + 4} class="rail" />
    {#each scene.bays as b (b.index)}
      <path
        d="M200 {mainY} C 215 {mainY}, 210 {bayY(b.index) + 18}, 228 {bayY(b.index) + 18} L 372 {bayY(b.index) + 18}"
        class="rail" />
    {/each}
    <path d="M400 {mainY} C 420 {mainY}, 410 {mainY + 70}, 430 {mainY + 70} L 590 {mainY + 70}" class="rail" />
    <path d="M1060 {mainY} C 1075 {mainY}, 1070 {mainY + 50}, 1085 {mainY + 50} L {VIEW_W - 15} {mainY + 50}" class="rail" />
    <text x="228" y="40">Gate runners · {nBays > 0 ? `${nBays} bays` : 'no reading'}</text>
    <text x="620" y={mainY + 32}>The track · one train at a time</text>

    <!-- the gate sheds -->
    {#each scene.bays as b (b.index)}
      <g
        class="machine"
        class:selected={selected === `bay:${b.index}`}
        role="button"
        tabindex="0"
        aria-label={bayLabel(b)}
        onclick={pick(`bay:${b.index}`)}
        onkeydown={pickKey(`bay:${b.index}`)}>
        <rect
          x="226"
          y={bayY(b.index) - 8}
          width="150"
          height="40"
          class="shed"
          class:busy={b.busy && !b.stale}
          class:warn={b.stale} />
        <rect x="234" y={bayY(b.index) + 26} width="120" height="3" class="barbg" />
        <rect
          x="234"
          y={bayY(b.index) + 26}
          width={Math.round(120 * b.progress)}
          height="3"
          class="barfill"
          class:warn={b.stale} />
        <text x="360" y={bayY(b.index) + 8} class="gear" class:spin={b.busy}>✳</text>
        <text x="234" y={bayY(b.index) - 12} class="tiny">{bayLabel(b)}</text>
      </g>
    {/each}

    <!-- the signals along the track: one machine, six lamps -->
    <g
      class="machine"
      class:selected={selected === 'track'}
      role="button"
      tabindex="0"
      aria-label="the track"
      onclick={pick('track')}
      onkeydown={pickKey('track')}>
      {#each STAGES as st, i (st)}
        <line x1={STAGE_X[i]} y1={mainY - 12} x2={STAGE_X[i]} y2={mainY - 46} class="sig-post post" />
        <circle cx={STAGE_X[i]} cy={mainY - 52} r="6" class="sig-lamp {scene.signals[i] ?? 'off'}" />
        <text x={STAGE_X[i]} y={mainY - 62} text-anchor="middle">{st}</text>
      {/each}
    </g>

    <!-- the deploy-runner shed: read off the newest converge ops-request
         (yard-machines.ts). It smokes while the converge runs; dark and
         "no reading" until the page has read the packets -->
    <g
      class="machine"
      class:selected={selected === 'runner'}
      role="button"
      tabindex="0"
      aria-label="deploy runner · {runnerText}"
      onclick={pick('runner')}
      onkeydown={pickKey('runner')}>
      <rect
        x="790"
        y="100"
        width="130"
        height="46"
        class="shed"
        class:busy={runner.kind === 'running'}
        class:warn={runner.kind === 'requested'}
        class:err={runner.kind === 'failed'} />
      <rect x="800" y="80" width="10" height="22" class="shed" />
      <circle cx="805" cy="76" r="5" class="smoke" class:on={runner.kind === 'running'} />
      <circle cx="805" cy="76" r="5" class="smoke" class:on={runner.kind === 'running'} />
      <circle cx="805" cy="76" r="5" class="smoke" class:on={runner.kind === 'running'} />
      <text x="798" y="116" class="big">deploy runner</text>
      <text x="798" y="130" class="tiny" class:err={runner.kind === 'failed'} class:warn={runner.kind === 'requested'}>{runnerShort}</text>
      <rect x="798" y="136" width="112" height="3" class="barbg" />
      <rect x="798" y="136" width={Math.round(112 * runnerBar)} height="3" class="barfill" />
    </g>

    <!-- the cluster tower: its lamp is the system of record observed
         from outside — this browser's read of /api/jobs/health; off
         until the page has read it, red when it does not answer -->
    <g
      class="machine"
      class:selected={selected === 'cluster'}
      role="button"
      tabindex="0"
      aria-label="cluster · {clusterLabel(cluster)}"
      onclick={pick('cluster')}
      onkeydown={pickKey('cluster')}>
      <rect x="950" y="70" width="60" height="76" class="shed" class:err={cluster.kind === 'dark'} />
      <circle cx="980" cy="92" r="9" class="tower-lamp" class:ok={cluster.kind === 'ready'} class:err={cluster.kind === 'dark'} />
      <text x="980" y="118" text-anchor="middle" class="big">cluster</text>
      <!-- two short lines fit the tower: the state, then the build -->
      <text x="980" y="132" text-anchor="middle" class="tiny" class:err={cluster.kind === 'dark'}
        >{cluster.kind === 'ready' ? 'ready' : clusterLabel(cluster)}</text>
      {#if cluster.kind === 'ready' && cluster.commit !== null}
        <text x="980" y="143" text-anchor="middle" class="tiny">{cluster.commit.slice(0, 7)}</text>
      {/if}
    </g>

    <!-- the conductor's clock -->
    <g
      class="machine"
      class:selected={selected === 'conductor'}
      role="button"
      tabindex="0"
      aria-label="conductor · {scene.machines.conductor.label}"
      onclick={pick('conductor')}
      onkeydown={pickKey('conductor')}>
      <circle cx="1120" cy="100" r="30" class="shed" class:err={scene.machines.conductor.silent} />
      {#if clock}
        <line x1="1120" y1="100" x2={clock.hx} y2={clock.hy} class="hand" />
        <line x1="1120" y1="100" x2={clock.mx} y2={clock.my} class="hand min" />
      {/if}
      <circle cx="1120" cy="100" r="3" class="lamp {scene.machines.conductor.lamp}" />
      <text x="1120" y="148" text-anchor="middle" class="big">conductor</text>
      <text x="1120" y="162" text-anchor="middle" class="tiny" class:err={scene.machines.conductor.silent}
        >{scene.machines.conductor.label}</text>
    </g>

    <!-- the sidings: approach, dock, garage, arrivals — each a machine
         with its sign and its status line -->
    <g
      class="machine area"
      class:selected={selected === 'approach'}
      role="button"
      tabindex="0"
      aria-label="approach · {scene.machines.approach.label}"
      onclick={pick('approach')}
      onkeydown={pickKey('approach')}>
      <rect x="24" y={mainY - 50} width="170" height="96" class="hit" />
      <text x="30" y={mainY + 32}>Approach · forge heads</text>
      <text x="30" y={mainY + 44} class="tiny">{scene.machines.approach.label}</text>
    </g>
    <g
      class="machine area"
      class:selected={selected === 'dock'}
      role="button"
      tabindex="0"
      aria-label="loading dock · {scene.machines.dock.label}"
      onclick={pick('dock')}
      onkeydown={pickKey('dock')}>
      <rect x="404" y={mainY - 50} width="190" height="96" class="hit" />
      <text x="408" y={mainY + 32}>Loading dock</text>
      <text x="408" y={mainY + 44} class="tiny">{scene.machines.dock.label}</text>
    </g>
    <g
      class="machine area"
      class:selected={selected === 'garage'}
      role="button"
      tabindex="0"
      aria-label="garage · {scene.machines.garage.label}"
      onclick={pick('garage')}
      onkeydown={pickKey('garage')}>
      <rect x="420" y={mainY + 50} width="170" height="66" class="hit" />
      <text x="430" y={mainY + 100} class:err={scene.machines.garage.count > 0}>Garage · red cars</text>
      <text x="430" y={mainY + 112} class="tiny">{scene.machines.garage.label}</text>
    </g>
    <g
      class="machine area"
      class:selected={selected === 'arrivals'}
      role="button"
      tabindex="0"
      aria-label="arrivals · {scene.machines.arrivals.label}"
      onclick={pick('arrivals')}
      onkeydown={pickKey('arrivals')}>
      <rect x="1062" y={mainY - 50} width={VIEW_W - 1070} height={height - mainY + 40} class="hit" />
      <text x="1066" y={mainY + 32}>Arrivals</text>
      <text x="1066" y={mainY + 44} class="tiny">{scene.machines.arrivals.label}</text>
      {#if drawn.hidden > 0}
        <!-- the stack is capped; the departure board lists every landed car -->
        <rect x="1080" y={plateY - 2} width="144" height="14" class="plate" />
        <text x="1152" y={plateY + 8} text-anchor="middle" class="tiny">+{drawn.hidden} more landed · see the board</text>
      {/if}
    </g>

    <!-- tokens: keyed by id, moved by transform, so a station change
         slides the same node -->
    <g class="tokens">
      {#each drawn.drawn as w (w.id)}
        {@const [x, y] = wagonXY(w)}
        <g
          class="token wagon {w.tone}"
          class:landed={w.station === 'arrivals'}
          class:sim={w.sim}
          class:selected={selected === `car:${w.id}`}
          style="transform: translate({x}px, {y}px)"
          role="button"
          tabindex="0"
          aria-label="{w.title} — {w.status}"
          onclick={pick(`car:${w.id}`)}
          onkeydown={pickKey(`car:${w.id}`)}
          transition:fade={{ duration: 500 }}>
          <title>{w.title} — {w.branch}@{w.head ?? '—'}</title>
          <rect x="0" y="-10" width={WAGON_W} height="20" class="body" />
          <rect x="0" y="-10" width="5" height="20" class="stripe" />
          <circle cx="12" cy="12" r="3" class="wheel" />
          <circle cx={WAGON_W - 12} cy="12" r="3" class="wheel" />
          <text x="8" y="4">{w.tag}</text>
        </g>
      {/each}
      {#each scene.locos as l (l.id)}
        <g
          class="token loco"
          class:blocked={l.blocked !== null}
          class:selected={selected === `train:${l.id}`}
          style="transform: translate({locoX(l)}px, {mainY - 2}px)"
          role="button"
          tabindex="0"
          aria-label="{l.title}{l.blocked ? ` — ${l.blocked}` : ''}"
          onclick={pick(`train:${l.id}`)}
          onkeydown={pickKey(`train:${l.id}`)}
          transition:fade={{ duration: 500 }}>
          <title>{l.title}{l.n !== null ? ` — PR #${l.n}` : ''}</title>
          <rect x="0" y="-12" width="44" height="24" class="body" />
          <rect x="30" y="-20" width="12" height="8" class="body" />
          <circle cx="42" cy="-2" r="3" class="lamp-f" />
          <circle cx="10" cy="14" r="4" class="wheel" />
          <circle cx="34" cy="14" r="4" class="wheel" />
          <text x="6" y="3">{l.n !== null ? `#${l.n}` : 'PR'}</text>
        </g>
      {/each}
    </g>
  </svg>
</section>

<style>
  /* The map's own tokens are the app's (styles.css): the rails are the
     strong border, the ties the hairline, the ground the void. No new
     colour enters the system here. */
  .yard {
    --rail: var(--border-strong, #3a434d);
    --tie: var(--hairline, #2a3138);
    background: var(--void, #0d1014);
    border: 1px solid var(--hairline, #2a3138);
    padding: var(--s2, 8px);
    overflow-x: auto;
  }
  .yard svg {
    display: block;
    width: 100%;
    min-width: 900px;
    height: auto;
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .yard text {
    fill: var(--static, #7a838c);
    font-size: 10px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .yard text.big { font-size: 11px; fill: var(--fog, #e8ecef); letter-spacing: 0.06em; }
  .yard text.tiny { font-size: 9px; letter-spacing: 0.04em; text-transform: none; }
  .yard text.err { fill: var(--err, #e2685c); }
  .rail { stroke: var(--rail); stroke-width: 2; fill: none; }
  .tie { stroke: var(--tie); stroke-width: 6; stroke-dasharray: 3 9; fill: none; }
  .shed { fill: var(--ink, #12161c); stroke: var(--border-strong, #3a434d); }
  .shed.busy { stroke: var(--signal, #5fd4a8); }
  .shed.err { stroke: var(--err, #e2685c); }
  .shed.warn { stroke: var(--warn, #d9a441); }
  .barbg { fill: var(--void, #0d1014); stroke: var(--hairline, #2a3138); }
  .barfill { fill: var(--signal, #5fd4a8); }
  .barfill.warn { fill: var(--warn, #d9a441); }
  .hit { fill: transparent; }
  .machine { cursor: pointer; outline: none; }
  .machine:hover .shed, .machine:hover .post, .machine:focus-visible .shed { stroke: var(--fog, #e8ecef); }
  .machine.selected .shed { stroke: var(--signal, #5fd4a8); stroke-width: 2; }
  .machine.area.selected .hit, .machine.area:focus-visible .hit {
    stroke: var(--signal, #5fd4a8); stroke-dasharray: 3 4;
  }
  .sig-post { stroke: var(--rail); stroke-width: 2; }
  .sig-lamp { fill: var(--border-strong, #3a434d); }
  .sig-lamp.ok { fill: var(--ok, #4fb98a); }
  .sig-lamp.now { fill: var(--signal, #5fd4a8); animation: pulse 1.6s ease-in-out infinite; }
  .sig-lamp.err { fill: var(--err, #e2685c); animation: blink 1s steps(2) infinite; }
  .gear { transform-origin: center; transform-box: fill-box; fill: var(--static, #7a838c); font-size: 14px; }
  .gear.spin { animation: spin 2.4s linear infinite; fill: var(--signal, #5fd4a8); }
  .smoke { fill: var(--static, #7a838c); opacity: 0; transform-box: fill-box; transform-origin: center; }
  .smoke.on { animation: puff 2.2s ease-out infinite; }
  .smoke.on:nth-of-type(2) { animation-delay: 0.7s; }
  .smoke.on:nth-of-type(3) { animation-delay: 1.4s; }
  .tower-lamp { fill: var(--border-strong, #3a434d); }
  .tower-lamp.ok { fill: var(--ok, #4fb98a); filter: drop-shadow(0 0 5px var(--ok, #4fb98a)); }
  .tower-lamp.err { fill: var(--err, #e2685c); filter: drop-shadow(0 0 6px var(--err, #e2685c)); animation: blink 1s steps(2) infinite; }
  .plate { fill: var(--ink, #12161c); stroke: var(--hairline, #2a3138); }
  .yard text.warn { fill: var(--warn, #d9a441); }
  .hand { stroke: var(--fog, #e8ecef); stroke-width: 2; stroke-linecap: round; }
  .hand.min { stroke: var(--signal, #5fd4a8); }
  /* The small lamps: the conductor's centre dot wears its liveness. */
  .lamp { fill: var(--border-strong, #3a434d); }
  .lamp.ok { fill: var(--ok, #4fb98a); }
  .lamp.working { fill: var(--signal, #5fd4a8); animation: pulse 1.6s ease-in-out infinite; }
  .lamp.warn { fill: var(--warn, #d9a441); }
  .lamp.err { fill: var(--err, #e2685c); animation: blink 1s steps(2) infinite; }

  /* Tokens move by transform; the transition is the slide. */
  .token {
    transition: transform 1.4s cubic-bezier(0.4, 0, 0.2, 1);
    cursor: pointer;
    outline: none;
  }
  .wagon rect.body { fill: var(--ink-raised, #171c24); stroke: var(--border-strong, #3a434d); }
  .wagon rect.stripe { fill: var(--signal, #5fd4a8); }
  .wagon.red rect.stripe { fill: var(--err, #e2685c); }
  .wagon.warn rect.stripe { fill: var(--warn, #d9a441); }
  .wagon.static rect.stripe { fill: var(--border-strong, #3a434d); }
  .wagon.landed rect.body { opacity: 0.55; }
  .wagon.sim rect.body { stroke-dasharray: 3 2; }
  .wagon text { fill: var(--fog, #e8ecef); font-size: 9px; letter-spacing: 0; text-transform: none; }
  .wagon.selected rect.body, .wagon:hover rect.body, .wagon:focus-visible rect.body {
    stroke: var(--signal, #5fd4a8); stroke-width: 1.5;
  }
  .wheel { fill: var(--rail); }
  .loco rect.body { fill: var(--ink-raised, #171c24); stroke: var(--fog, #e8ecef); }
  .loco .lamp-f { fill: var(--warn, #d9a441); }
  .loco.blocked rect.body { stroke: var(--err, #e2685c); }
  .loco.selected rect.body, .loco:hover rect.body, .loco:focus-visible rect.body {
    stroke: var(--signal, #5fd4a8); stroke-width: 1.5;
  }
  .loco text { fill: var(--fog, #e8ecef); font-size: 9px; letter-spacing: 0; text-transform: none; }

  @keyframes pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.45; } }
  @keyframes blink { 50% { opacity: 0.25; } }
  @keyframes spin { to { transform: rotate(360deg); } }
  @keyframes puff {
    0% { opacity: 0.5; transform: translate(0, 0) scale(0.6); }
    100% { opacity: 0; transform: translate(6px, -28px) scale(1.6); }
  }

  /* A viewer who asked for less motion keeps the layout and the lamps'
     colours; the loops and the slide are dropped. */
  @media (prefers-reduced-motion: reduce) {
    .lamp, .sig-lamp, .gear, .smoke, .token { animation: none !important; transition: none !important; }
  }
</style>
