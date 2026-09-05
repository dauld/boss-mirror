<script lang="ts">
  // The train yard — the IT department's front door (departure-
  // board.md Q1, David's call: guest-visible, the IT app's landing).
  // A queue lens in the departure-board idiom: every row is a Job
  // the conductor writes; nothing here is new state. Reads are
  // audit-readonly-safe by construction.
  import { onMount } from 'svelte';
  import {
    disciplineLabel,
    dockUpstream,
    fetchYard,
    splitAtDeparture,
    wipAdvisory,
    type Eta,
    type EtaPhase,
    type YardState,
    type TrainRow, troubleLabel } from './yard';
  import {
    fetchYardStatus,
    gateSlots,
    type YardStatus,
  } from './yard-status';
  import type { Remote } from '../../data/remote';
  import PacketCard from '@boss/web-kit/ui/PacketCard.svelte';
  import PacketModal, { type PacketJob } from '@boss/web-kit/ui/PacketModal.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import { navigate } from '@boss/web-kit/nav';

  let yard = $state<YardState | null>(null);
  let loading = $state(true);

  // The gate slots + garage ride the server-computed read-model, whose
  // capacity is the delivery policy's gate_max_concurrent — no constant
  // baked into this page. Held as a Remote so an outage renders honestly
  // (the same deserialize-once pattern the Yard-status page uses) rather
  // than a false-empty approach.
  let status = $state<Remote<YardStatus>>({ kind: 'loading' });
  const slots = $derived(status.kind === 'ready' ? gateSlots(status.data.gates) : []);
  const garage = $derived(status.kind === 'ready' ? status.data.garage : []);

  // The condensed packet panel (David, fc67bed2). The dock rows are a
  // slim projection — no steps, no metadata — so opening a packet
  // fetches the Job. Holding the id rather than the row also means the
  // 10s poll cannot swap the panel's contents underneath a read.
  let packetId = $state<string | null>(null);
  let packet = $state<PacketJob | null>(null);
  let packetLoading = $state(false);
  let packetError = $state<string | null>(null);

  async function openPacket(id: string): Promise<void> {
    // Open first, fill in after: waiting on a round trip before the
    // panel appears reads as a dropped double-click.
    packetId = id;
    packet = null;
    packetError = null;
    packetLoading = true;
    try {
      const r = await fetch(`/api/jobs/${id}`);
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const body = (await r.json()) as PacketJob;
      // A second double-click while this was in flight wins; dropping
      // the stale response stops it overwriting the newer packet.
      if (packetId === id) packet = body;
    } catch (e) {
      if (packetId === id) {
        packetError = `Could not load the packet — ${e instanceof Error ? e.message : String(e)}`;
      }
    } finally {
      if (packetId === id) packetLoading = false;
    }
  }

  function closePacket(): void {
    packetId = null;
    packet = null;
    packetError = null;
    packetLoading = false;
  }

  // The dock's walk upstream, when its registry row declares one.
  // Nothing station-specific lives here: the row says where upstream
  // is, so a station that names a different queue tomorrow moves this
  // button with it, and one that names none renders nothing.
  const upstream = $derived(yard ? dockUpstream(yard.dockStation) : null);
  // The departure line is the merge (0bba59f7): pre-merge trains are
  // yard work where red is status, post-merge is transit where green
  // holds by construction.
  const split = $derived(
    yard ? splitAtDeparture(yard.inFlight) : { inYard: [], inTransit: [] },
  );

  onMount(() => {
    let cancelled = false;
    async function tick() {
      const [y, s] = await Promise.all([fetchYard(), fetchYardStatus()]);
      if (cancelled) return;
      if (y) yard = y;
      status = s;
      loading = false;
    }
    tick();
    const t = setInterval(tick, 10_000);
    return () => {
      cancelled = true;
      clearInterval(t);
    };
  });

  function stampOf(t: TrainRow): string {
    if (t.status === 'ARRIVED' && t.deployed) return t.deployed;
    if (t.status === 'DEPARTED' && t.mergeRef) return `merged ${t.mergeRef}`;
    return '';
  }

  const clock = (ms: number) =>
    new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });

  // What the train is waiting on, when there is no honest time to give.
  const PHASE_LABEL: Record<EtaPhase, string> = {
    boarding: 'boarding',
    ci: 'CI running',
    merging: 'awaiting merge',
    deploying: 'deploying',
    blocked: 'CI red',
    arrived: 'arrived',
  };

  // Always `~`: this is a median of what recent trains did, not a
  // promise about this one.
  const etaText = (e: Eta) =>
    e.kind === 'eta' ? `ETA ~${clock(e.atMs)}` : PHASE_LABEL[e.phase];
  const etaTitle = (e: Eta) =>
    e.kind === 'eta'
      ? `estimate — ${e.basis}`
      : 'no estimate yet — not enough recent arrivals with usable timestamps';

  // The arrival instant, shown at the granularity of its evidence: a
  // train whose only stamp is a date gets a date, never an invented
  // clock time.
  function arrivalText(t: TrainRow): string {
    const a = t.arrivedAt;
    if (a.at === '') return '—';
    if (a.basis !== 'completed_at') return a.at;
    return new Date(a.ms).toLocaleString([], {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    });
  }
  const arrivalTitle = (t: TrainRow) =>
    t.arrivedAt.at === '' ? 'no arrival stamp' : `${t.arrivedAt.at} · ${t.arrivedAt.basis}`;

  // Same affordance PacketCard carries: double-click, or Enter on the
  // row's link. The row leads to the train's Job — where the landing
  // report is.
  const trainHref = (t: TrainRow) => entityHref('job', t.id);
  const openTrain = (t: TrainRow) => navigate(trainHref(t));
</script>

<div class="theme-exec yard-root">
  <PageHeader
    eyebrow="IT · Forge line"
    title="The train yard"
    subtitle="Gated → parked → boarded → departed → arrived → proven — the pipeline's queues, in the order a change travels"
  />

  {#if loading}
    <div class="yard-empty">Reading the yard…</div>
  {:else if !yard}
    <div class="yard-empty">The yard is unreachable right now.</div>
  {:else}
    <!--
      THE SCOREBOARD LEADS. David, 2026-08-28: "We should have these
      stats at the top of the Train Yard if they are what matter." The
      yard opened with trains and the dock — useful, but they show what
      is moving right now rather than whether delivery is getting better
      or worse. A statistic nobody sees cannot discipline a decision.

      Rendered only when a version has actually RESOLVED something, so
      the panel is absent rather than showing zeros for a version whose
      packets are all still in flight.
    -->
    <!-- SECTION ORDER IS THE PROTOCOL'S ORDER (David, feedback 7d31e246:
         "rearrange the Train Yard to flow in protocol-sequence order").
         A change travels approach → dock → yard → transit → arrival →
         proof, and the page reads top to bottom in exactly that
         sequence, after the scoreboard. The numbering is the flow; a
         block moved out of sequence is a wrong page, and
         yard-page-order.test.ts pins the headings' order. -->

    {#if yard.delivery.length > 0}
      <div class="yard-section">00 — DELIVERY</div>
      <div class="yard-scoreboard">
        {#each yard.delivery as stat (stat.label)}
          <div class="yard-stat" class:is-provisional={stat.provisional}>
            <div class="yard-stat-v">{stat.value}</div>
            <div class="yard-stat-l">{stat.label}</div>
            <div class="yard-stat-p">
              {#if stat.previous}prev {stat.previous} · {/if}n={stat.samples}
              {#if stat.provisional}<span class="yard-stat-warn">small n</span>{/if}
            </div>
          </div>
        {/each}
      </div>
    {/if}

    <!-- The approach (f930cda2): the car lifecycle upstream of the
         dock, which used to render as silence — 8 publish-requests,
         8 gates and an arrival in one hour once drew an empty yard.
         The full pre-boarding lifecycle: queued → gating (the slots) →
         green becomes a parked car, RED drops into the garage. Ordered
         by distance from the dock: publishing, gating, red,
         green-unparked; each row opens its own packet. The gate SLOTS
         and the GARAGE come from the server-computed status (David,
         2026-09-03) so capacity is the live policy, not folklore. -->
    {#if yard.approach.length > 0 || slots.length > 0 || garage.length > 0}
      <div class="yard-section">01 — THE APPROACH <span class="yard-n">{yard.approach.length}</span></div>

      {#if yard.approach.length > 0}
        <table class="yard-board">
          <tbody>
            {#each yard.approach as a (a.id)}
              <tr class="yard-approach" ondblclick={() => openPacket(a.id)}>
                <td class="yard-appr-state" data-state={a.state}>{a.state.replace('-', ' ')}</td>
                <td>
                  <a
                    href={`/jobs/${a.id}`}
                    title="open the packet behind this row"
                    onclick={e => {
                      e.preventDefault();
                      openPacket(a.id);
                    }}>{a.branch}</a>
                </td>
                <td class="yard-stamp">{a.sha ? a.sha.slice(0, 8) : '—'}</td>
                <td class="yard-stamp">{a.note ?? a.opened_on}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}

      <!-- The gate SLOTS: N parallel bays (N = the policy's
           gate_max_concurrent), each empty or holding the car being
           assessed in it. Capacity + usage legible at a glance. -->
      {#if slots.length > 0}
        <div class="yard-gates-head">
          GATES
          <span class="yard-gates-n"
            >{slots.filter(s => s.kind === 'occupied').length} / {slots.length} in use</span>
        </div>
        <div class="yard-gates">
          {#each slots as slot, i (i)}
            {#if slot.kind === 'occupied'}
              <button
                type="button"
                class="yard-slot is-busy"
                title="open the gate-run packet"
                onclick={() => openPacket(slot.gate.packet_id)}>
                <span class="yard-slot-n">gate {i + 1}</span>
                <span class="yard-slot-branch">{slot.gate.branch}</span>
                <span class="yard-slot-since">gating since {slot.gate.since}</span>
              </button>
            {:else}
              <div class="yard-slot is-free">
                <span class="yard-slot-n">gate {i + 1}</span>
                <span class="yard-slot-free">available</span>
              </div>
            {/if}
          {/each}
        </div>
      {/if}

      <!-- The GARAGE: cars whose latest gate went RED, waiting for
           rework. A branch that re-gated green has left already (the
           server keeps only the latest run per branch). -->
      {#if garage.length > 0}
        <div class="yard-gates-head">GARAGE <span class="yard-gates-n">gated red, awaiting rework</span></div>
        <ul class="yard-garage">
          {#each garage as c (c.branch)}
            <li class="yard-garage-row">
              <span class="yard-garage-branch">{c.branch}</span>
              <span class="yard-garage-check">{c.failed_check ?? 'run died outside a check'}</span>
              <span class="yard-stamp">{c.since}</span>
            </li>
          {/each}
        </ul>
      {/if}
    {/if}

    <!-- The dock is a station rendered (stations.md): when the
         registry served, the header carries the station's own facts —
         the ordering discipline (Q2: never wonder why the queue is in
         this order) and the advisory WIP verdict (Q3: warn, don't
         enforce). The derived fallback has no facts to show.

         The walk upstream sits INSIDE this div, anchored left (David,
         feedback 3ccb79f5) — a queue that isn't filling is diagnosed
         upstream, and the affordance has to be where the operator is
         already looking when they notice. It is navigation, not
         content: the dock gains no row, no count, no state. -->
    <div class="yard-section">
      {#if upstream}
        <button
          type="button"
          class="yard-upstream"
          title={upstream.title}
          onclick={() => navigate(upstream.href)}>{upstream.label}</button>
      {/if}
      02 — LOADING DOCK <span class="yard-n">{yard.dock.length}</span>
      {#if yard.dockStation.source === 'station'}
        <span class="yard-discipline" title="queue discipline"
          >{disciplineLabel(yard.dockStation.discipline)}</span>
        {#if wipAdvisory(yard.dockStation)}
          <span class="yard-wip" title="over the station's advisory WIP limit"
            >{wipAdvisory(yard.dockStation)}</span>
        {/if}
      {/if}
    </div>
    {#if yard.dock.length === 0}
      <div class="yard-empty">The dock is clear.</div>
    {:else}
      <div class="yard-dock">
        {#each yard.dock as c (c.id)}
          <PacketCard card={c} size="dock" onOpen={openPacket} />
        {/each}
      </div>
    {/if}

    <!-- The departure line is the MERGE (0bba59f7). Pre-merge is yard
         work — assembling, inspecting (CI), under repair — where a red
         lamp is status, not alarm; red is deliberately NOT softened,
         it just lives here. Post-merge is transit, green by
         construction, and short. -->
    <div class="yard-section">
      03 — IN THE YARD <span class="yard-n">{split.inYard.length}</span>
      <span class="yard-hint">assembling · inspecting · under repair — red is work, not a wreck</span>
    </div>
    {#if split.inYard.length === 0}
      <div class="yard-empty">Yard clear — nothing assembling.</div>
    {:else}
      {#each split.inYard as t (t.id)}{@render trainBlock(t)}{/each}
    {/if}

    <div class="yard-section">
      04 — DEPARTED · IN TRANSIT <span class="yard-n">{split.inTransit.length}</span>
      <span class="yard-hint">past the merge — irreversible, green by construction</span>
    </div>
    {#if split.inTransit.length === 0}
      <div class="yard-empty">Nothing in transit.</div>
    {:else}
      {#each split.inTransit as t (t.id)}{@render trainBlock(t)}{/each}
    {/if}

    <!-- Arrivals are trains that ARRIVED — a cancelled train never
         did, and it keeps its own muted line below rather than
         disappearing. Ordered by the best arrival instant each train
         carries (the column's tooltip names the evidence), because
         `opened_on` is day-granular and tied every train opened on the
         same day. Each row opens the train's Job, where the landing
         report is. -->
    <div class="yard-section">05 — RECENT ARRIVALS</div>
    <table class="yard-board">
      <thead><tr><th>Train</th><th>Consist</th><th>Arrival</th></tr></thead>
      <tbody>
        {#each yard.arrivals as t (t.id)}
          <tr class="yard-arrival" ondblclick={() => openTrain(t)}>
            <td>
              <a
                href={trainHref(t)}
                title="{t.title} — open the train's landing report"
                onclick={e => {
                  e.preventDefault();
                  openTrain(t);
                }}>{t.title}</a>
            </td>
            <td>{t.cars.length} cars</td>
            <td class="yard-stamp" title={arrivalTitle(t)}>{arrivalText(t)}</td>
          </tr>
        {/each}
        {#if yard.arrivals.length === 0}
          <tr><td colspan="3" class="yard-empty">No train has arrived yet.</td></tr>
        {/if}
      </tbody>
    </table>

    {#if yard.cancelled.length > 0}
      <div class="yard-cancelled">
        {#each yard.cancelled as t (t.id)}
          <div>
            <a
              href={trainHref(t)}
              onclick={e => {
                e.preventDefault();
                openTrain(t);
              }}>{t.title}</a>
            — {t.outcome === 'cancelled' ? 'cancelled, nothing to board' : 'closed, never arrived'}
          </div>
        {/each}
      </div>
    {/if}

    {#if yard.awaitingProof.length > 0}
      <div class="yard-section">
        06 — AWAITING PROOF <span class="yard-n">{yard.awaitingProof.length}</span>
      </div>
      <!--
        Merged, deployed, and unverified. These belong to none of the
        yard's other partitions — open trains, arrivals, the dock — so
        seven of them sat invisible on 2026-08-28 while being the agreed
        bottleneck.
      -->
      <div class="yard-awaiting">
        {#each yard.awaitingProof as c (c.id)}
          <a class="yard-awaiting-car" href="/ux/jobs/{c.id}">{c.title}</a>
        {/each}
      </div>
    {/if}

    {#snippet trainBlock(t: TrainRow)}
      <div class="yard-trainblock">
        <div class="yard-trainhead">
          {#if t.live}<span class="yard-dot" title="in motion"></span>{/if}
          <span class="yard-trainname">{t.title}</span>
          <span class="yard-lamp" class:ok={t.lamp === 'green'} class:err={t.lamp === 'failing'} class:run={t.lamp === 'pending'}>
            {t.lamp === 'green' ? 'CI ✓' : t.lamp === 'failing' ? 'CI ✗' : 'CI …'}
          </span>
          <span class="yard-chip">{t.status}</span>
          {#if t.trouble}
            <span class="yard-trouble" title="an alarm was already raised for this train">
              {troubleLabel(t.trouble)}
            </span>
          {/if}
          {#if t.eta.phase !== 'arrived'}
            <span class="yard-eta" class:est={t.eta.kind === 'eta'} title={etaTitle(t.eta)}>
              {etaText(t.eta)}
            </span>
          {/if}
          <span class="yard-stamp">{stampOf(t)}</span>
        </div>
        <div class="yard-consist">
          {#if t.cars.length === 0}
            <span class="yard-empty">consist forming…</span>
          {:else}
            {#each t.cars as c (c.id)}
              <PacketCard card={c} size="consist" onOpen={openPacket} />
            {/each}
          {/if}
        </div>
      </div>
    {/snippet}

    <div class="yard-flow">GATED → PARKED → BOARDED → <em>DEPARTED</em> → ARRIVED → PROVEN</div>
  {/if}
</div>

<!-- Outside .yard-root so the backdrop covers the page rather than
     sitting inside the padded column. -->
{#if packetId}
  <PacketModal
    job={packet}
    loading={packetLoading}
    error={packetError}
    onClose={closePacket}
  />
{/if}

<style>
  .yard-root { padding: 0 32px 32px; }
  .yard-hint {
    font-size: 11px; font-weight: 400; letter-spacing: 0;
    text-transform: none; color: var(--text-dim, #78716c); margin-left: 10px;
  }
  .yard-section {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px; letter-spacing: var(--ls-eyebrow, 0.3em);
    color: var(--signal, #5FD4A8); margin: 28px 0 8px;
    display: flex; align-items: center; gap: 12px;
  }
  .yard-section::after { content: ''; flex: 1; border-top: 1px solid var(--hairline, #2A3138); }
  .yard-n { color: var(--static, #7A838C); }
  /* Station facts in the section header: discipline stays quiet
     (static grey, same mono caps), the WIP advisory wears --warn —
     the one state color in the header, present only when the queue
     exceeds its declared bandwidth. */
  .yard-discipline { color: var(--static, #7A838C); letter-spacing: var(--ls-nav, 0.14em); }
  .yard-wip { color: var(--warn, #d9a441); border: 1px solid var(--warn, #d9a441);
    padding: 1px 7px; letter-spacing: 0.1em; }
  /* The walk upstream: the chip grammar exactly (mono caps, hairline,
     radius 0, --static), because it is an instrument on the header
     rather than a call to action. It brightens to --signal on hover
     and focus — the same "this is live" green the arrivals rows use —
     so it reads as inert until you reach for it. */
  .yard-upstream {
    font: inherit;
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase;
    color: var(--static, #7A838C);
    background: transparent;
    border: 1px solid var(--hairline, #2A3138);
    border-radius: 0;
    padding: 2px 8px;
    cursor: pointer;
    white-space: nowrap;
    transition: color 120ms ease, border-color 120ms ease;
  }
  .yard-upstream:hover, .yard-upstream:focus-visible {
    color: var(--signal, #5FD4A8); border-color: var(--signal, #5FD4A8);
  }
  @media (prefers-reduced-motion: reduce) { .yard-upstream { transition: none; } }
  .yard-board { width: 100%; border-collapse: collapse; background: var(--card, var(--ink, #12161C));
    border: 1px solid var(--hairline, #2A3138); font-size: 14px; }
  .yard-board th { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: var(--ls-nav, 0.14em); text-transform: uppercase; font-weight: 400;
    color: var(--static, #7A838C); text-align: left; padding: 8px 12px;
    border-bottom: 1px solid var(--hairline, #2A3138); }
  .yard-board td { padding: 7px 12px; border-bottom: 1px solid var(--hairline, #2A3138); }
  .yard-board tr:last-child td { border-bottom: none; }
  .yard-trainblock { border: 1px solid var(--hairline, #2A3138);
    background: var(--card, var(--ink, #12161C)); margin-bottom: 12px; }
  .yard-trainhead { display: flex; align-items: center; gap: 12px; padding: 9px 12px;
    border-bottom: 1px solid var(--hairline, #2A3138); font-size: 14px; }
  .yard-trainname { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis;
    white-space: nowrap; }
  /* The flatbed: consist cards sit on VOID so the packets read as
     cargo loaded onto the train, the same cards that wait in the dock. */
  .yard-consist { display: flex; flex-wrap: wrap; gap: 8px; padding: 10px 12px;
    background: var(--bg, var(--void, #0D1014)); }
  .yard-dock { display: grid; gap: 10px;
    grid-template-columns: repeat(auto-fill, minmax(250px, 1fr)); }
  /* Trouble reads LOUDER than the phase chip beside it: the whole
     defect this fixes was a wedged train looking like a moving one. */
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
  .yard-chip { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: 0.1em; border: 1px solid var(--hairline, #2A3138); padding: 2px 8px; }
  .yard-lamp { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: 0.1em; border: 1px solid var(--hairline, #2A3138); padding: 2px 8px; }
  .yard-lamp.ok  { color: var(--ok, #4fb98a); border-color: var(--ok, #4fb98a); }
  .yard-lamp.err { color: var(--err, #e2685c); border-color: var(--err, #e2685c); }
  .yard-lamp.run { color: var(--warn, #d9a441); border-color: var(--warn, #d9a441); }
  /* Approach states borrow the lamp palette: a red gate IS an error
     lamp, a live gate a running one; green-unparked and publishing
     stay muted — inbound, not yet the dock's business. */
  .yard-appr-state { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: 0.1em; white-space: nowrap; }
  .yard-appr-state[data-state='gating'] { color: var(--warn, #d9a441); }
  .yard-appr-state[data-state='gated-red'] { color: var(--err, #e2685c); }
  .yard-appr-state[data-state='gated-green'] { color: var(--ok, #4fb98a); }
  .yard-appr-state[data-state='publishing'] { color: var(--static, #7A838C); }
  .yard-dot { display: inline-block; width: 7px; height: 7px; border-radius: 50%;
    background: var(--signal, #5FD4A8); margin-right: 8px;
    animation: yard-pulse 1.4s ease-in-out infinite; }
  @keyframes yard-pulse { 50% { opacity: 0.35; } }
  @media (prefers-reduced-motion: reduce) { .yard-dot { animation: none; } }
  .yard-stamp { font-family: var(--font-mono, ui-monospace, monospace); font-size: 12px;
    color: var(--static, #7A838C); font-variant-numeric: tabular-nums; }
  /* The ETA chip. An estimate reads brighter than the phase-only
     state, and never brighter than the live dot — it is a median of
     what recent trains did, not a promise about this one. */
  .yard-eta { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: 0.1em; color: var(--static, #7A838C); padding: 2px 8px;
    border: 1px solid var(--hairline, #2A3138); font-variant-numeric: tabular-nums;
    white-space: nowrap; }
  .yard-eta.est { color: var(--text, #C7CED6); }
  /* The arrivals row is a link to the train's landing report. */
  .yard-arrival { cursor: pointer; }
  .yard-arrival:hover { background: var(--bg, var(--void, #0D1014)); }
  .yard-board a { color: inherit; text-decoration: none; }
  .yard-board a:hover, .yard-board a:focus-visible { color: var(--signal, #5FD4A8); }
  /* Cancelled trains: kept in the world, kept out of the arrivals
     board. One muted line each. */
  .yard-cancelled { margin-top: 10px; font-size: 12.5px; color: var(--static, #7A838C);
    display: flex; flex-direction: column; gap: 4px; }
  .yard-cancelled a { color: inherit; text-decoration: none; }
  .yard-cancelled a:hover, .yard-cancelled a:focus-visible { color: var(--signal, #5FD4A8); }
  .yard-scoreboard {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    margin-bottom: 14px;
  }
  .yard-stat {
    flex: 1 1 140px;
    padding: 10px 12px;
    border: 1px solid var(--line, #2a2f3a);
    border-radius: 6px;
  }
  .yard-stat.is-provisional { opacity: 0.75; }
  .yard-stat-v { font-size: 24px; font-weight: 600; line-height: 1.1; }
  .yard-stat-l { font-size: 12px; text-transform: uppercase; letter-spacing: 0.04em; opacity: 0.7; }
  .yard-stat-p { font-size: 11px; opacity: 0.6; margin-top: 4px; }
  .yard-stat-warn { margin-left: 6px; opacity: 0.9; }
  .yard-awaiting { display: flex; flex-wrap: wrap; gap: 8px; margin-bottom: 14px; }
  .yard-awaiting-car {
    font-size: 12px;
    padding: 4px 8px;
    border: 1px solid var(--line, #2a2f3a);
    border-radius: 4px;
    text-decoration: none;
  }
  .yard-empty { color: var(--static, #78716c); padding: 12px 0; font-size: 14px; }
  /* Gates + garage sub-headers inside the Approach section: quieter than
     a numbered section header, the same mono-caps grammar. */
  .yard-gates-head {
    font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: var(--ls-nav, 0.14em); text-transform: uppercase;
    color: var(--static, #7A838C); margin: 14px 0 8px;
    display: flex; align-items: center; gap: 10px;
  }
  .yard-gates-n { color: var(--static, #7A838C); text-transform: none; letter-spacing: 0; }
  /* The slots: N parallel bays. A busy bay wears the running-lamp warn,
     a free one the muted hairline — the same palette the approach states
     and the CI lamps use, no new colour. */
  .yard-gates { display: grid; gap: 10px;
    grid-template-columns: repeat(auto-fill, minmax(180px, 1fr)); }
  .yard-slot {
    display: flex; flex-direction: column; gap: 3px; text-align: left;
    border: 1px solid var(--hairline, #2A3138);
    background: var(--card, var(--ink, #12161C));
    padding: 10px 12px; min-height: 64px;
    font: inherit; border-radius: 0;
  }
  .yard-slot.is-busy { border-color: var(--warn, #d9a441); cursor: pointer; }
  .yard-slot.is-busy:hover, .yard-slot.is-busy:focus-visible {
    border-color: var(--signal, #5FD4A8); }
  .yard-slot.is-free { border-style: dashed; justify-content: center; }
  .yard-slot-n { font-family: var(--font-mono, ui-monospace, monospace); font-size: 10px;
    letter-spacing: 0.14em; text-transform: uppercase; color: var(--static, #7A838C); }
  .yard-slot-branch { font-size: 13px; overflow: hidden; text-overflow: ellipsis;
    white-space: nowrap; color: var(--text, #C7CED6); }
  .yard-slot-since { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    color: var(--warn, #d9a441); font-variant-numeric: tabular-nums; }
  .yard-slot-free { font-size: 12px; color: var(--static, #7A838C); font-style: italic; }
  /* The garage: gated-red cars, one row each. The err lamp on the
     branch, the failing check beside it. */
  .yard-garage { list-style: none; padding: 0; margin: 0;
    border: 1px solid var(--hairline, #2A3138); background: var(--card, var(--ink, #12161C)); }
  .yard-garage-row { display: flex; align-items: center; gap: 12px; padding: 8px 12px;
    border-bottom: 1px solid var(--hairline, #2A3138); font-size: 13px; }
  .yard-garage-row:last-child { border-bottom: none; }
  .yard-garage-branch { color: var(--err, #e2685c); font-weight: 600;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; flex: 1; }
  .yard-garage-check { font-family: var(--font-mono, ui-monospace, monospace); font-size: 12px;
    color: var(--static, #7A838C); }
  .yard-flow { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: var(--ls-nav, 0.14em); color: var(--static, #7A838C);
    border-top: 1px solid var(--hairline, #2A3138); margin-top: 28px; padding-top: 12px; }
  .yard-flow em { color: var(--signal, #5FD4A8); font-style: normal; }
</style>
