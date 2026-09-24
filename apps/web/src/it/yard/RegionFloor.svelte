<svelte:options namespace="svg" />

<script lang="ts">
  // ONE REGION'S SLICE OF THE YARD FLOOR (design fe77a1d2, car 2;
  // backlog c0565f48).
  //
  // Until this car /it/yard/<region> stacked two maps: the region map
  // with one plate per wagon, which could not be clicked, and under it
  // YardMap — the WHOLE six-region floor, every region's wagons again.
  // This is YardMap's drawing cut along the region lines car 1 laid the
  // floor out by: the dock draws its stretch of the mainline and its
  // wagons, the track its mainline, signals, locomotives and the
  // conductor's clock, the shed its probe lanes. RegionMap mounts it in
  // place of the plates, and nothing draws the whole floor any more.
  //
  // WHERE things stand is decided in floor-slices.ts (`regionFloorView`:
  // the slice, the frame it hangs off, and how far it is lifted to the
  // top of the region's canvas). This draws; it never places a token.
  //
  // Every machine and every token is a button, as on the floor: clicking
  // (or Enter / Space on) any of them selects it, and the entity panel
  // under the map shows its facts and the verbs that apply. The map
  // draws; it never decides.
  //
  // Tokens are keyed by id and moved by a CSS transform, so a wagon
  // whose place changes WITHIN the region slides there. One leaving the
  // region leaves this map and appears on the next region's (design
  // fe77a1d2 Q1); the world's borders carry the crossing.
  import { fade } from 'svelte/transition';
  import { ARRIVALS_DRAWN, STAGES, type Bay, type Scene } from './yard-floor';
  import { DELIVERY_CHANNELS, type DeliveryChannel } from './yard';
  import { clusterLabel, runnerLabel, runnerProgress } from './yard-machines';
  import {
    LANE_ROW_H,
    QUEUE_ROW_H,
    STAGE_X,
    VIEW_W,
    WAGON_W,
    bayY,
    machineAt,
    queueY as queueRowY,
    sidingY as sidingRowY,
    type RegionFloorView,
  } from './floor-slices';

  type Props = Readonly<{
    view: RegionFloorView;
    scene: Scene;
    /** The selection key (`car:<id>`, `train:<id>`, `bay:<n>`, a machine). */
    selected: string;
    onselect: (key: string) => void;
  }>;
  let { view, scene, selected, onselect }: Props = $props();

  // ---- the names the scenery has always read, off the view's frame ----
  const region = $derived(view.slice.region);
  const frame = $derived(view.frame);
  const nBays = $derived(scene.bays.length);
  const queueRows = $derived(frame.queueRows);
  const queueRowIndexes = $derived(Array.from({ length: queueRows }, (_, i) => i));
  const queueTop = $derived(frame.queueTop);
  const queueY = (row: number): number => queueRowY(frame, row);
  const mainY = $derived(frame.mainY);
  const sidingY = (i: number): number => sidingRowY(frame, i);
  const cancelledY = $derived(frame.cancelledY);
  const onSiding = (ch: DeliveryChannel) => scene.wagons.filter(w => w.station === 'arrivals' && w.siding === ch);
  /** How many landed wagons the siding's plate stands for. */
  const sidingHidden = (ch: DeliveryChannel): number => Math.max(0, onSiding(ch).length - ARRIVALS_DRAWN);
  const shedY = $derived(frame.shedY);
  const eventY = $derived(frame.eventY);
  const noProbeY = $derived(frame.noProbeY);
  const laneBottom = $derived(frame.laneBottom);
  const shed = $derived(scene.machines.inspection);
  const at = $derived(machineAt(frame));
  /** The deploy runner's housing and the cluster tower, by top-left. */
  const rn = $derived(at.runner);
  const tw = $derived(at.cluster);
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
    const c = at.conductor;
    return {
      hx: c.x + 16 * Math.sin(ha),
      hy: c.y - 16 * Math.cos(ha),
      mx: c.x + 24 * Math.sin(ma),
      my: c.y - 24 * Math.cos(ma),
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

<!-- A stretch of the mainline: the ties, and the two rails. Each region
     draws the stretch its rails join, so a siding never starts in air. -->
{#snippet mainline(x1: number, x2: number)}
  <line {x1} y1={mainY} {x2} y2={mainY} class="tie" />
  <line {x1} y1={mainY - 4} {x2} y2={mainY - 4} class="rail" />
  <line {x1} y1={mainY + 4} {x2} y2={mainY + 4} class="rail" />
{/snippet}

<g class="floor" data-floor={region} transform="translate(0 {view.dy})">
  {#if region === 'gates'}
    <!-- THE GATES: the approach siding along the mainline, the branch
         to each gate shed, and the queue lane when a run waits -->
    {@render mainline(20, 400)}
    {#each view.slice.bays as p (p.bay.index)}
      <path
        d="M200 {mainY} C 215 {mainY}, 210 {p.y + 18}, 228 {p.y + 18} L 372 {p.y + 18}"
        class="rail" />
    {/each}
    <!-- the queue lane's rails, and the connector taking it into the
         gate branch: a queued run is next through those sheds -->
    {#each queueRowIndexes as r (r)}
      <line x1="24" y1={queueY(r) + 6} x2="200" y2={queueY(r) + 6} class="rail" />
    {/each}
    {#if queueRows > 0 && nBays > 0}
      <path
        d="M200 {queueY(0) + 6} C 214 {queueY(0) + 6}, 210 {bayY(nBays - 1) + 18}, 228 {bayY(nBays - 1) + 18}"
        class="rail" />
    {/if}
    <text x="228" y="40">Gate runners · {nBays > 0 ? `${nBays} bays` : 'no reading'}</text>

    {#each view.slice.bays as p (p.bay.index)}
      {@const b = p.bay}
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
          y={p.y - 8}
          width="150"
          height="40"
          class="shed"
          class:busy={b.busy && !b.stale}
          class:warn={b.stale} />
        <rect x="234" y={p.y + 26} width="120" height="3" class="barbg" />
        <rect
          x="234"
          y={p.y + 26}
          width={Math.round(120 * b.progress)}
          height="3"
          class="barfill"
          class:warn={b.stale} />
        <text x="360" y={p.y + 8} class="gear" class:spin={b.busy}>✳</text>
        <text x="234" y={p.y - 12} class="tiny">{bayLabel(b)}</text>
      </g>
    {/each}

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
    <!-- the queue lane: drawn only when a run is waiting for a bay, and
         deliberately neutral — a queue is the pipeline working to its
         bound, not an alarm -->
    {#if queueRows > 0}
      <g
        class="machine area"
        class:selected={selected === 'gate-queue'}
        role="button"
        tabindex="0"
        aria-label="gate queue · {scene.machines.queue.label}"
        onclick={pick('gate-queue')}
        onkeydown={pickKey('gate-queue')}>
        <rect x="24" y={queueTop - 22} width="360" height={queueRows * QUEUE_ROW_H + 20} class="hit" />
        <text x="30" y={queueTop - 10}>Gate queue · waiting for a bay</text>
        <text x="230" y={queueTop - 10} class="tiny">{scene.machines.queue.label}</text>
      </g>
    {/if}
  {:else if region === 'dock'}
    <!-- THE LOADING DOCK: its stretch of the mainline, where parked cars
         stand until the conductor boards them -->
    {@render mainline(396, 600)}
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
  {:else if region === 'garage'}
    <!-- THE GARAGE: the siding red cars are shunted onto, turning out
         of the dock's stretch of the mainline -->
    {@render mainline(396, 600)}
    <path d="M400 {mainY} C 420 {mainY}, 410 {mainY + 70}, 430 {mainY + 70} L 590 {mainY + 70}" class="rail" />
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
  {:else if region === 'track'}
    <!-- THE TRACK: the whole mainline, one signal per stage, the trains
         on it and the conductor that boards them -->
    {@render mainline(20, VIEW_W - 20)}
    <text x="620" y={mainY + 32}>The track · one train at a time</text>
    <!-- the signals along the track: one machine, one lamp per stage.
         The last one, `proven`, is the exception — no locomotive reaches
         it, so the inspection shed lights it (yard-floor.ts). -->
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
    <!-- the conductor's clock -->
    <g
      class="machine"
      class:selected={selected === 'conductor'}
      role="button"
      tabindex="0"
      aria-label="conductor · {scene.machines.conductor.label}"
      onclick={pick('conductor')}
      onkeydown={pickKey('conductor')}>
      <circle cx={at.conductor.x} cy={at.conductor.y} r="30" class="shed" class:err={scene.machines.conductor.silent} />
      {#if clock}
        <line x1={at.conductor.x} y1={at.conductor.y} x2={clock.hx} y2={clock.hy} class="hand" />
        <line x1={at.conductor.x} y1={at.conductor.y} x2={clock.mx} y2={clock.my} class="hand min" />
      {/if}
      <circle cx={at.conductor.x} cy={at.conductor.y} r="3" class="lamp {scene.machines.conductor.lamp}" />
      <text x={at.conductor.x} y={at.conductor.y + 48} text-anchor="middle" class="big">conductor</text>
      <text x={at.conductor.x} y={at.conductor.y + 62} text-anchor="middle" class="tiny" class:err={scene.machines.conductor.silent}
        >{scene.machines.conductor.label}</text>
    </g>
  {:else if region === 'arrivals'}
    <!-- THE ARRIVALS YARD: the turnout off the mainline's end, the four
         channel sidings, the cancelled siding, and the machines that
         deploy what landed and report what the cluster runs -->
    {@render mainline(1000, VIEW_W - 20)}
    <path d="M1060 {mainY} C 1075 {mainY}, 1070 {sidingY(0)}, 1085 {sidingY(0)}" class="rail" />

    <!-- the deploy-runner shed: read off the newest converge ops-request
         (yard-machines.ts) composed with the run's own maintenance
         packet (yard-converge.ts). It smokes while the converge runs;
         dark and "no reading" until the page has read the packets -->
    <g
      class="machine"
      class:selected={selected === 'runner'}
      role="button"
      tabindex="0"
      aria-label="deploy runner · {runnerText}"
      onclick={pick('runner')}
      onkeydown={pickKey('runner')}>
      <rect
        x={rn.x}
        y={rn.y}
        width="130"
        height="46"
        class="shed"
        class:busy={runner.kind === 'running'}
        class:warn={runner.kind === 'requested'}
        class:err={runner.kind === 'failed'} />
      <rect x={rn.x + 10} y={rn.y - 20} width="10" height="22" class="shed" />
      <circle cx={rn.x + 15} cy={rn.y - 24} r="5" class="smoke" class:on={runner.kind === 'running'} />
      <circle cx={rn.x + 15} cy={rn.y - 24} r="5" class="smoke" class:on={runner.kind === 'running'} />
      <circle cx={rn.x + 15} cy={rn.y - 24} r="5" class="smoke" class:on={runner.kind === 'running'} />
      <text x={rn.x + 8} y={rn.y + 16} class="big">deploy runner</text>
      <text x={rn.x + 8} y={rn.y + 30} class="tiny" class:err={runner.kind === 'failed'} class:warn={runner.kind === 'requested'}>{runnerShort}</text>
      <rect x={rn.x + 8} y={rn.y + 36} width="112" height="3" class="barbg" />
      <rect x={rn.x + 8} y={rn.y + 36} width={Math.round(112 * runnerBar)} height="3" class="barfill" />
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
      <rect x={tw.x} y={tw.y} width="60" height="76" class="shed" class:err={cluster.kind === 'dark'} />
      <circle cx={tw.x + 30} cy={tw.y + 22} r="9" class="tower-lamp" class:ok={cluster.kind === 'ready'} class:err={cluster.kind === 'dark'} />
      <text x={tw.x + 30} y={tw.y + 48} text-anchor="middle" class="big">cluster</text>
      <!-- two short lines fit the tower: the state, then the build -->
      <text x={tw.x + 30} y={tw.y + 62} text-anchor="middle" class="tiny" class:err={cluster.kind === 'dark'}
        >{cluster.kind === 'ready' ? 'ready' : clusterLabel(cluster)}</text>
      {#if cluster.kind === 'ready' && cluster.commit !== null}
        <text x={tw.x + 30} y={tw.y + 73} text-anchor="middle" class="tiny">{cluster.commit.slice(0, 7)}</text>
      {/if}
    </g>

    <g
      class="machine area"
      class:selected={selected === 'arrivals'}
      role="button"
      tabindex="0"
      aria-label="arrivals · {scene.machines.arrivals.label}"
      onclick={pick('arrivals')}
      onkeydown={pickKey('arrivals')}>
      <!-- the sign's corner plus the four sidings, one area; bounded
           above the cancelled siding, which is its own machine — an
           area that swallowed its clicks would make it unselectable -->
      <path
        d="M600 {sidingY(0) - 24} H1062 V{mainY + 20} H{VIEW_W - 16} V{sidingY(DELIVERY_CHANNELS.length - 1) + 22} H600 Z"
        class="hit" />
      <text x={VIEW_W - 16} y={mainY + 32} text-anchor="end">Arrivals · by channel</text>
      <text x={VIEW_W - 16} y={mainY + 44} text-anchor="end" class="tiny">{scene.machines.arrivals.label}</text>
      <!-- the ladder: siding 0 runs from the turnout; each next siding
           hangs off the one above at the left end -->
      {#each DELIVERY_CHANNELS as ch, i (ch)}
        {@const y = sidingY(i)}
        {#if i > 0}
          <path d="M668 {sidingY(i - 1)} C 652 {sidingY(i - 1)}, 648 {y}, 632 {y}" class="rail" />
        {/if}
        <line x1="632" y1={y} x2={VIEW_W - 15} y2={y} class="rail" />
        <text x="634" y={y - 14} class="tiny">{ch}</text>
        {#if sidingHidden(ch) > 0}
          <!-- the siding is capped; the departure board lists every landed car -->
          <rect x="1088" y={y - 8} width="136" height="14" class="plate" />
          <text x="1156" y={y + 2} text-anchor="middle" class="tiny">+{sidingHidden(ch)} more · see the board</text>
        {/if}
      {/each}
    </g>
    <!-- THE CANCELLED SIDING — withdrawn cars, the third terminal track
         (design c6bd173e, outcomes). Off the ladder below the arrivals,
         a step apart: a withdrawal is settled, and it is not a landing. -->
    <g
      class="machine area"
      class:selected={selected === 'cancelled'}
      role="button"
      tabindex="0"
      aria-label="cancelled siding · {scene.machines.cancelled.label}"
      onclick={pick('cancelled')}
      onkeydown={pickKey('cancelled')}>
      <rect x="600" y={cancelledY - 24} width={VIEW_W - 616} height="46" class="hit" />
      <path
        d="M668 {sidingY(DELIVERY_CHANNELS.length - 1)} C 652 {sidingY(DELIVERY_CHANNELS.length - 1)}, 648 {cancelledY}, 632 {cancelledY}"
        class="rail" />
      <line x1="632" y1={cancelledY} x2={VIEW_W - 15} y2={cancelledY} class="rail" />
      <text x="634" y={cancelledY - 14} class="tiny">cancelled · {scene.machines.cancelled.label}</text>
    </g>
  {:else}
    <!-- THE INSPECTION SHED — fed by the arrivals sidings. A car that
         landed carrying a probe stands here until the forge's
         `run-car-probe` request is drained and the `proven` step is
         stamped; the two sidings under it hold the cars no probe can
         settle. Everything drawn is a packet field (yard-shed.ts) — the
         yard invents nothing here, and draws no count the record does
         not hold. -->
    <g
      class="machine area"
      class:selected={selected === 'inspection-shed'}
      role="button"
      tabindex="0"
      aria-label="inspection shed · {shed.label}"
      onclick={pick('inspection-shed')}
      onkeydown={pickKey('inspection-shed')}>
      <rect x="24" y={shedY - 22} width={VIEW_W - 40} height={laneBottom - shedY + 26} class="hit" />
      <!-- the ladder continues down from the cancelled siding to the
           inspection lane, and one rail per lane -->
      <path d="M668 {cancelledY} C 652 {cancelledY}, 648 {shedY + 14}, 632 {shedY + 14}" class="rail" />
      <path d="M668 {shedY + 14} C 652 {shedY + 14}, 648 {eventY + 14}, 632 {eventY + 14} L {VIEW_W - 15} {eventY + 14}" class="rail" />
      <path d="M668 {eventY + 14} C 652 {eventY + 14}, 648 {noProbeY + 14}, 632 {noProbeY + 14} L {VIEW_W - 15} {noProbeY + 14}" class="rail" />
      <line x1="632" y1={shedY + 14} x2={VIEW_W - 15} y2={shedY + 14} class="rail" />
      <!-- the shed building over the inspection lane -->
      <rect
        x="620"
        y={shedY - 14}
        width={VIEW_W - 628}
        height={frame.shedRows * LANE_ROW_H + 6}
        class="shed"
        class:busy={shed.inspecting > 0 && shed.failed === 0}
        class:err={shed.failed > 0} />
      <text x="30" y={shedY - 2} class:err={shed.failed > 0}>Inspection shed · proven?</text>
      <text x="30" y={shedY + 12} class="tiny" class:err={shed.failed > 0}>{shed.label}</text>
      <text x="30" y={eventY + 10}>Siding · on an event</text>
      <text x="30" y={eventY + 22} class="tiny"
        >{shed.onEvent === 0 ? 'empty' : `${shed.onEvent} waiting — no probe can settle it`}</text>
      <text x="30" y={noProbeY + 10}>Siding · no probe</text>
      <text x="30" y={noProbeY + 22} class="tiny"
        >{shed.noProbe === 0 ? 'empty' : `${shed.noProbe} carrying no probe — a person must prove them`}</text>
    </g>
  {/if}

  <!-- tokens: keyed by id, moved by transform, so a change of place
       inside the region slides the same node -->
  <g class="tokens">
    {#each view.slice.wagons as p (p.wagon.id)}
      {@const w = p.wagon}
      <g
        class="token wagon {w.tone}"
        class:landed={w.station === 'arrivals' || w.station === 'cancelled'}
        class:sim={w.sim}
        class:train-gate={w.kind === 'train-gate'}
        class:selected={selected === `car:${w.id}`}
        style="transform: translate({p.x}px, {p.y}px)"
        role="button"
        tabindex="0"
        aria-label="{w.title} — {w.status}"
        data-car={w.id}
        data-station={w.station}
        onclick={pick(`car:${w.id}`)}
        onkeydown={pickKey(`car:${w.id}`)}
        transition:fade={{ duration: 500 }}>
        <!-- A wagon standing in the inspection shed shows the probe
             command and the string it must print; the entity panel
             carries them unwrapped. A wagon in the garage shows the
             line its check failed on (6730dccb). -->
        <title
          >{w.title} — {w.branch}@{w.head ?? '—'}{w.probe
            ? `\nprobe: ${w.probe.command}\nmust print: ${w.probe.expect ?? '(nothing recorded)'}`
            : ''}{w.event ? `\nwaiting on: ${w.event}` : ''}{w.why ? `\nfailed on: ${w.why}` : ''}</title>
        <rect x="0" y="-10" width={WAGON_W} height="20" class="body" />
        <rect x="0" y="-10" width="5" height="20" class="stripe" />
        <circle cx="12" cy="12" r="3" class="wheel" />
        <circle cx={WAGON_W - 12} cy="12" r="3" class="wheel" />
        <text x="8" y="4">{w.tag}</text>
      </g>
    {/each}
    {#each view.slice.locos as p (p.loco.id)}
      {@const l = p.loco}
      <g
        class="token loco"
        class:blocked={l.blocked !== null}
        class:selected={selected === `train:${l.id}`}
        style="transform: translate({p.x}px, {p.y}px)"
        role="button"
        tabindex="0"
        aria-label="{l.title}{l.channel ? ` — ${l.channel} train` : ''}{l.blocked ? ` — ${l.blocked}` : ''}"
        data-train={l.id}
        onclick={pick(`train:${l.id}`)}
        onkeydown={pickKey(`train:${l.id}`)}
        transition:fade={{ duration: 500 }}>
        <title>{l.title}{l.channel ? ` — ${l.channel} train` : ''}{l.n !== null ? ` — PR #${l.n}` : ''}</title>
        <!-- THE CHANNEL PLATE — how this train ships, the heaviest of its
             cars' (the conductor's stamp at board, cffef553). A train
             boarded before the stamp carries no plate: nothing drawn,
             never 'software' guessed. -->
        {#if l.channel}
          <text x="6" y="-26" class="plate">{l.channel} train</text>
        {/if}
        <rect x="0" y="-12" width="44" height="24" class="body" />
        <rect x="30" y="-20" width="12" height="8" class="body" />
        <circle cx="42" cy="-2" r="3" class="lamp-f" />
        <circle cx="10" cy="14" r="4" class="wheel" />
        <circle cx="34" cy="14" r="4" class="wheel" />
        <text x="6" y="3">{l.n !== null ? `#${l.n}` : 'PR'}</text>
      </g>
    {/each}
  </g>
</g>

<style>
  /* The floor's classes, as YardMap declared them, in the map's own
     --map-* grammar with no fallback (42f66fb3, map-palette.test.ts):
     the rails are the strong rule, the ties the hairline, bodies the
     map's surface, the accent what YardMap called the signal. No new
     colour enters the system here; the redesign re-skins the tokens. */
  .floor {
    --rail: var(--map-rule-strong);
    --tie: var(--map-rule);
  }
  .floor text {
    fill: var(--map-muted);
    font-size: 10px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .floor text.big { font-size: 11px; fill: var(--map-ink); letter-spacing: 0.06em; }
  .floor text.tiny { font-size: 9px; letter-spacing: 0.04em; text-transform: none; }
  .floor text.err { fill: var(--map-bad-ink); }
  .floor text.warn { fill: var(--map-warn-ink); }
  .rail { stroke: var(--rail); stroke-width: 2; fill: none; }
  .tie { stroke: var(--tie); stroke-width: 6; stroke-dasharray: 3 9; fill: none; }
  .shed { fill: var(--map-surface); stroke: var(--map-rule-strong); }
  .shed.busy { stroke: var(--map-accent); }
  .shed.err { stroke: var(--map-bad-edge); }
  .shed.warn { stroke: var(--map-warn-edge); }
  .barbg { fill: var(--map-bg); stroke: var(--map-rule); }
  .barfill { fill: var(--map-accent); }
  .barfill.warn { fill: var(--map-warn-edge); }
  .hit { fill: transparent; }
  .machine { cursor: pointer; outline: none; }
  .machine:hover .shed, .machine:hover .post, .machine:focus-visible .shed { stroke: var(--map-ink); }
  .machine.selected .shed { stroke: var(--map-accent); stroke-width: 2; }
  .machine.area.selected .hit, .machine.area:focus-visible .hit {
    stroke: var(--map-accent); stroke-dasharray: 3 4;
  }
  .sig-post { stroke: var(--rail); stroke-width: 2; }
  .sig-lamp { fill: var(--map-rule-strong); }
  .sig-lamp.ok { fill: var(--map-ok-edge); }
  .sig-lamp.now { fill: var(--map-accent); animation: pulse 1.6s ease-in-out infinite; }
  .sig-lamp.err { fill: var(--map-bad-edge); animation: blink 1s steps(2) infinite; }
  .gear { transform-origin: center; transform-box: fill-box; fill: var(--map-muted); font-size: 14px; }
  .gear.spin { animation: spin 2.4s linear infinite; fill: var(--map-accent); }
  .smoke { fill: var(--map-muted); opacity: 0; transform-box: fill-box; transform-origin: center; }
  .smoke.on { animation: puff 2.2s ease-out infinite; }
  .smoke.on:nth-of-type(2) { animation-delay: 0.7s; }
  .smoke.on:nth-of-type(3) { animation-delay: 1.4s; }
  .tower-lamp { fill: var(--map-rule-strong); }
  .tower-lamp.ok { fill: var(--map-ok-edge); filter: drop-shadow(0 0 5px var(--map-ok-edge)); }
  .tower-lamp.err { fill: var(--map-bad-edge); filter: drop-shadow(0 0 6px var(--map-bad-edge)); animation: blink 1s steps(2) infinite; }
  rect.plate { fill: var(--map-surface); stroke: var(--map-rule); }
  .hand { stroke: var(--map-ink); stroke-width: 2; stroke-linecap: round; }
  .hand.min { stroke: var(--map-accent); }
  /* The small lamps: the conductor's centre dot wears its liveness. */
  .lamp { fill: var(--map-rule-strong); }
  .lamp.ok { fill: var(--map-ok-edge); }
  .lamp.working { fill: var(--map-accent); animation: pulse 1.6s ease-in-out infinite; }
  .lamp.warn { fill: var(--map-warn-edge); }
  .lamp.err { fill: var(--map-bad-edge); animation: blink 1s steps(2) infinite; }

  /* Tokens move by transform; the transition is the slide. */
  .token {
    transition: transform 1.4s cubic-bezier(0.4, 0, 0.2, 1);
    cursor: pointer;
    outline: none;
  }
  .wagon rect.body { fill: var(--map-surface); stroke: var(--map-rule-strong); }
  .wagon rect.stripe { fill: var(--map-accent); }
  .wagon.red rect.stripe { fill: var(--map-bad-edge); }
  .wagon.warn rect.stripe { fill: var(--map-warn-edge); }
  .wagon.static rect.stripe { fill: var(--map-rule-strong); }
  .wagon.landed rect.body { opacity: 0.55; }
  .wagon.sim rect.body { stroke-dasharray: 3 2; }
  /* A TRAIN's gate in a bay is drawn in the locomotive's livery — the
     train under test, not a PR car (128b5496; asked twice 2026-09-14). */
  .wagon.train-gate rect.body { stroke: var(--map-ink); }
  .wagon.train-gate rect.stripe { fill: var(--map-ink); }
  .wagon.train-gate text { font-weight: 600; }
  .floor .wagon text { fill: var(--map-ink); font-size: 9px; letter-spacing: 0; text-transform: none; }
  .wagon.selected rect.body, .wagon:hover rect.body, .wagon:focus-visible rect.body {
    stroke: var(--map-accent); stroke-width: 1.5;
  }
  .wheel { fill: var(--rail); }
  .loco rect.body { fill: var(--map-surface); stroke: var(--map-ink); }
  .loco .lamp-f { fill: var(--map-warn-edge); }
  .loco.blocked rect.body { stroke: var(--map-bad-edge); }
  .loco.selected rect.body, .loco:hover rect.body, .loco:focus-visible rect.body {
    stroke: var(--map-accent); stroke-width: 1.5;
  }
  .floor .loco text { fill: var(--map-ink); font-size: 9px; letter-spacing: 0; text-transform: none; }
  /* The channel plate rides above the cab in the signals' muted ink — a
     reading, not livery. */
  .floor .loco text.plate { fill: var(--map-muted); letter-spacing: 0.04em; }

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
