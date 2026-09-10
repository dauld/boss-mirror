<script lang="ts">
  // The train yard — the IT department's front door (departure-
  // board.md Q1, David's call: guest-visible, the IT app's landing).
  // A queue lens in the departure-board idiom, drawn as a rail yard
  // (the-yard-is-a-floor-you-can-follow, approved 2026-09-08): every
  // change is a wagon you can follow from the approach siding to the
  // arrivals yard; every machine on the line says what it is doing and
  // why it is not; the departure board under the map lists where each
  // wagon is; clicking anything selects it and the entity panel shows
  // its facts and the verbs that apply. Every row is a Job the
  // conductor writes; nothing here is new state. Reads are
  // audit-readonly-safe by construction.
  //
  // ONE write (backlog 7a24caf3): the cancel button stamps
  // `cancel_requested` on a troubled, not-yet-merged train through the
  // metadata merge, and the conductor honours it on its next reconcile.
  // The page offers the button only to a platform-admin session
  // (`CANCEL_ROLE`) — affordance, not the gate. The gate is the API's
  // `job:update`, which an audit-readonly guest does not hold: a guest
  // who reaches the endpoint by hand is refused there, and the page
  // shows that refusal in the server's own words.
  //
  // Two reads come from OUTSIDE the yard's read models (Car 2a): the
  // converge ops-request packets, which drive the deploy-runner shed,
  // and this browser's own read of /api/jobs/health, which drives the
  // cluster tower — the system of record observed from outside, the
  // reading the 2026-09-05 outage wanted a page to show. Both are
  // "no reading" until the first poll, never idle or healthy by default.
  import { onMount } from 'svelte';
  import {
    CANCEL_ROLE,
    canOfferCancel,
    cancelRequestBody,
    disciplineLabel,
    dockUpstream,
    fetchYard,
    splitAtDeparture,
    troubleLabel,
    wipAdvisory,
    type CancelRequest,
    type Eta,
    type EtaPhase,
    type JobLite,
    type TrainRow,
    type YardPartition,
    type YardState,
  } from './yard';
  import { session } from '@boss/web-kit/session/session.svelte';
  import {
    blockLabel,
    boardHold,
    boardsWhen,
    conductorReading,
    elapsedText,
    etaDetail,
    etaReading,
    fetchYardStatus,
    journeyText,
    lastVerbReading,
    phaseLabel,
    queueLabel,
    type TrainStatus,
    type YardStatus,
  } from './yard-status';
  import {
    journeyStops,
    parseSelection,
    scene as sceneOf,
    sinceText,
    STAGES,
    type Feeds,
    type Scene,
  } from './yard-floor';
  import {
    CONVERGE_USUAL_MINUTES,
    clusterLabel,
    clusterReading,
    fetchHealth,
    fetchOpsRequests,
    runnerLabel,
    runnerMachine,
    runnerPacketId,
    type ClusterMachine,
  } from './yard-machines';
  import { yardAlerts, type Alert } from './yard-alerts';
  import { yardSignals } from './yard-signals';
  import { production as productionOf } from './yard-production';
  import YardMap from './YardMap.svelte';
  import DepartureBoard from './DepartureBoard.svelte';
  import ArrivalReport from './ArrivalReport.svelte';
  import ProductionPanel from './ProductionPanel.svelte';
  import SignalsPanel from './SignalsPanel.svelte';
  import type { Remote } from '../../data/remote';
  import PacketCard from '@boss/web-kit/ui/PacketCard.svelte';
  import PacketModal, { type PacketJob } from '@boss/web-kit/ui/PacketModal.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import { navigate } from '@boss/web-kit/nav';

  let yard = $state<YardState | null>(null);
  let loading = $state(true);
  // The clock the "since" columns and the elapsed readings run on,
  // advanced on every poll so a wagon's age keeps counting.
  let nowMs = $state(Date.now());

  // The server-computed read-model (gate capacity from the delivery
  // policy, the garage, the conductor's own liveness, the boarding
  // hold). Held as a Remote so an outage renders honestly — the map's
  // bays read "no reading" — rather than a false-empty approach.
  let status = $state<Remote<YardStatus>>({ kind: 'loading' });
  const statusData = $derived(status.kind === 'ready' ? status.data : null);
  const garage = $derived(statusData?.garage ?? []);
  // Operating info for the gates (David, feedback 3771438f): the
  // capacity is the live delivery policy, the usage is now, and the
  // approach's publishing rows are the branches queued to reach a gate.
  const gateCapacity = $derived(statusData?.gates.capacity ?? 0);
  const gatesInUse = $derived(statusData?.gates.active.length ?? 0);
  const gatesFree = $derived(Math.max(gateCapacity - gatesInUse, 0));
  const waitingToGate = $derived(yard ? yard.approach.filter(a => a.state === 'publishing').length : 0);
  // The QUEUE: runs that asked for a bay and were given a place in line
  // (db7f7b73). Distinct from `waitingToGate`, which counts branches still
  // publishing — these have already asked to gate and are waiting on
  // bandwidth, which is what makes three busy bays read as a queue rather
  // than as absence.
  const gateQueue = $derived(statusData?.gates.queued ?? []);
  const queueTypical = $derived(statusData?.gates.typical_seconds ?? null);

  // THE TWO OUTSIDE READS. The converge ops-requests (null before the
  // first read, or when the read failed — the shed then says "no
  // reading") and the tower's reading, which keeps its `since` across
  // polls while its state holds (yard-machines.ts).
  let opsRequests = $state<readonly JobLite[] | null>(null);
  let cluster = $state<ClusterMachine>({ kind: 'unknown' });
  const feeds = $derived<Feeds>({ runner: runnerMachine(opsRequests, nowMs), cluster });

  // THE FLOOR: the pure scene both the map and the board draw.
  const floor = $derived<Scene | null>(yard ? sceneOf(yard, statusData, nowMs, feeds) : null);
  const wagonById = $derived(new Map((floor?.wagons ?? []).map(w => [w.id, w])));
  const whereById = $derived(new Map((floor?.boardRows ?? []).map(r => [r.id, r.where])));
  const locoById = $derived(new Map((floor?.locos ?? []).map(l => [l.id, l])));

  // The CONDUCTOR block: the actor that boards the dock, read from its
  // own firing record (the-board-does-not-lie) — never inferred from
  // the dock looking full or the trains looking healthy. The boards
  // line is the server's HOLD — why the dock is not boarding right now
  // and what clears it — with the live cadence rule beneath it; never
  // a next-board time (the rule is depth-triggered; it has no clock).
  // `moving` is the server's own phase / step / block per train in
  // transit, so a wedged train shows WHERE and WHY beside the actor
  // that should be moving it.
  const conductor = $derived(statusData?.conductor ?? null);
  const liveness = $derived(conductorReading(conductor));
  const lastVerb = $derived(lastVerbReading(conductor));
  const hold = $derived(status.kind === 'ready' ? boardHold(status.data.boarding) : null);
  const boardingRule = $derived(status.kind === 'ready' ? boardsWhen(status.data.boarding) : '');
  const moving = $derived(statusData ? statusData.trains.filter(t => t.phase !== 'arrived') : []);
  const serverTrainById = $derived(new Map((statusData?.trains ?? []).map(t => [t.id, t])));

  // Elapsed at the train's current position, from whichever stamp the
  // record carries: a block's `since`, else the converge start the
  // board already tracks for the same train. No stamp, no number.
  function movingFor(t: TrainStatus): string | null {
    const b = t.block;
    const since = b && (b.kind === 'deploy-blocked' || b.kind === 'stalled') ? b.since : null;
    if (since) return elapsedText(since, nowMs);
    const row = yard?.inFlight.find(r => r.id === t.id) ?? null;
    return row ? convergingFor(row) : null;
  }

  // THE ALERTS STRIP: what is wrong on the floor right now, derived
  // from the scene (yard-alerts.ts) — each a button to its subject.
  const alerts = $derived<readonly Alert[]>(floor ? yardAlerts(floor, statusData, nowMs) : []);

  // THE LOWER DECK: what the floor produced today, and what fired what,
  // both read off the packets the page already holds plus the
  // ops-requests it now fetches.
  const signalRows = $derived(yard ? yardSignals(yard.packets.trains, yard.packets.gateRuns, opsRequests ?? []) : []);

  // The last merged train — an in-flight train past the merge (the
  // trains are served newest first), else the newest arrival — so the
  // tower can say whether the cluster is on its merge yet. A merge_ref
  // is a prefix of the full sha the health endpoint reports.
  const lastMerged = $derived.by((): Readonly<{ ref: string; title: string }> | null => {
    if (!yard) return null;
    const t = yard.inFlight.find(x => x.mergeRef) ?? yard.arrivals.find(x => x.mergeRef) ?? null;
    return t && t.mergeRef ? { ref: t.mergeRef, title: t.title } : null;
  });
  const shasMatch = (a: string, b: string): boolean => a.startsWith(b) || b.startsWith(a);

  // THE SELECTION — one string the map, the board and the alerts all
  // speak. The track by default: the thing most often worth watching.
  let selected = $state<string>('track');
  const sel = $derived(parseSelection(selected));

  // The selected packet's own steps (a car's journey, a train's steps
  // and landing report), fetched on selection and refreshed with each
  // poll. Holding the id rather than the row means the 10s poll cannot
  // swap the panel's contents underneath a read.
  type EntityStep = Readonly<{
    id: string;
    spec_slug?: string | null;
    title: string;
    status: string;
    metadata?: Record<string, unknown> | null;
    completed_at?: string | null;
    completed_on?: string | null;
    assignee_id?: string | null;
  }>;
  type EntityJob = Readonly<{
    id: string;
    kind: string;
    title: string;
    status: string;
    opened_on?: string;
    closed_on?: string | null;
    tags?: readonly string[];
    owner_id?: string | null;
    subject?: { subject_kind?: string; id?: string } | null;
    metadata?: Record<string, unknown> | null;
    steps?: readonly EntityStep[];
  }>;
  let entityJob = $state<EntityJob | null>(null);
  let entityJobFor = $state<string | null>(null);
  let entityJobError = $state<string | null>(null);
  const entityStops = $derived(journeyStops(entityJob));

  /** A wagon whose id is a packet — not a server-garaged branch the
   *  window holds no packet for. */
  const isPacket = (id: string): boolean => !id.startsWith('garage:');

  async function loadEntityJob(id: string): Promise<void> {
    entityJobFor = id;
    entityJobError = null;
    try {
      const r = await fetch(`/api/jobs/${encodeURIComponent(id)}`);
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const body = (await r.json()) as EntityJob;
      if (entityJobFor === id) entityJob = body;
    } catch (e) {
      if (entityJobFor === id) {
        entityJob = null;
        entityJobError = `Could not read the packet — ${e instanceof Error ? e.message : String(e)}`;
      }
    }
  }

  function select(key: string): void {
    selected = key;
    const s = parseSelection(key);
    const id =
      s.kind === 'car' || s.kind === 'train' ? s.id : s.kind === 'runner' ? runnerPacketId(feeds.runner) : null;
    if (id !== null && isPacket(id)) {
      if (entityJobFor !== id) entityJob = null;
      void loadEntityJob(id);
    } else {
      entityJob = null;
      entityJobFor = null;
      entityJobError = null;
    }
  }

  // The condensed packet panel (David, fc67bed2) — the "open packet"
  // verb, and what a double-click on any packet card does here.
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
  // Which side of the line a single selected train is on — the same
  // split, asked for one train, so the cancel rule sees the partition.
  const partitionOf = (t: TrainRow): YardPartition =>
    split.inYard.some(x => x.id === t.id) ? 'in-yard' : 'in-transit';

  // The viewer, for the one write on this page. The session is the
  // app's single identity read (web-kit's `/api/session` probe,
  // resolved to an Employee row); its `id` is the actor the stamp
  // names, its `role` the affordance gate.
  const viewer = $derived(session.value.kind === 'ready' ? session.value.user : null);
  const viewerPrivileged = $derived(
    session.value.kind === 'ready' && session.value.user.role === CANCEL_ROLE,
  );

  // The cancel dialog: one train at a time, a reason typed, the write
  // in flight, and the server's own words when it refuses. A sent
  // stamp is kept by train id so the chip shows at once; the 10s poll
  // then reads the same stamp back off the Job and the local copy is
  // moot.
  let cancelFor = $state<string | null>(null);
  let cancelReason = $state('');
  let cancelBusy = $state(false);
  let cancelError = $state<string | null>(null);
  let cancelSent = $state<Readonly<Record<string, CancelRequest>>>({});

  function openCancel(t: TrainRow): void {
    cancelFor = t.id;
    cancelReason = '';
    cancelError = null;
  }

  function closeCancel(): void {
    cancelFor = null;
    cancelReason = '';
    cancelError = null;
  }

  async function requestCancel(t: TrainRow): Promise<void> {
    if (!viewer) return;
    const body = cancelRequestBody(viewer.id, cancelReason, new Date().toISOString());
    if (!body) {
      cancelError = 'A reason is required.';
      return;
    }
    cancelBusy = true;
    cancelError = null;
    try {
      const r = await fetch(`/api/jobs/${encodeURIComponent(t.id)}/metadata`, {
        method: 'PATCH',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!r.ok) {
        // The server's words, never swallowed — a 403 here is the real
        // gate speaking.
        const text = (await r.text()).trim();
        cancelError = `HTTP ${r.status}${text ? ` — ${text}` : ''}`;
        return;
      }
      cancelSent = { ...cancelSent, [t.id]: body.cancel_requested };
      closeCancel();
    } catch (e) {
      cancelError = e instanceof Error ? e.message : String(e);
    } finally {
      cancelBusy = false;
    }
  }

  onMount(() => {
    let cancelled = false;
    async function tick() {
      const [y, s, ops, health] = await Promise.all([
        fetchYard(),
        fetchYardStatus(),
        fetchOpsRequests(),
        fetchHealth(),
      ]);
      if (cancelled) return;
      if (y) yard = y;
      status = s;
      opsRequests = ops;
      const now = Date.now();
      cluster = clusterReading(cluster, health, now);
      nowMs = now;
      loading = false;
      // The selected packet's steps move too; the runner's packet may
      // be a newer one than the panel was opened on.
      const runnerId = sel.kind === 'runner' ? runnerPacketId(feeds.runner) : null;
      if (runnerId !== null && runnerId !== entityJobFor) void loadEntityJob(runnerId);
      else if (entityJobFor !== null) void loadEntityJob(entityJobFor);
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
  /** An RFC3339 instant as a clock time, or the raw text when it is
   *  not one — never a dash for a stamp that exists. */
  const clockOf = (iso: string): string => {
    const ms = Date.parse(iso);
    return Number.isNaN(ms) ? iso : clock(ms);
  };

  // What the train is waiting on, when there is no honest time to give.
  const PHASE_LABEL: Record<EtaPhase, string> = {
    boarding: 'boarding',
    ci: 'CI running',
    merging: 'awaiting merge',
    deploying: 'deploying',
    converging: 'awaiting cluster convergence',
    blocked: 'CI red',
    arrived: 'arrived',
  };

  // The converge wait, as elapsed time — the honest measure when there
  // is no median to project from (mirrors the yard-status page's idiom).
  // Recomputed each 10s tick, when `nowMs` advances.
  function convergingFor(t: TrainRow): string | null {
    if (!t.convergingSince) return null;
    const started = Date.parse(t.convergingSince);
    if (Number.isNaN(started)) return null;
    return journeyText((nowMs - started) / 1000);
  }

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

  /** Any train the yard knows by id — in flight, arrived or cancelled. */
  function trainById(id: string): TrainRow | null {
    if (!yard) return null;
    return (
      yard.inFlight.find(t => t.id === id) ??
      yard.arrivals.find(t => t.id === id) ??
      yard.cancelled.find(t => t.id === id) ??
      null
    );
  }
</script>

<div class="theme-exec yard-root">
  <PageHeader
    eyebrow="IT · Forge line"
    title="The train yard"
    subtitle="Gated → parked → boarded → departed → arrived → proven — every change is a wagon you can follow from the approach siding to the arrivals yard"
  />

  {#if loading}
    <div class="yard-empty">Reading the yard…</div>
  {:else if !yard || !floor}
    <div class="yard-empty">The yard is unreachable right now.</div>
  {:else}
    {@const prod = productionOf(yard, statusData, nowMs)}
    <!-- THE ALERTS STRIP: what is wrong right now, each a button to
         its subject. Quiet when every machine is working or idle by
         design. -->
    <div class="yard-alerts" aria-live="polite">
      {#each alerts as a (a.id)}
        <button type="button" class="yard-alert {a.sev}" onclick={() => select(a.subject)}>
          <span class="yard-lamp-dot {a.sev}"></span>
          <span>{a.text}</span>
          {#if a.since}<time class="yard-mono">since {clockOf(a.since)}</time>{/if}
        </button>
      {/each}
      {#if alerts.length === 0}
        <span class="yard-quiet"><span class="yard-lamp-dot ok"></span>No alerts — every machine is working or idle by design.</span>
      {/if}
    </div>

    <!-- THE MAP -->
    <YardMap scene={floor} {selected} onselect={select} />

    <!-- THE DECK: the departure board and the entity panel -->
    <div class="yard-deck">
      <div class="yard-panel">
        <DepartureBoard scene={floor} {selected} onselect={select} {nowMs} />
      </div>
      <div class="yard-panel yard-entity" aria-live="polite">
        {#if sel.kind === 'car'}
          {@const w = wagonById.get(sel.id) ?? null}
          <h2 class="yard-panel-h">Entity · car</h2>
          {#if !w}
            <div class="yard-empty">This car is no longer on the floor — it landed and left the window, or the yard changed underneath it.</div>
          {:else}
            <div class="yard-entity-title">{w.title}</div>
            <div class="yard-entity-sub yard-mono">{w.branch}@{w.head ?? '—'} · packet {w.id.slice(0, 8)}</div>
            <dl class="yard-kv">
              <dt>now</dt>
              <dd><span class="yard-lamp-dot {w.lamp}"></span>{w.status} · since {sinceText(w.since, nowMs)}</dd>
              <dt>where</dt>
              <dd>{whereById.get(w.id) ?? '—'}</dd>
              {#if w.trainId}
                {@const tr = trainById(w.trainId)}
                <dt>train</dt>
                <dd>
                  <button type="button" class="yard-link" onclick={() => select(`train:${w.trainId}`)}
                    >{tr?.title ?? w.trainId}</button>
                </dd>
              {/if}
            </dl>
            <div class="yard-label">Its journey</div>
            <div class="yard-steps">
              {#each entityStops as s, i (i)}
                <div class="yard-step">
                  <span class="yard-lamp-dot {s.lamp}"></span>
                  <span>{s.what}</span>
                  <span class="yard-when yard-mono">{s.when ? clockOf(s.when) : ''}{s.note ? ` · ${s.note}` : ''}</span>
                </div>
              {/each}
              {#if entityJobError}
                <span class="yard-empty">{entityJobError}</span>
              {:else if !isPacket(w.id)}
                <span class="yard-empty">no packet in the window names this branch — the server garages it by its latest gate</span>
              {:else if entityJob === null}
                <span class="yard-empty">reading the packet…</span>
              {:else if entityStops.length === 0}
                <span class="yard-empty">no steps on record</span>
              {/if}
            </div>
            {#if isPacket(w.id)}
              <div class="yard-verbs">
                <button type="button" onclick={() => openPacket(w.id)}>open packet</button>
                <button type="button" onclick={() => navigate(entityHref('job', w.id))}>open job page</button>
              </div>
            {/if}
          {/if}
        {:else if sel.kind === 'train'}
          {@const t = trainById(sel.id)}
          {@const st = serverTrainById.get(sel.id) ?? null}
          {@const l = locoById.get(sel.id) ?? null}
          <h2 class="yard-panel-h">Entity · train</h2>
          {#if !t}
            <div class="yard-empty">This train is no longer in the window.</div>
          {:else}
            {#if t.status === 'ARRIVED'}
              <div class="yard-entity-title">{t.title}</div>
              <div class="yard-entity-sub">
                {t.cars.length} cars · {t.outcome === 'arrived' ? 'arrived' : t.outcome === 'cancelled' ? 'cancelled, nothing to board' : 'closed, never arrived'}
                {#if stampOf(t)} · <span class="yard-mono">{stampOf(t)}</span>{/if}
              </div>
              <div class="yard-consist">
                {#each t.cars as c (c.id)}
                  <PacketCard card={c} size="consist" onOpen={openPacket} />
                {/each}
              </div>
            {:else}
              {@render trainBlock(t, partitionOf(t))}
            {/if}
            <dl class="yard-kv">
              {#if l}
                <dt>stage</dt>
                <dd>{STAGES[l.stage]}{l.blocked ? ` · blocked — ${l.blocked}` : ''}</dd>
              {/if}
              {#if st}
                <dt>conductor says</dt>
                <dd>{phaseLabel(st.phase)}{st.at_step ? ` · at ${st.at_step}` : ''}{st.ci_result ? ` · CI ${st.ci_result}` : ''}</dd>
                <!-- Measured from ARRIVED trains, with the leg it covers
                     and the spread named — so the answer to "is this
                     normal?" is on the panel rather than in a head. -->
                <dt>when it lands</dt>
                <dd>
                  {etaReading(st.eta).text}
                  <span class="yard-eta-why">{etaDetail(st.eta)}</span>
                </dd>
                {#if st.block}
                  <dt>block</dt>
                  <dd><span class="yard-trouble">{blockLabel(st.block)}</span></dd>
                {/if}
              {/if}
              {#if t.prUrl}
                <dt>PR</dt>
                <dd><a href={t.prUrl} target="_blank" rel="noreferrer">{t.prUrl}</a></dd>
              {/if}
            </dl>
            <div class="yard-label">Its steps</div>
            <div class="yard-steps">
              {#each entityStops as s, i (i)}
                <div class="yard-step">
                  <span class="yard-lamp-dot {s.lamp}"></span>
                  <span>{s.what}</span>
                  <span class="yard-when yard-mono">{s.when ? clockOf(s.when) : ''}{s.note ? ` · ${s.note}` : ''}</span>
                </div>
              {/each}
              {#if entityJobError}
                <span class="yard-empty">{entityJobError}</span>
              {:else if entityJob === null}
                <span class="yard-empty">reading the train…</span>
              {:else if entityStops.length === 0}
                <span class="yard-empty">no steps on record</span>
              {/if}
            </div>
            {#if entityJob}
              <ArrivalReport job={entityJob} />
            {/if}
            <div class="yard-verbs">
              {#if t.prUrl}
                <a class="yard-verb-link" href={t.prUrl} target="_blank" rel="noreferrer">open PR</a>
              {/if}
              <button type="button" onclick={() => openTrain(t)}>open job page</button>
            </div>
          {/if}
        {:else if sel.kind === 'track'}
          <h2 class="yard-panel-h">Entity · the track</h2>
          <!-- The departure line is the MERGE (0bba59f7). Pre-merge is
               yard work — assembling, inspecting (CI), under repair —
               where a red lamp is status, not alarm; red is deliberately
               NOT softened, it just lives here. Post-merge is transit,
               green by construction, and short. -->
          <div class="yard-gates-head">
            IN THE YARD <span class="yard-gates-n">{split.inYard.length} · assembling · inspecting · under repair — red is work, not a wreck</span>
          </div>
          {#if split.inYard.length === 0}
            <div class="yard-empty">Yard clear — nothing assembling.</div>
          {:else}
            {#each split.inYard as t (t.id)}{@render trainBlock(t, 'in-yard')}{/each}
          {/if}
          <div class="yard-gates-head">
            DEPARTED · IN TRANSIT <span class="yard-gates-n">{split.inTransit.length} · past the merge — irreversible, green by construction</span>
          </div>
          {#if split.inTransit.length === 0}
            <div class="yard-empty">Nothing in transit.</div>
          {:else}
            {#each split.inTransit as t (t.id)}{@render trainBlock(t, 'in-transit')}{/each}
          {/if}
        {:else if sel.kind === 'bay'}
          {@const b = floor.bays[sel.index] ?? null}
          <h2 class="yard-panel-h">Entity · gate bay {sel.index + 1}</h2>
          {#if !b}
            <div class="yard-empty">No such bay on this server's policy.</div>
          {:else if !b.busy}
            <div class="yard-entity-title">available</div>
            <div class="yard-entity-sub">{gatesInUse} / {gateCapacity} in use · {gatesFree} free{gateQueue.length > 0 ? ` · ${gateQueue.length} queued for a bay` : ''}{waitingToGate > 0 ? ` · ${waitingToGate} waiting to gate` : ''}</div>
          {:else}
            <div class="yard-entity-title">{b.branch}</div>
            <div class="yard-entity-sub yard-mono">packet {b.packetId?.slice(0, 8) ?? '—'}</div>
            <dl class="yard-kv">
              <dt>since</dt>
              <dd>{b.since ?? '—'}</dd>
              <dt>elapsed</dt>
              <dd>{b.elapsed ?? '—'}</dd>
              <dt>state</dt>
              <dd>
                {#if b.stale}
                  <span class="yard-trouble">STALE — past the runner's usual; the verdict may never reach the packet. Re-gate.</span>
                {:else}
                  <span class="yard-lamp-dot working"></span>running
                {/if}
              </dd>
            </dl>
            {#if b.packetId}
              {@const pid = b.packetId}
              <div class="yard-verbs">
                <button type="button" onclick={() => openPacket(pid)}>open gate packet</button>
                <button type="button" onclick={() => navigate(entityHref('job', pid))}>open job page</button>
                {#if b.wagonId && b.wagonId !== pid}
                  {@const wid = b.wagonId}
                  <button type="button" onclick={() => select(`car:${wid}`)}>the car</button>
                {/if}
              </div>
            {/if}
          {/if}
        {:else if sel.kind === 'dock'}
          <h2 class="yard-panel-h">Entity · loading dock</h2>
          <!-- The dock is a station rendered (stations.md): when the
               registry served, the header carries the station's own
               facts — the ordering discipline (Q2) and the advisory
               WIP verdict (Q3: warn, don't enforce). The walk upstream
               (David, feedback 3ccb79f5) sits beside them: a queue that
               isn't filling is diagnosed upstream. Navigation, not
               content. -->
          <div class="yard-entity-title">{yard.dock.length} car{yard.dock.length === 1 ? '' : 's'} parked</div>
          <div class="yard-entity-sub yard-dock-facts">
            {#if upstream}
              <button type="button" class="yard-upstream" title={upstream.title} onclick={() => navigate(upstream.href)}>{upstream.label}</button>
            {/if}
            {#if yard.dockStation.source === 'station'}
              <span class="yard-discipline" title="queue discipline">{disciplineLabel(yard.dockStation.discipline)}</span>
              {#if wipAdvisory(yard.dockStation)}
                <span class="yard-wip" title="over the station's advisory WIP limit">{wipAdvisory(yard.dockStation)}</span>
              {/if}
            {/if}
          </div>
          <dl class="yard-kv">
            {#if hold}
              <dt>boards</dt>
              <dd class="yard-cond-v" data-tone={hold.primary.tone}>{hold.primary.text}{#if hold.lastBoard} <span class="yard-since">last board {hold.lastBoard}</span>{/if}</dd>
              {#if hold.next}
                <dt>next</dt>
                <dd class="yard-cond-v" data-tone="muted">{hold.next}</dd>
              {/if}
            {/if}
            {#if boardingRule}
              <dt>rule</dt>
              <dd class="yard-cond-v" data-tone="muted" title="the live cadence rule — never a predicted time">{boardingRule}</dd>
            {/if}
          </dl>
          {#if yard.dock.length === 0}
            <div class="yard-empty">The dock is clear.</div>
          {:else}
            <div class="yard-dock">
              {#each yard.dock as c (c.id)}
                <PacketCard card={c} size="dock" onOpen={openPacket} />
              {/each}
            </div>
          {/if}
        {:else if sel.kind === 'garage'}
          <h2 class="yard-panel-h">Entity · garage</h2>
          <!-- Cars whose latest gate went RED, waiting for rework. An
               empty garage is a claim worth making — nothing is gated
               red. A branch that re-gated green has left already (the
               server keeps only the latest run per branch). -->
          <div class="yard-entity-title">{garage.length > 0 ? `${garage.length} gated red` : 'Empty'}</div>
          <div class="yard-entity-sub">{garage.length > 0 ? 'gated red, awaiting rework' : 'no cars in the garage — every gated branch is green or moving'}</div>
          {#if garage.length > 0}
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
        {:else if sel.kind === 'approach'}
          <h2 class="yard-panel-h">Entity · approach</h2>
          <!-- The approach (f930cda2): the car lifecycle upstream of the
               dock. queued → gating (the bays) → green becomes a parked
               car, RED drops into the garage. Ordered by distance from
               the dock: publishing, red, green-unparked, HELD (gated
               green with the brake deliberately on — not a stranded
               car); each row opens its own packet. -->
          <div class="yard-entity-title">{floor.machines.approach.label}</div>
          <div class="yard-entity-sub">
            gates {gatesInUse} / {gateCapacity} in use · {gatesFree} free{gateQueue.length > 0 ? ` · ${gateQueue.length} queued for a bay` : ''}{waitingToGate > 0 ? ` · ${waitingToGate} waiting to gate` : ''}
          </div>
          {#if yard.approach.length > 0}
            <table class="yard-board">
              <tbody>
                {#each yard.approach as a (a.id)}
                  <tr class="yard-approach" class:is-held={a.state === 'held'} ondblclick={() => openPacket(a.id)}>
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
                    {#if a.hold !== null}
                      <!-- The brake and its reason, where the note goes. -->
                      <td class="yard-stamp yard-hold-note" title={`gated ${a.opened_on} — held, not parked`}>brake on — {a.hold}</td>
                    {:else}
                      <td class="yard-stamp">{a.note ?? a.opened_on}</td>
                    {/if}
                  </tr>
                {/each}
              </tbody>
            </table>
          {:else}
            <div class="yard-empty">The approach is clear — nothing publishing, no verdict unclaimed.</div>
          {/if}
          {#if statusData && statusData.stranded.length > 0}
            <div class="yard-label">Stranded greens — gated, never parked; rebase + re-gate</div>
            <ul class="yard-garage">
              {#each statusData.stranded as s (s.branch)}
                <li class="yard-garage-row"><span class="yard-stranded-branch">{s.branch}</span></li>
              {/each}
            </ul>
          {/if}
          {#if statusData && statusData.held.length > 0}
            <!-- Held greens: the brake deliberately on, with the operator's
                 reason. Neutral colour — not the stranded amber. -->
            <div class="yard-label">Held greens — kept off the dock on purpose; nothing to rescue</div>
            <ul class="yard-garage">
              {#each statusData.held as h (h.branch)}
                <li class="yard-garage-row">
                  <span class="yard-held-branch">{h.branch}</span>
                  <span class="yard-stamp" title={`gated ${h.since} — held, not parked`}>held — {h.reason}</span>
                </li>
              {/each}
            </ul>
          {/if}
        {:else if sel.kind === 'gate-queue'}
          <h2 class="yard-panel-h">Entity · gate queue</h2>
          <!-- The line waiting for a bay. `boss gate --wait` takes a place
               in line when every bay is busy rather than refusing, and a
               queued run holds no bay — so without this lane the floor
               showed nothing at all for it, and a queued gate looked
               exactly like one that never launched. Neutral: a queue is
               the pipeline working to its bound, not an alarm. -->
          <div class="yard-entity-title">{floor.machines.queue.label}</div>
          <div class="yard-entity-sub">
            {gatesInUse} / {gateCapacity} bays in use{queueTypical !== null ? ` · a gate here measures ~${journeyText(queueTypical)}` : ' · no gate measured yet — the waits below are unknown, not zero'}
          </div>
          {#if gateQueue.length === 0}
            <div class="yard-empty">Nothing is waiting for a bay.</div>
          {:else}
            <ul class="yard-garage">
              {#each gateQueue as q (q.packet_id)}
                <li class="yard-garage-row">
                  <span class="yard-held-branch">{q.branch}</span>
                  <span class="yard-garage-check">{queueLabel(q)}</span>
                  <button type="button" class="yard-verb-link" onclick={() => openPacket(q.packet_id)}>open gate packet</button>
                </li>
              {/each}
            </ul>
          {/if}
        {:else if sel.kind === 'arrivals'}
          <h2 class="yard-panel-h">Entity · arrivals</h2>
          <div class="yard-entity-title">{floor.machines.arrivals.label}</div>
          <div class="yard-entity-sub">how each landed — each row is a car; its train's landing report is on the train</div>
          <div class="yard-steps">
            {#each floor.boardRows.filter(r => r.landed) as r (r.id)}
              {@const w = wagonById.get(r.id)}
              {#if w}
                <button type="button" class="yard-step yard-step-btn" onclick={() => select(`car:${w.id}`)}>
                  <span class="yard-lamp-dot {w.lamp}"></span>
                  <span>{w.title}</span>
                  <span class="yard-when yard-mono">{sinceText(w.since, nowMs)} · {w.status}</span>
                </button>
              {/if}
            {/each}
            {#if floor.boardRows.every(r => !r.landed)}
              <span class="yard-empty">none yet</span>
            {/if}
          </div>
        {:else if sel.kind === 'conductor'}
          <h2 class="yard-panel-h">Entity · conductor</h2>
          <!-- THE CONDUCTOR: the actor that boards this dock, read from
               its own firing record. On 2026-09-04 the conductor was
               dead for two and a half hours while this page drew full
               docks and healthy trains — absence of a trouble flag
               rendered as absence of trouble. So: liveness against the
               conductor's own declared heartbeat, the last verb it ran
               and its exit code, the boarding predicate from the live
               cadence rows, and each train in transit at its
               server-computed step. An older server that sends no
               reading gets a line that SAYS "no reading". -->
          <div class="yard-entity-title" class:is-silent={conductor?.silent === true}>{floor.machines.conductor.label}</div>
          <div class="yard-entity-sub">the actor that boards the dock — runs in the cluster; boards, reconciles, cancels stalled reds, files proof</div>
          <div class="yard-conductor" class:is-silent={conductor?.silent === true}>
            <div class="yard-cond-row">
              <span class="yard-cond-k">liveness</span>
              <span class="yard-cond-v" data-tone={liveness.tone}>{liveness.text}</span>
            </div>
            <div class="yard-cond-row">
              <!-- The heartbeat rule's VERB (reconcile), read from its
                   registry row, and the exit code of its last pass. -->
              <span class="yard-cond-k">last verb</span>
              <span class="yard-cond-v" data-tone={lastVerb.tone}>{lastVerb.text}</span>
            </div>
            <!-- "boards": WHY the dock is not boarding right now and what
                 clears it, read server-side from the conductor's own facts.
                 The depth rule has no clock, so the "next" line is always
                 "once <the hold clears>" — never a time. -->
            {#if hold}
              <div class="yard-cond-row">
                <span class="yard-cond-k">boards</span>
                <span class="yard-cond-v" data-tone={hold.primary.tone}>{hold.primary.text}</span>
                {#if hold.lastBoard}<span class="yard-since">last board {hold.lastBoard}</span>{/if}
              </div>
              {#if hold.next}
                <div class="yard-cond-row">
                  <span class="yard-cond-k">next</span>
                  <span class="yard-cond-v" data-tone="muted">{hold.next}</span>
                </div>
              {/if}
            {/if}
            <div class="yard-cond-row">
              <span class="yard-cond-k">rule</span>
              <span class="yard-cond-v" data-tone="muted" title="the live cadence rule — never a predicted time">{boardingRule}</span>
            </div>
            {#each moving as t (t.id)}
              {@const since = movingFor(t)}
              <div class="yard-cond-row" class:is-blocked={!!t.block}>
                <span class="yard-cond-k">moving</span>
                <span class="yard-cond-v">{t.title} · {phaseLabel(t.phase)}{t.at_step ? ` · at ${t.at_step}` : ''}</span>
                {#if t.block}
                  <span class="yard-trouble" title="the block the conductor recorded">{blockLabel(t.block)}</span>
                {/if}
                {#if since}<span class="yard-since">for {since}</span>{/if}
              </div>
            {/each}
            {#if moving.length === 0}
              <div class="yard-cond-row">
                <span class="yard-cond-k">moving</span>
                <span class="yard-cond-v" data-tone="muted">no train in transit</span>
              </div>
            {/if}
          </div>
        {:else if sel.kind === 'runner'}
          {@const r = floor.machines.runner}
          {@const rid = runnerPacketId(r)}
          <h2 class="yard-panel-h">Entity · deploy runner</h2>
          <!-- THE SHED reads the newest converge ops-request: the packet
               the dispatcher files when a train merges and the forge's
               ops-runner answers by starting cluster-deploy-runner. The
               packet proves the unit was STARTED; whether the cluster
               moved is the tower's reading, beside it. -->
          <div class="yard-entity-title" class:is-silent={r.kind === 'failed'}>{runnerLabel(r, nowMs)}</div>
          <div class="yard-entity-sub">cluster-deploy-runner on the forge — started by the ops-request the dispatcher files when a train merges (rule converge-on-merge); builds the image, pushes it, rolls the cluster, waits for Ready</div>
          <dl class="yard-kv">
            {#if r.kind === 'requested'}
              <dt>requested</dt>
              <dd>{r.at ? `${clockOf(r.at)} · ${sinceText(r.at, nowMs)} ago` : '—'} · waiting for the ops-runner on forge (polls every minute)</dd>
            {:else if r.kind === 'running'}
              <dt>started</dt>
              <dd>{r.since ? `${clockOf(r.since)} · ${sinceText(r.since, nowMs)} ago` : '—'}{r.host ? ` on ${r.host}` : ''}</dd>
            {:else if r.kind === 'failed'}
              <dt>failed</dt>
              <dd>{r.at ? clockOf(r.at) : '—'} · <span class="yard-trouble">{r.reason}</span></dd>
            {:else if r.kind === 'idle' && r.last}
              <dt>last converge</dt>
              <dd>{clockOf(r.last.at)} · {sinceText(r.last.at, nowMs)} ago{r.last.host ? ` on ${r.last.host}` : ''}</dd>
            {/if}
            <dt>cluster</dt>
            <dd>{clusterLabel(floor.machines.cluster)}</dd>
            <dt>usual</dt>
            <dd class="yard-since-muted">~{CONVERGE_USUAL_MINUTES} min build → push → roll → Ready — a drawing scale, not a reading</dd>
          </dl>
          {#if rid !== null}
            <div class="yard-label">The packet's steps</div>
            <div class="yard-steps">
              {#each entityStops as s, i (i)}
                <div class="yard-step">
                  <span class="yard-lamp-dot {s.lamp}"></span>
                  <span>{s.what}</span>
                  <span class="yard-when yard-mono">{s.when ? clockOf(s.when) : ''}{s.note ? ` · ${s.note}` : ''}</span>
                </div>
              {/each}
              {#if entityJobError}
                <span class="yard-empty">{entityJobError}</span>
              {:else if entityJob === null}
                <span class="yard-empty">reading the packet…</span>
              {:else if entityStops.length === 0}
                <span class="yard-empty">no steps on record</span>
              {/if}
            </div>
            <div class="yard-verbs">
              <button type="button" onclick={() => openPacket(rid)}>open packet</button>
              <button type="button" onclick={() => navigate(entityHref('job', rid))}>open job page</button>
            </div>
          {:else if r.kind === 'unknown'}
            <div class="yard-empty">No reading — the page has not read the converge ops-requests yet, or the read failed.</div>
          {:else}
            <div class="yard-empty">No converge packet in the window.</div>
          {/if}
        {:else if sel.kind === 'cluster'}
          {@const c = floor.machines.cluster}
          <h2 class="yard-panel-h">Entity · cluster</h2>
          <!-- THE TOWER: this browser's own read of /api/jobs/health —
               the build the running jobs API reports, the same field
               the conductor verifies convergence against. Dark when it
               does not answer: the system of record observed from
               outside, not from inside. -->
          <div class="yard-entity-title" class:is-silent={c.kind === 'dark'}>{clusterLabel(c)}</div>
          <div class="yard-entity-sub">the system of record observed from outside — what this browser gets from /api/jobs/health: the build the running jobs API was made from</div>
          <dl class="yard-kv">
            <dt>reachable</dt>
            <dd>
              {#if c.kind === 'ready'}yes{:else if c.kind === 'dark'}<span class="yard-trouble">no — {c.error}</span>{:else}not read yet{/if}
            </dd>
            {#if c.kind === 'ready'}
              <dt>build</dt>
              <dd class="yard-mono">{c.commit ?? 'not reported'}</dd>
              <dt>last merge</dt>
              <dd>
                {#if !lastMerged}
                  no merged train in the window
                {:else if c.commit !== null && shasMatch(c.commit, lastMerged.ref)}
                  <span class="yard-mono">{lastMerged.ref.slice(0, 7)}</span> ({lastMerged.title}) — the cluster is on it
                {:else}
                  <span class="yard-mono">{lastMerged.ref.slice(0, 7)}</span> ({lastMerged.title}) — <span class="yard-trouble">the cluster is not on it</span>
                {/if}
              </dd>
            {/if}
            {#if c.kind !== 'unknown'}
              <dt>since</dt>
              <dd>{clockOf(c.since)} · {sinceText(c.since, nowMs)}</dd>
            {/if}
          </dl>
        {/if}
      </div>
    </div>

    <!-- THE LOWER DECK: what the floor produced today, and what fired
         what — both from the packets the page already holds. -->
    <div class="yard-deck yard-deck-lower">
      <div class="yard-panel">
        <ProductionPanel
          production={prod}
          {alerts}
          dockHold={floor.machines.dock.parked > 0 && floor.machines.dock.held
            ? { text: floor.machines.dock.held, since: statusData?.boarding.last_board_at ?? null }
            : null}
          {nowMs}
          onselect={select} />
      </div>
      <div class="yard-panel">
        <SignalsPanel signals={signalRows} onopen={openPacket} />
      </div>
    </div>

    <!--
      THE SCOREBOARD. David, 2026-08-28: "We should have these stats at
      the top of the Train Yard if they are what matter." A statistic
      nobody sees cannot discipline a decision. Rendered only when a
      version has actually RESOLVED something, so the panel is absent
      rather than showing zeros for a version whose packets are all
      still in flight.
    -->
    {#if yard.delivery.length > 0}
      <div class="yard-section">DELIVERY</div>
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

    <!-- Arrivals are trains that ARRIVED — a cancelled train never
         did, and it keeps its own muted line below rather than
         disappearing. Ordered by the best arrival instant each train
         carries (the column's tooltip names the evidence). Each row
         opens the train's Job, where the landing report is. -->
    <div class="yard-section">RECENT ARRIVALS</div>
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
        AWAITING PROOF <span class="yard-n">{yard.awaitingProof.length}</span>
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

    {#snippet trainBlock(t: TrainRow, partition: YardPartition)}
      {@const pending = t.cancelRequested ?? cancelSent[t.id] ?? null}
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
          <!-- The cancel control. A refusal outranks a request (the
               conductor looked and the train had already merged); a
               request — read off the Job, or sent from this page a
               moment ago — replaces the button; the button itself is
               offered only where canOfferCancel says so: in the yard,
               troubled, privileged, not yet asked. -->
          {#if t.cancelRefused}
            <span
              class="yard-cancel-chip is-refused"
              title="the conductor looked — the train had already merged"
              >cancel refused — already merged</span>
          {:else if pending}
            <span
              class="yard-cancel-chip"
              title={`requested by ${pending.by}${pending.at ? ` at ${pending.at}` : ''} — ${pending.reason}`}
              >cancel requested — the conductor acts within 10 min</span>
          {:else if canOfferCancel(t, partition, viewerPrivileged)}
            <button
              type="button"
              class="yard-cancel-btn"
              title="ask the conductor to cancel this train — the cars return to the dock unstruck"
              onclick={() => openCancel(t)}>cancel train</button>
          {/if}
          {#if t.eta.phase !== 'arrived'}
            {@const srv = serverTrainById.get(t.id)?.eta ?? null}
            <span
              class="yard-eta"
              class:est={t.eta.kind === 'eta'}
              title={srv && t.eta.kind !== 'eta' ? etaDetail(srv) : etaTitle(t.eta)}>
              {etaText(t.eta)}
            </span>
            <!-- The SERVER's measured estimate, shown when the
                 client-side projection has none. That is not a rare
                 case: `trainEta` samples the last 5 ARRIVED trains out
                 of the 40 pr-trains this page fetches, and measured
                 2026-09-10 exactly ONE of the 40 most recent had arrived
                 — 696 of 1,014 are boards the consist check refused.
                 Reaching 10 measurable arrivals from the newest end
                 needs a window 583 deep, so no client fetch can get
                 there; /api/yard/status narrows on the outcome in SQL
                 instead. One chip at a time, never two numbers. -->
            {#if srv && t.eta.kind !== 'eta'}
              {@const r = etaReading(srv)}
              {#if srv.kind === 'estimate'}
                <span class="yard-eta is-measured" class:late={r.tone === 'err'} title={etaDetail(srv)}>
                  {r.text}
                </span>
              {/if}
            {/if}
          {/if}
          {#if t.status === 'CONVERGING'}
            {@const since = convergingFor(t)}
            {#if since}
              <span
                class="yard-since"
                title="deployed — awaiting the cluster to converge on the merge"
                >converging for {since}</span
              >
            {/if}
          {/if}
          <span class="yard-stamp">{stampOf(t)}</span>
        </div>
        {#if cancelFor === t.id}
          <form
            class="yard-cancel-form"
            onsubmit={e => {
              e.preventDefault();
              void requestCancel(t);
            }}>
            <label class="yard-cancel-label" for="yard-cancel-reason-{t.id}">reason</label>
            <input
              id="yard-cancel-reason-{t.id}"
              class="yard-cancel-reason"
              type="text"
              bind:value={cancelReason}
              placeholder="why this train is pulled — read later by whoever asks"
              disabled={cancelBusy}
              autocomplete="off" />
            <button
              type="submit"
              class="yard-cancel-btn is-confirm"
              disabled={cancelBusy || cancelReason.trim() === ''}
              >{cancelBusy ? 'requesting…' : 'request cancel'}</button>
            <button type="button" class="yard-cancel-btn" onclick={closeCancel} disabled={cancelBusy}
              >keep</button>
            {#if cancelError}<span class="yard-cancel-err">{cancelError}</span>{/if}
          </form>
        {/if}
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
  .yard-mono { font-family: var(--font-mono, ui-monospace, monospace); font-variant-numeric: tabular-nums; }

  /* The alerts strip: each alert a button to its subject. */
  .yard-alerts { display: flex; flex-wrap: wrap; gap: var(--s2, 8px); min-height: 30px; margin: 0 0 var(--s3, 12px); }
  .yard-alert {
    display: inline-flex; align-items: center; gap: var(--s2, 8px);
    padding: 5px 10px 5px 8px; border: 1px solid var(--border-strong, #3a434d);
    background: var(--ink, #12161c); color: var(--fog, #e8ecef); cursor: pointer;
    text-align: left; font: inherit; font-size: 12.5px; border-radius: 0;
  }
  .yard-alert.err { border-color: color-mix(in srgb, var(--err, #e2685c) 60%, var(--hairline, #2a3138)); }
  .yard-alert.warn { border-color: color-mix(in srgb, var(--warn, #d9a441) 55%, var(--hairline, #2a3138)); }
  .yard-alert time { color: var(--static, #7a838c); font-size: 11px; }
  .yard-quiet { color: var(--text-faint, #5c656e); font-size: 12.5px; padding: 6px 0; display: inline-flex; gap: var(--s2, 8px); align-items: center; }

  /* The small round lamps the strip, the entity panel and the board share. */
  .yard-lamp-dot { display: inline-block; width: 8px; height: 8px; border-radius: 50%; background: var(--static, #7a838c); flex: none; margin-right: 6px; position: relative; top: -1px; }
  .yard-lamp-dot.ok { background: var(--ok, #4fb98a); box-shadow: 0 0 6px var(--ok, #4fb98a); }
  .yard-lamp-dot.working { background: var(--signal, #5fd4a8); box-shadow: 0 0 6px var(--signal, #5fd4a8); animation: yard-pulse 1.6s ease-in-out infinite; }
  .yard-lamp-dot.warn { background: var(--warn, #d9a441); box-shadow: 0 0 6px var(--warn, #d9a441); }
  .yard-lamp-dot.err { background: var(--err, #e2685c); box-shadow: 0 0 7px var(--err, #e2685c); animation: yard-blink 1s steps(2) infinite; }
  .yard-lamp-dot.off { background: var(--border-strong, #3a434d); }
  @keyframes yard-blink { 50% { opacity: 0.25; } }

  /* The deck: the board left, the entity panel right; one column when narrow. */
  .yard-deck { display: grid; grid-template-columns: 3fr 2fr; gap: var(--s3, 12px); margin-top: var(--s3, 12px); }
  .yard-panel { background: var(--ink, #12161c); border: 1px solid var(--hairline, #2a3138); padding: var(--s4, 16px); min-height: 260px; min-width: 0; }
  .yard-panel-h { margin: 0; font-size: 11px; letter-spacing: var(--ls-label, 0.1em); text-transform: uppercase; color: var(--static, #7a838c); font-weight: 600; }
  .yard-entity-title { font-size: 18px; font-weight: 600; margin: var(--s2, 8px) 0 var(--s1, 4px); text-wrap: balance; overflow-wrap: anywhere; }
  .yard-entity-title.is-silent { color: var(--err, #e2685c); }
  .yard-entity-sub { color: var(--static, #7a838c); font-size: 13px; margin-bottom: var(--s3, 12px); overflow-wrap: anywhere; }
  .yard-dock-facts { display: flex; flex-wrap: wrap; gap: 10px; align-items: center; }
  .yard-kv { display: grid; grid-template-columns: max-content 1fr; gap: 4px var(--s4, 16px); font-size: 13px; margin: 0 0 var(--s3, 12px); }
  .yard-kv dt { color: var(--static, #7a838c); }
  .yard-kv dd { margin: 0; overflow-wrap: anywhere; }
  .yard-kv a { color: var(--signal, #5fd4a8); }
  .yard-label { font-size: 11px; letter-spacing: var(--ls-label, 0.1em); text-transform: uppercase; color: var(--static, #7a838c); font-weight: 600; margin-top: var(--s3, 12px); }
  .yard-steps { display: grid; gap: 3px; margin-top: var(--s2, 8px); }
  .yard-step { display: grid; grid-template-columns: 14px minmax(120px, 1fr) 1.4fr; gap: var(--s2, 8px); align-items: baseline; font-size: 12.5px; }
  .yard-step-btn { background: transparent; border: 0; color: inherit; font: inherit; text-align: left; padding: 2px 0; cursor: pointer; border-radius: 0; }
  .yard-step-btn:hover, .yard-step-btn:focus-visible { color: var(--signal, #5fd4a8); }
  .yard-when { color: var(--static, #7a838c); font-size: 11.5px; overflow-wrap: anywhere; }
  .yard-verbs { display: flex; gap: var(--s2, 8px); flex-wrap: wrap; margin-top: var(--s4, 16px); }
  .yard-verbs button, .yard-verb-link {
    background: transparent; color: var(--fog, #e8ecef); border: 1px solid var(--border-strong, #3a434d);
    padding: 5px 10px; font: inherit; font-size: 11px; letter-spacing: var(--ls-label, 0.1em);
    text-transform: uppercase; font-weight: 600; cursor: pointer; text-decoration: none; border-radius: 0;
  }
  .yard-verbs button:hover, .yard-verbs button:focus-visible, .yard-verb-link:hover, .yard-verb-link:focus-visible {
    color: var(--signal, #5fd4a8); border-color: var(--signal, #5fd4a8);
  }
  .yard-link { background: transparent; border: 0; padding: 0; font: inherit; color: var(--signal, #5fd4a8); cursor: pointer; text-align: left; }
  .yard-stranded-branch { color: var(--warn, #d9a441); font-weight: 600; overflow-wrap: anywhere; }
  .yard-held-branch { color: var(--static, #7a838c); font-weight: 600; overflow-wrap: anywhere; }

  .yard-section {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px; letter-spacing: var(--ls-eyebrow, 0.3em);
    color: var(--signal, #5FD4A8); margin: 28px 0 8px;
    display: flex; align-items: center; gap: 12px;
  }
  .yard-section::after { content: ''; flex: 1; border-top: 1px solid var(--hairline, #2A3138); }
  .yard-n { color: var(--static, #7A838C); }
  /* Station facts in the dock's header: discipline stays quiet
     (static grey, same mono caps), the WIP advisory wears --warn —
     the one state color, present only when the queue exceeds its
     declared bandwidth. */
  .yard-discipline { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px; color: var(--static, #7A838C); letter-spacing: var(--ls-nav, 0.14em); text-transform: uppercase; }
  .yard-wip { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px; color: var(--warn, #d9a441); border: 1px solid var(--warn, #d9a441);
    padding: 1px 7px; letter-spacing: 0.1em; }
  /* The walk upstream: the chip grammar exactly (mono caps, hairline,
     radius 0, --static) — an instrument, not a call to action. It
     brightens to --signal on hover and focus. */
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
    border-bottom: 1px solid var(--hairline, #2A3138); font-size: 14px; flex-wrap: wrap; }
  /* The title takes its own line in the panel; the chips wrap beneath
     it rather than squeezing the name to an ellipsis. */
  .yard-trainname { flex: 1 1 100%; min-width: 0; overflow-wrap: anywhere; }
  /* The flatbed: consist cards sit on VOID so the packets read as
     cargo loaded onto the train, the same cards that wait in the dock. */
  .yard-consist { display: flex; flex-wrap: wrap; gap: 8px; padding: 10px 12px;
    background: var(--bg, var(--void, #0D1014)); }
  .yard-dock { display: grid; gap: 10px;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); }
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
  /* The cancel control, in the badge idiom. The button is quiet until
     hovered — it sits beside a red badge and must not shout over it.
     The chip that replaces it reads in the warn tone because the
     request is pending, not done: the conductor acts, this page asks. */
  .yard-cancel-btn {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    padding: 1px 8px;
    border: 1px solid var(--hairline, #2A3138);
    border-radius: 2px;
    background: transparent;
    color: var(--text-dim, #78716c);
    cursor: pointer;
  }
  .yard-cancel-btn:hover:not(:disabled), .yard-cancel-btn:focus-visible {
    color: var(--err, #e2685c); border-color: var(--err, #e2685c);
  }
  .yard-cancel-btn.is-confirm { color: var(--err, #e2685c); border-color: var(--err, #e2685c); }
  .yard-cancel-btn:disabled { opacity: 0.5; cursor: default; }
  .yard-cancel-chip {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
    letter-spacing: 0.04em;
    padding: 1px 6px;
    border: 1px solid var(--warn, #d9a441);
    color: var(--warn, #d9a441);
    border-radius: 2px;
    white-space: nowrap;
  }
  .yard-cancel-chip.is-refused { border-color: var(--static, #7A838C); color: var(--static, #7A838C); }
  .yard-cancel-form {
    display: flex; flex-wrap: wrap; align-items: center; gap: 8px;
    padding: 8px 12px; border-top: 1px solid var(--hairline, #2A3138);
  }
  .yard-cancel-label { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: 0.1em; color: var(--static, #7A838C); text-transform: uppercase; }
  .yard-cancel-reason {
    flex: 1 1 240px; min-width: 160px;
    font: inherit; font-size: 12.5px; padding: 3px 8px;
    background: var(--bg, var(--void, #0D1014)); color: var(--text, #C7CED6);
    border: 1px solid var(--hairline, #2A3138); border-radius: 2px;
  }
  .yard-cancel-err { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    color: var(--err, #e2685c); flex-basis: 100%; }
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
  .yard-appr-state[data-state='gated-red'] { color: var(--err, #e2685c); }
  .yard-appr-state[data-state='gated-green'] { color: var(--ok, #4fb98a); }
  .yard-appr-state[data-state='publishing'] { color: var(--static, #7A838C); }
  /* A HELD car: brake deliberately on. Not the ok-green of a gated
     green (which reads "ready — forgotten?"), not the warn of a running
     gate, not the err of a red: a boxed lamp on a dimmed row, with the
     reason where the note goes. Parked-brake-on, not stuck. */
  .yard-appr-state[data-state='held'] { border: 1px solid var(--static, #7A838C);
    padding: 1px 6px; }
  .yard-approach.is-held td { color: var(--static, #7A838C); }
  .yard-approach.is-held .yard-appr-state,
  .yard-approach.is-held .yard-hold-note { color: var(--text, #C7CED6); }
  .yard-dot { display: inline-block; width: 7px; height: 7px; border-radius: 50%;
    background: var(--signal, #5FD4A8); margin-right: 8px;
    animation: yard-pulse 1.4s ease-in-out infinite; }
  @keyframes yard-pulse { 50% { opacity: 0.35; } }
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
  /* The server-measured estimate: brighter than a phase-only chip
     because it IS a measurement, and red once the train is past the
     slowest arrival on record — a state past its own threshold has to
     look past it (CLAUDE.md §Diagnosis). */
  .yard-eta-why { display: block; color: var(--static, #7A838C); font-size: 11px; }
  .yard-eta.is-measured { color: var(--text, #C7CED6); letter-spacing: 0.04em; }
  .yard-eta.is-measured.late { color: var(--alarm, #E5484D); border-color: var(--alarm, #E5484D); }
  /* The converge wait, as elapsed time — an active signal in the
     signal-green idiom, not the muted arrival stamp. */
  .yard-since { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: 0.1em; color: var(--signal, #5FD4A8); font-variant-numeric: tabular-nums;
    white-space: nowrap; }
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
  /* Sub-headers inside the entity panel: quieter than a section
     header, the same mono-caps grammar. */
  .yard-gates-head {
    font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: var(--ls-nav, 0.14em); text-transform: uppercase;
    color: var(--static, #7A838C); margin: 14px 0 8px;
    display: flex; align-items: center; gap: 10px; flex-wrap: wrap;
  }
  .yard-gates-n { color: var(--static, #7A838C); text-transform: none; letter-spacing: 0; }
  /* The garage: gated-red cars, one row each. The err lamp on the
     branch, the failing check beside it. */
  .yard-garage { list-style: none; padding: 0; margin: 0;
    border: 1px solid var(--hairline, #2A3138); background: var(--card, var(--ink, #12161C)); }
  .yard-garage-row { display: flex; align-items: center; gap: 12px; padding: 8px 12px;
    border-bottom: 1px solid var(--hairline, #2A3138); font-size: 13px; flex-wrap: wrap; }
  .yard-garage-row:last-child { border-bottom: none; }
  .yard-garage-branch { color: var(--err, #e2685c); font-weight: 600;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; flex: 1; }
  .yard-garage-check { font-family: var(--font-mono, ui-monospace, monospace); font-size: 12px;
    color: var(--static, #7A838C); }
  /* The conductor: the garage's card + hairline grammar, one reading a
     row. Readings wear the lamp palette (ok / warn / err / muted). A
     SILENT conductor turns the border err — while it is silent, every
     section it writes is last-known-good, not current, and the whole
     block has to say so. */
  .yard-conductor { border: 1px solid var(--hairline, #2A3138);
    background: var(--card, var(--ink, #12161C)); padding: 4px 0; }
  .yard-conductor.is-silent { border-color: var(--err, #e2685c); }
  .yard-cond-row { display: flex; align-items: baseline; gap: 12px; padding: 5px 12px;
    font-size: 13px; flex-wrap: wrap; }
  .yard-cond-k { font-family: var(--font-mono, ui-monospace, monospace); font-size: 10px;
    letter-spacing: 0.14em; text-transform: uppercase; color: var(--static, #7A838C);
    flex: 0 0 72px; }
  .yard-cond-v { font-family: var(--font-mono, ui-monospace, monospace); font-size: 12px;
    font-variant-numeric: tabular-nums; color: var(--text, #C7CED6); min-width: 0; }
  .yard-cond-v[data-tone='ok'] { color: var(--ok, #4fb98a); }
  .yard-cond-v[data-tone='warn'] { color: var(--warn, #d9a441); }
  .yard-cond-v[data-tone='err'] { color: var(--err, #e2685c); }
  .yard-cond-v[data-tone='muted'] { color: var(--static, #7A838C); }
  .yard-cond-row.is-blocked .yard-cond-v { color: var(--err, #e2685c); }
  .yard-flow { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: var(--ls-nav, 0.14em); color: var(--static, #7A838C);
    border-top: 1px solid var(--hairline, #2A3138); margin-top: 28px; padding-top: 12px; }
  .yard-flow em { color: var(--signal, #5FD4A8); font-style: normal; }

  .yard-deck-lower { grid-template-columns: 1fr 1fr; }
  .yard-since-muted { color: var(--static, #7a838c); }
  @media (max-width: 1000px) { .yard-deck { grid-template-columns: 1fr; } }
  @media (prefers-reduced-motion: reduce) {
    .yard-dot, .yard-lamp-dot { animation: none; }
    .yard-upstream { transition: none; }
  }
</style>
