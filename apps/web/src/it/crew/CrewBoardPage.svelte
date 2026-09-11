<script lang="ts">
  // The Crew Board — the MIDDLE third of the operator surface.
  //
  // The Train Yard shows landed work and the Marshalling Yard shows work
  // waiting. The interval in between — an actor building something, while
  // it happens — had no surface at all. This is it. Decided on backlog
  // 04c5bbc0 (David, 2026-09-11: "Port the Crew Board as a new sidebar
  // page in IT"), prototyped on a real snapshot 2026-09-08.
  //
  // The shaping lives in `crew.ts` and is unit-tested there; this file is
  // render only. Every figure on this page comes from one of four reads
  // and nothing on it is computed from a constant — the rule the
  // Marshalling Yard states and this page inherits: NO NUMBER THIS
  // SURFACE MAKES UP. Where a figure cannot be counted the page prints
  // the reason, because a zero on a board about who is working reads as
  // "nobody is".
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { formatRelative } from '@boss/web-kit/ui/date';
  import { appToday } from '@boss/web-kit/sim-clock';
  import { formatActor } from '../../data/actor';
  import { href } from '../../router';
  import {
    actorCards,
    CAR_WINDOW,
    fetchCrew,
    pipelineTrack,
    takenNotProgressed,
    waitText,
    type ActorCard,
    type CrewState,
    type TrackCar,
  } from './crew';

  let crew = $state<CrewState | null>(null);
  // One clock for every relative stamp, taken when the data arrived —
  // formatRelative takes `now` explicitly, no hidden wallclock.
  let loadedAt = $state<Date>(new Date());

  async function refresh(): Promise<void> {
    crew = await fetchCrew();
    loadedAt = new Date();
  }

  onMount(() => {
    void refresh();
    // This board is about what is happening NOW, so it refreshes faster
    // than the estate's 60s — but not on a tight loop: the four reads
    // behind it include a 60-packet job list.
    const t = setInterval(() => void refresh(), 20_000);
    return () => clearInterval(t);
  });

  // Every lane needs all four reads to be honest, so each section below
  // branches on its own sources rather than one page-wide spinner.
  const track = $derived(
    crew && crew.cars.kind === 'ready' && crew.gateRuns.kind === 'ready' && crew.yard.kind === 'ready'
      ? pipelineTrack(crew.cars.data, crew.gateRuns.data, crew.yard.data)
      : null,
  );

  const cards = $derived<ReadonlyArray<ActorCard> | null>(
    crew && crew.cars.kind === 'ready' && crew.gateRuns.kind === 'ready' && crew.waits.kind === 'ready'
      ? actorCards({
          cars: crew.cars.data,
          gateRuns: crew.gateRuns.data,
          waits: crew.waits.data,
          today: appToday(),
        })
      : null,
  );

  // The staleness floor for "taken, not progressed". A step claimed
  // minutes ago is somebody working, not an alarm; a day is the point at
  // which a claim stops explaining itself.
  const STALE_AFTER_DAYS = 1;
  const stalled = $derived(
    crew && crew.waits.kind === 'ready'
      ? takenNotProgressed(crew.waits.data, STALE_AFTER_DAYS)
      : null,
  );

  const realCrew = $derived(cards?.filter((c) => c.lane !== 'sim') ?? []);
  const simCrew = $derived(cards?.filter((c) => c.lane === 'sim') ?? []);

  const LANE_LABEL: Readonly<Record<string, string>> = {
    human: 'human',
    agent: 'agent',
    automation: 'automation',
    sim: 'sim',
  };

  function jobHref(packetId: string | null): string | null {
    return packetId === null ? null : href(`/ux/jobs/${packetId}`);
  }

  const STAGES: ReadonlyArray<{ key: keyof NonNullable<typeof track>; label: string; blank: string }> = [
    { key: 'building', label: 'BUILDING', blank: 'no car has a branch without a gate run' },
    { key: 'gating', label: 'GATING', blank: 'no gate bay is occupied' },
    { key: 'parked', label: 'PARKED', blank: 'the dock is empty' },
    { key: 'boarded', label: 'BOARDED', blank: 'no car is in transit' },
    { key: 'landed', label: 'LANDED', blank: 'no merged car in this window' },
  ];

  function stageCars(k: keyof NonNullable<typeof track>): ReadonlyArray<TrackCar> {
    return track ? track[k] : [];
  }
</script>

<div class="crew-root">
  <PageHeader
    eyebrow="IT · Delivery"
    title="The Crew Board"
    subtitle="Who is building what, right now — the interval between work waiting and work landed"
  />

  {#if !crew}
    <p class="crew-quiet">Reading the pipeline…</p>
  {:else}
    <!-- ============================ THE TRACK ======================== -->
    <div class="crew-section">00 — THE BUILDER PIPELINE</div>

    {#if crew.yard.kind === 'failed'}
      <p class="crew-fail load-failed">
        The yard did not answer: {crew.yard.error}. This page refuses to guess — an unreachable
        yard is not an idle pipeline.
      </p>
    {:else if crew.cars.kind === 'failed'}
      <p class="crew-fail load-failed">
        The car packets did not answer: {crew.cars.error}. Without them the track cannot say what
        is being built.
      </p>
    {:else if crew.gateRuns.kind === 'failed'}
      <p class="crew-fail load-failed">
        The gate registry did not answer: {crew.gateRuns.error}. BUILDING is defined as "a branch
        with no gate run behind it", so without the registry this page cannot tell a car being
        built from one being assessed, and refuses to draw either.
      </p>
    {:else if track}
      <div class="crew-track">
        {#each STAGES as s (s.key)}
          <div class="crew-stage">
            <div class="crew-stage-head">
              <span class="crew-stage-label">{s.label}</span>
              <span class="crew-stage-count">{stageCars(s.key).length}</span>
            </div>
            {#if stageCars(s.key).length === 0}
              <p class="crew-stage-blank">{s.blank}</p>
            {:else}
              {#each stageCars(s.key) as c (c.branch)}
                <div class="crew-car" class:troubled={c.troubled}>
                  {#if jobHref(c.packetId)}
                    <a class="crew-car-branch" href={jobHref(c.packetId)}>{c.branch}</a>
                  {:else}
                    <span class="crew-car-branch">{c.branch}</span>
                  {/if}
                  <span class="crew-car-title">{c.title}</span>
                  <span class="crew-car-detail">{c.detail}</span>
                </div>
              {/each}
            {/if}
          </div>
        {/each}
      </div>

      {#if track.garage.length > 0}
        <!-- A siding, not a stage. Hiding a failed gate would make the
             track read healthier than it is. -->
        <div class="crew-garage">
          <span class="crew-garage-label">OFF THE TRACK — gate failed</span>
          {#each track.garage as c (c.branch)}
            <div class="crew-car troubled">
              <span class="crew-car-branch">{c.branch}</span>
              <span class="crew-car-title">{c.title}</span>
              <span class="crew-car-detail">{c.detail}</span>
            </div>
          {/each}
        </div>
      {/if}

      <p class="crew-note">
        GATING and PARKED are the yard's own reading of the gate registry and the dock station.
        BUILDING is derived: an open car packet with a branch that the gate registry has never
        seen. A packet with no branch is never drawn — three such packets on 2026-09-10 were
        abandoned cadence sweeps, not builds. LANDED can only show what the last {CAR_WINDOW} car
        packets hold; it is a window, not a total.
      </p>
    {/if}

    <!-- ============================ THE CREW ========================= -->
    <div class="crew-section">01 — THE CREW</div>

    {#if crew.waits.kind === 'failed'}
      <p class="crew-fail load-failed">
        The queue-age projection did not answer: {crew.waits.error}. Without it the board cannot
        say what anyone is holding.
      </p>
    {:else if cards === null}
      <p class="crew-fail load-failed">
        The crew cannot be derived: one of the car, gate or queue reads failed above.
      </p>
    {:else if realCrew.length === 0}
      <p class="crew-stage-blank">
        No actor holds a step or signed a completion in this window.
      </p>
    {:else}
      <div class="crew-cards">
        {#each realCrew as a (a.id)}
          <div class="crew-card">
            <div class="crew-card-head">
              <span class="crew-card-name">{formatActor(a.id)}</span>
              <span class="crew-lane crew-lane-{a.lane}">{LANE_LABEL[a.lane]}</span>
            </div>
            <div class="crew-card-figs">
              <div class="crew-fig">
                <span class="crew-fig-n">{a.holds}</span>
                <span class="crew-fig-l">holding<br /><em>all open work</em></span>
              </div>
              <div class="crew-fig">
                <span class="crew-fig-n">{a.completedToday}</span>
                <span class="crew-fig-l">done today<br /><em>car packets only</em></span>
              </div>
            </div>
            <div class="crew-card-last">
              last write
              {#if a.lastWriteAt === null}
                <!-- Never a date we do not have. -->
                <span class="crew-unknown">not recorded in this window</span>
              {:else}
                <span class="crew-when">{formatRelative(a.lastWriteAt, loadedAt)}</span>
              {/if}
            </div>
            {#if a.holding.length > 0}
              <ul class="crew-holding">
                {#each a.holding.slice(0, 3) as w (w.stepId ?? w.jobId)}
                  <li>
                    <a href={href(`/ux/jobs/${w.jobId}`)}>{w.stepTitle}</a>
                    <span class="crew-holding-meta">{w.jobKind} · {waitText(w)}</span>
                  </li>
                {/each}
                {#if a.holding.length > 3}
                  <li class="crew-holding-more">+{a.holding.length - 3} more</li>
                {/if}
              </ul>
            {/if}
          </div>
        {/each}
      </div>

      {#if simCrew.length > 0}
        <p class="crew-note">
          {simCrew.length} simulated actor(s) are held out of the crew above: the brewery's event
          clock runs about a thousand times faster than the wall, so their figures cannot be read
          on the same scale.
        </p>
      {/if}

      <p class="crew-note">
        There is no actor roster to read — no agents table, and the employees roster holds humans
        only — so this crew is DERIVED: an actor appears because it holds a step or signed a
        completion. The two figures have different scopes on purpose and each says which.
        "Done today" counts step completions on car packets, not writes: the audit tail carries
        its actor only inside the event payload, offers no actor filter and caps a page at 500
        rows, so a per-actor write rate cannot be counted here. The prototype's writes-per-hour
        strip is therefore absent rather than invented.
      </p>
    {/if}

    <!-- ===================== TAKEN, NOT PROGRESSED =================== -->
    <div class="crew-section">02 — TAKEN, NOT PROGRESSED</div>

    {#if crew.waits.kind === 'failed'}
      <p class="crew-fail load-failed">Queue ages unavailable: {crew.waits.error}</p>
    {:else if stalled === null}
      <p class="crew-fail load-failed">Queue ages unavailable.</p>
    {:else if stalled.length === 0}
      <p class="crew-stage-blank">
        Nothing claimed has sat longer than {STALE_AFTER_DAYS} day without moving.
      </p>
    {:else}
      <table class="crew-table">
        <thead>
          <tr><th>actor</th><th>step</th><th>packet</th><th>waiting</th></tr>
        </thead>
        <tbody>
          {#each stalled as w (w.stepId ?? w.jobId)}
            <tr>
              <td>{formatActor(w.assigneeId)}</td>
              <td>{w.stepTitle}</td>
              <td><a href={href(`/ux/jobs/${w.jobId}`)}>{w.jobTitle}</a></td>
              <td class="crew-num" class:crew-floor={!w.exact}>{waitText(w)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
      <p class="crew-note">
        Only steps somebody TOOK. An unassigned step that has waited a long time is a queue
        problem and belongs on the Marshalling Yard; this table is the crew's half. A wait shown
        as "at least" is a lower bound — the projection's stamp was a fallback, not an exact one.
      </p>
    {/if}
  {/if}
</div>

<style>
  .crew-root {
    padding: 0 1.25rem 3rem;
  }
  .crew-quiet {
    color: var(--muted, #6b675e);
    font-size: 0.85rem;
  }
  .crew-fail {
    color: var(--danger, #a3302a);
    font-size: 0.85rem;
    background: var(--danger-bg, #fbf0ef);
    border-left: 3px solid var(--danger, #a3302a);
    padding: 0.5rem 0.75rem;
    margin: 0.5rem 0;
  }
  .crew-section {
    font-size: 0.7rem;
    letter-spacing: 0.12em;
    color: var(--muted, #6b675e);
    border-bottom: 1px solid var(--border, #d5d2ca);
    padding-bottom: 0.3rem;
    margin: 1.75rem 0 0.75rem;
  }
  .crew-note {
    font-size: 0.74rem;
    line-height: 1.5;
    color: var(--muted, #6b675e);
    max-width: 62rem;
    margin: 0.75rem 0 0;
  }

  /* ---- the track ---- */
  .crew-track {
    display: grid;
    grid-template-columns: repeat(5, minmax(0, 1fr));
    gap: 0.5rem;
    overflow-x: auto;
  }
  .crew-stage {
    border: 1px solid var(--border, #d5d2ca);
    border-radius: 3px;
    padding: 0.4rem;
    min-width: 9rem;
  }
  .crew-stage-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    margin-bottom: 0.4rem;
  }
  .crew-stage-label {
    font-size: 0.66rem;
    letter-spacing: 0.1em;
    color: var(--muted, #6b675e);
  }
  .crew-stage-count {
    font-size: 0.95rem;
    font-variant-numeric: tabular-nums;
  }
  .crew-stage-blank {
    font-size: 0.72rem;
    color: var(--muted, #6b675e);
    font-style: italic;
    margin: 0;
  }
  .crew-car {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
    border-left: 3px solid var(--accent, #4a7c59);
    padding: 0.3rem 0.4rem;
    margin-bottom: 0.35rem;
    background: var(--surface-2, #f6f4ef);
  }
  .crew-car.troubled {
    border-left-color: var(--danger, #a3302a);
    background: var(--danger-bg, #fbf0ef);
  }
  .crew-car-branch {
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 0.68rem;
    overflow-wrap: anywhere;
  }
  .crew-car-title {
    font-size: 0.72rem;
  }
  .crew-car-detail {
    font-size: 0.67rem;
    color: var(--muted, #6b675e);
  }
  .crew-garage {
    margin-top: 0.75rem;
    max-width: 32rem;
  }
  .crew-garage-label {
    display: block;
    font-size: 0.66rem;
    letter-spacing: 0.1em;
    color: var(--danger, #a3302a);
    margin-bottom: 0.3rem;
  }

  /* ---- the crew ---- */
  .crew-cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(15rem, 1fr));
    gap: 0.6rem;
  }
  .crew-card {
    border: 1px solid var(--border, #d5d2ca);
    border-radius: 3px;
    padding: 0.55rem 0.65rem;
  }
  .crew-card-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 0.4rem;
  }
  .crew-card-name {
    font-size: 0.82rem;
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .crew-lane {
    font-size: 0.6rem;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    padding: 0.08rem 0.3rem;
    border-radius: 2px;
    white-space: nowrap;
    background: var(--surface-2, #f6f4ef);
    color: var(--muted, #6b675e);
  }
  .crew-lane-agent {
    background: #e8f0fb;
    color: #2a5a8f;
  }
  .crew-lane-human {
    background: #eef7ea;
    color: #2f6b3a;
  }
  .crew-lane-automation {
    background: #f4f0fa;
    color: #5d4a8f;
  }
  .crew-card-figs {
    display: flex;
    gap: 1.1rem;
    margin: 0.45rem 0 0.35rem;
  }
  .crew-fig {
    display: flex;
    align-items: baseline;
    gap: 0.3rem;
  }
  .crew-fig-n {
    font-size: 1.25rem;
    font-variant-numeric: tabular-nums;
    line-height: 1;
  }
  .crew-fig-l {
    font-size: 0.64rem;
    color: var(--muted, #6b675e);
    line-height: 1.2;
  }
  .crew-fig-l em {
    font-size: 0.6rem;
    opacity: 0.8;
  }
  .crew-card-last {
    font-size: 0.68rem;
    color: var(--muted, #6b675e);
  }
  .crew-unknown {
    font-style: italic;
  }
  .crew-when {
    color: var(--fg, #2b2823);
  }
  .crew-holding {
    list-style: none;
    margin: 0.4rem 0 0;
    padding: 0.4rem 0 0;
    border-top: 1px solid var(--border, #d5d2ca);
    font-size: 0.7rem;
  }
  .crew-holding li {
    margin-bottom: 0.2rem;
  }
  .crew-holding-meta {
    color: var(--muted, #6b675e);
    display: block;
    font-size: 0.65rem;
  }
  .crew-holding-more {
    color: var(--muted, #6b675e);
    font-style: italic;
  }

  /* ---- table ---- */
  .crew-table {
    border-collapse: collapse;
    width: 100%;
    max-width: 62rem;
    font-size: 0.76rem;
  }
  .crew-table th {
    text-align: left;
    font-size: 0.64rem;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted, #6b675e);
    border-bottom: 1px solid var(--border, #d5d2ca);
    padding: 0.25rem 0.5rem 0.25rem 0;
  }
  .crew-table td {
    padding: 0.3rem 0.5rem 0.3rem 0;
    border-bottom: 1px solid var(--border-subtle, #eceae4);
    vertical-align: top;
  }
  .crew-num {
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .crew-floor {
    font-style: italic;
    color: var(--muted, #6b675e);
  }
</style>
