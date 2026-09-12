<script lang="ts">
  // The Receiving Yard — the INBOUND third of the operator surface,
  // upstream of the Marshalling Yard (what is queued where) and the
  // Crew Board (who is building what).
  //
  // David, 2026-09-12, approving the prototype this ports: "each
  // department will have a view of jobs flowing in, jobs getting worked
  // within the department, and jobs flowing out" — this is IT's IN. It
  // keeps the floor's grammar: a packet is a car, a channel is a track,
  // the car carries its days waiting and a mark for who holds it, and
  // the same car is meant to be recognisable when it reaches the Crew
  // Board and then the Train Yard.
  //
  // WHAT IS SHOWN, in reading order: the week's flow (in, out, standing);
  // what the snapshot says — three readings that were the reason the
  // prototype existed; arrivals per day by channel; the inbound tracks;
  // then the manifest, every standing packet oldest first, each a link
  // to the packet itself.
  //
  // NO NUMBER THIS SURFACE MAKES UP. Kinds come from the registry, not a
  // list here; a channel is a recorded fact where the packet carries
  // one and a derived reading (said so) otherwise; a kind whose page
  // truncated is named; a failed read is a failure, never a clear track.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { href, navigate } from '../../router';
  import type { Remote } from '../../data/remote';
  import {
    AGE_THRESHOLDS,
    CHANNELS,
    CHANNEL_LABEL,
    arrivalsByDay,
    daysEndingOn,
    inboundKinds,
    loadKind,
    loadWorkflows,
    readings,
    waiting,
    type Channel,
    type InboundRow,
    type WaitingRow,
  } from './receiving';

  /** The window: a week of days, today included. Departures are read
   *  from the same rows, so a packet that closed inside the window
   *  counts as having left even if it arrived before it. */
  const WINDOW_DAYS = 8;
  /** One page per kind. The busiest kind (backlog-item) ran 270 in a
   *  week when this was written; a kind past this is reported, not
   *  silently cut (a-limit-is-not-a-filter). */
  const PAGE = 500;

  let today = $state<string>(new Date().toISOString().slice(0, 10));
  let registry = $state<Remote<ReadonlyArray<string>>>({ kind: 'loading' });
  let rows = $state<Remote<ReadonlyArray<InboundRow>>>({ kind: 'loading' });
  let truncated = $state<ReadonlyArray<{ kind: string; total: number }>>([]);

  async function refresh(): Promise<void> {
    today = new Date().toISOString().slice(0, 10);
    const wf = await loadWorkflows();
    if (wf.kind === 'failed') {
      registry = wf;
      return;
    }
    const kinds = inboundKinds(wf.data);
    registry = { kind: 'ready', data: kinds };
    // One read per kind, together: each is small, and the registry
    // says how many there are.
    const pages = await Promise.all(kinds.map((k) => loadKind(k, WINDOW_DAYS, PAGE)));
    const failed = pages.find((p) => p.kind === 'failed');
    if (failed && failed.kind === 'failed') {
      rows = failed;
      return;
    }
    const ready = pages.flatMap((p) => (p.kind === 'ready' ? [p.data] : []));
    truncated = kinds.flatMap((kind, i) => {
      const p = ready[i];
      return p && p.total > p.rows.length ? [{ kind, total: p.total }] : [];
    });
    rows = { kind: 'ready', data: ready.flatMap((p) => p.rows) };
  }

  onMount(() => {
    void refresh();
    // Arrivals are counted by day; a minute is plenty.
    const t = setInterval(() => void refresh(), 60_000);
    return () => clearInterval(t);
  });

  const days = $derived(daysEndingOn(today, WINDOW_DAYS));
  const all = $derived<ReadonlyArray<InboundRow>>(rows.kind === 'ready' ? rows.data : []);
  const flow = $derived(arrivalsByDay(all, days));
  const standing = $derived<ReadonlyArray<WaitingRow>>(waiting(all, today));
  const said = $derived(readings(all, today, days));
  const arrived = $derived(flow.reduce((n, d) => n + d.arrived, 0));
  const left = $derived(flow.reduce((n, d) => n + d.left, 0));
  const past3 = $derived(standing.filter((r) => r.band !== 'fresh').length);
  const past14 = $derived(standing.filter((r) => r.band === 'stale').length);
  const held = $derived({
    agent: standing.filter((r) => r.holder.who === 'agent').length,
    human: standing.filter((r) => r.holder.who === 'human').length,
    nobody: standing.filter((r) => r.holder.who === 'nobody').length,
  });
  const derivedShare = $derived(all.filter((r) => r.channelBasis === 'derived').length);

  // The chart: one scale for every bar, ticks at round numbers the
  // tallest day reaches, departures as a dashed level on each day.
  const CH_W = 640;
  const CH_H = 200;
  const PAD_L = 36;
  const PAD_B = 28;
  const BAR_W = 52;
  const gapX = $derived((CH_W - PAD_L - WINDOW_DAYS * BAR_W) / (WINDOW_DAYS + 1));
  const scaleMax = $derived.by(() => {
    const m = Math.max(1, ...flow.map((d) => Math.max(d.arrived, d.left)));
    const step = m <= 20 ? 5 : m <= 50 ? 10 : m <= 100 ? 25 : 50;
    return Math.ceil(m / step) * step;
  });
  const ticks = $derived([0, 1, 2, 3, 4].map((i) => (i * scaleMax) / 4));
  const yOf = (v: number): number => CH_H - PAD_B - (v / scaleMax) * (CH_H - PAD_B - 24);
  const xOf = (i: number): number => PAD_L + gapX + i * (BAR_W + gapX);

  type Segment = Readonly<{ channel: Channel; n: number; y: number; h: number }>;
  function segments(byChannel: Readonly<Partial<Record<Channel, number>>>): ReadonlyArray<Segment> {
    return CHANNELS.reduce<{ y: number; out: Segment[] }>(
      (acc, c) => {
        const n = byChannel[c] ?? 0;
        if (n === 0) return acc;
        const h = yOf(0) - yOf(n);
        const y = acc.y - h;
        return { y, out: [...acc.out, { channel: c, n, y, h }] };
      },
      { y: yOf(0), out: [] },
    ).out;
  }

  const byChannel = $derived(
    CHANNELS.map((c) => ({
      channel: c,
      arrived: flow.reduce((n, d) => n + (d.byChannel[c] ?? 0), 0),
      cars: standing.filter((r) => r.channel === c),
    })),
  );

  const shortLabel = (c: Channel): string => CHANNEL_LABEL[c].split(' ·')[0] ?? c;
  const openPacket = (jobId: string) => navigate(`/jobs/${jobId}`);
</script>

<div class="ry-root">
  <PageHeader
    eyebrow="IT · Inbound · Before anyone builds it"
    title="Receiving Yard"
    subtitle="What came in, and what is still standing on the inbound track — every packet that asks the platform for something, from the moment it is opened until an actor takes it into a build, a review or a decision."
  />

  {#if registry.kind === 'failed'}
    <p class="ry-fail load-failed">
      The workflow registry did not answer: {registry.error}. Which kinds are inbound is read
      from the registry, so without it this page shows nothing rather than a guess.
    </p>
  {:else if rows.kind === 'failed'}
    <p class="ry-fail load-failed">
      A packet read did not answer: {rows.error}. An unreachable read is not an empty
      track, so nothing is drawn.
    </p>
  {:else if registry.kind === 'loading' || rows.kind === 'loading'}
    <p class="ry-quiet">Reading the inbound track…</p>
  {:else}
    {#if truncated.length > 0}
      <p class="ry-fail load-failed">
        {#each truncated as t (t.kind)}
          <span class="mono">{t.kind}</span> has {t.total} packets in the window and only {PAGE}
          were read;
        {/each}
        the counts below are floors for those kinds.
      </p>
    {/if}

    <div class="ry-strip">
      <div><div class="k">Arrived · {WINDOW_DAYS} days</div><div class="v">{arrived}</div></div>
      <div><div class="k">Left · closed</div><div class="v">{left}</div></div>
      <div><div class="k">Standing now</div><div class="v warn">{standing.length}</div></div>
      <div>
        <div class="k">Past {AGE_THRESHOLDS.aging} days</div>
        <div class="v warn">{past3}<small>of {standing.length}</small></div>
      </div>
      <div><div class="k">Past {AGE_THRESHOLDS.stale} days</div><div class="v err">{past14}</div></div>
      <div>
        <div class="k">Held by</div>
        <div class="v">
          {held.agent}<small>agent</small> · {held.human}<small>people</small> ·
          {held.nobody}<small>nobody</small>
        </div>
      </div>
    </div>

    <div class="ry-section">00 — WHAT THIS SNAPSHOT SAYS</div>
    <div class="ry-findings">
      <div class="ry-finding" class:err={said.feedbackStanding.oldestDays > AGE_THRESHOLDS.stale}
        class:ok={said.feedbackStanding.count === 0}>
        <b>
          {#if said.feedbackStanding.count === 0}
            No feedback packet is standing.
          {:else}
            {said.feedbackStanding.count} feedback packet{said.feedbackStanding.count === 1 ? '' : 's'} standing,
            the oldest {said.feedbackStanding.oldestDays} days.
          {/if}
        </b>
        <span class="why">Feedback is the one channel where a person wrote the packet.</span>
      </div>
      <div class="ry-finding" class:ok={said.onOneActor.of === 0}>
        <b>{said.onOneActor.count} of {said.onOneActor.of} standing packets are on one actor — {said.onOneActor.actor}.</b>
        <span class="why">
          A packet assigned at filing and a packet being built look the same here; the Crew
          Board's build rows are the other half of this reading.
        </span>
      </div>
      <div class="ry-finding" class:ok={said.unrecorded.count === 0}>
        <b>{said.unrecorded.count} of {said.unrecorded.of} arrivals record no channel.</b>
        <span class="why">
          {derivedShare} of {all.length} channels on this page are derived from the kind or the
          reporter rather than read off the packet. A <span class="mono">channel</span> key at
          filing makes this row a fact.
        </span>
      </div>
    </div>

    <div class="ry-section">01 — ARRIVALS PER DAY, BY CHANNEL</div>
    <div class="ry-panel">
      <svg viewBox="0 0 {CH_W} {CH_H}" class="ry-chart" role="img" aria-label="inbound packets per day">
        {#each ticks as v (v)}
          <line x1={PAD_L} x2={CH_W} y1={yOf(v)} y2={yOf(v)} class="grid" />
          <text x={PAD_L - 6} y={yOf(v) + 4} text-anchor="end" class="cl">{v}</text>
        {/each}
        {#each flow as d, i (d.day)}
          {#each segments(d.byChannel) as s (s.channel)}
            <rect x={xOf(i)} y={s.y} width={BAR_W} height={s.h} class="bar {s.channel}">
              <title>{d.day} · {s.n} {shortLabel(s.channel)}</title>
            </rect>
          {/each}
          <text x={xOf(i) + BAR_W / 2} y={yOf(d.arrived) - 5} text-anchor="middle" class="cv">{d.arrived}</text>
          <text x={xOf(i) + BAR_W / 2} y={CH_H - 8} text-anchor="middle" class="cl">{d.day.slice(5).replace('-', '/')}</text>
          <line x1={xOf(i) - 3} x2={xOf(i) + BAR_W + 3} y1={yOf(d.left)} y2={yOf(d.left)} class="left">
            <title>{d.day} · {d.left} left</title>
          </line>
        {/each}
      </svg>
      <div class="ry-legend">
        {#each CHANNELS as c (c)}
          <span><i class={c}></i>{CHANNEL_LABEL[c]}</span>
        {/each}
        <span><span class="dash"></span>left (closed that day)</span>
      </div>
    </div>

    <div class="ry-section">02 — INBOUND TRACKS, WHAT IS STANDING</div>
    <div class="ry-tracks">
      {#each byChannel as t (t.channel)}
        <div class="ry-track">
          <div class="tk-name"><i class={t.channel}></i>{CHANNEL_LABEL[t.channel]}</div>
          <div class="tk-num">{t.arrived} in · {t.cars.length} standing</div>
          <div class="tk-cars">
            {#if t.cars.length === 0}
              <span class="ry-empty">nothing standing</span>
            {:else}
              {#each t.cars as r (r.id)}
                <a
                  class="car {r.band} h-{r.holder.who}"
                  href={href(`/jobs/${r.id}`)}
                  title="{r.title} — {r.age} d, on {r.holder.label}"
                  onclick={(e) => {
                    e.preventDefault();
                    openPacket(r.id);
                  }}><span>{r.age}</span></a
                >
              {/each}
            {/if}
          </div>
        </div>
      {/each}
    </div>
    <div class="ry-key">
      <span class="fresh">■ ≤ {AGE_THRESHOLDS.aging} d</span>
      <span class="aging">■ {AGE_THRESHOLDS.aging + 1}–{AGE_THRESHOLDS.stale} d</span>
      <span class="stale">■ &gt; {AGE_THRESHOLDS.stale} d</span>
      <span>a dot marks a car on a person · a dashed outline, a car on nobody</span>
    </div>

    <div class="ry-section">03 — THE MANIFEST, OLDEST FIRST</div>
    {#if standing.length === 0}
      <p class="ry-quiet">The inbound track is clear.</p>
    {:else}
      <div class="ry-tbl">
        <table class="ry-table">
          <thead>
            <tr>
              <th class="num">Waiting</th>
              <th>Channel</th>
              <th>Kind</th>
              <th>Packet</th>
              <th>Ready step</th>
              <th>Held by</th>
            </tr>
          </thead>
          <tbody>
            {#each standing as r (r.id)}
              <tr>
                <td class="num {r.band}">{r.age} d</td>
                <td>
                  <span class="pill {r.channel}" title={r.channelBasis === 'recorded' ? 'recorded on the packet' : 'derived from the kind or reporter'}>
                    {shortLabel(r.channel)}{#if r.channelBasis === 'derived'}<span class="dim"> ?</span>{/if}
                  </span>
                </td>
                <td class="mono dim">{r.kind}</td>
                <td class="ttl">
                  <a
                    href={href(`/jobs/${r.id}`)}
                    onclick={(e) => {
                      e.preventDefault();
                      openPacket(r.id);
                    }}>{r.title}</a
                  >
                  <span class="id">{r.id.slice(0, 8)}{#if r.priority === 'urgent'} · urgent{/if}</span>
                </td>
                <td class="mono dim">{r.ready.map((s) => s.kind).join(', ') || '—'}</td>
                <td class="hold h-{r.holder.who}">{r.holder.label}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}

    <p class="ry-footnote">
      Inbound kinds are the registry's active platform kinds that are neither chores
      (<span class="mono">maintenance-*</span>) nor the delivery pipeline
      ({registry.data.length} kinds this read); each is read with
      <span class="mono">simulated=false&amp;closed_within={WINDOW_DAYS}</span>, so every open packet
      is here and departures are counted from <span class="mono">closed_on</span>. Ages count from
      <span class="mono">opened_on</span>. Thresholds are the Marshalling Yard's proposal until the
      packet carries its own. A channel marked <span class="mono">?</span> was derived, not recorded.
    </p>
  {/if}
</div>

<style>
  .ry-root { padding: 0 32px 32px; }
  .ry-section {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px; letter-spacing: var(--ls-eyebrow, 0.3em);
    color: var(--signal, #5fd4a8); margin: 28px 0 8px;
    display: flex; align-items: center; gap: 12px;
  }
  .ry-section::after { content: ''; flex: 1; border-top: 1px solid var(--hairline, #2a3138); }
  .ry-quiet { color: var(--static, #7a838c); font-size: 13px; }
  .ry-fail {
    color: var(--warn, #d9a441); border: 1px solid var(--warn, #d9a441);
    padding: 8px 12px; font-size: 13px;
  }
  .mono { font-family: var(--font-mono, ui-monospace, monospace); }
  .dim { color: var(--static, #7a838c); }

  .ry-strip {
    display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    border: 1px solid var(--hairline, #2a3138); margin-top: 16px;
  }
  .ry-strip > div { padding: 12px 16px; border-right: 1px solid var(--hairline, #2a3138); }
  .ry-strip > div:last-child { border-right: 0; }
  .ry-strip .k {
    font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: 0.1em; text-transform: uppercase; color: var(--static, #7a838c);
  }
  .ry-strip .v { font-size: 28px; font-weight: 500; font-variant-numeric: tabular-nums; line-height: 1.1; margin-top: 4px; }
  .ry-strip .v small { font-size: 12px; color: var(--static, #7a838c); font-weight: 400; margin-left: 5px; }
  .ry-strip .v.warn { color: var(--warn, #d9a441); }
  .ry-strip .v.err { color: var(--err, #e2685c); }

  .ry-findings { display: grid; gap: 10px; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr)); }
  .ry-finding { border-left: 3px solid var(--warn, #d9a441); padding: 2px 0 2px 14px; font-size: 13px; }
  .ry-finding.err { border-left-color: var(--err, #e2685c); }
  .ry-finding.ok { border-left-color: var(--ok, #4fb98a); }
  .ry-finding b { font-weight: 500; }
  .ry-finding .why { display: block; color: var(--static, #7a838c); margin-top: 3px; }

  .ry-panel { border: 1px solid var(--hairline, #2a3138); padding: 14px 16px; }
  .ry-chart { width: 100%; height: auto; display: block; }
  .ry-chart .grid { stroke: var(--hairline, #2a3138); stroke-width: 1; }
  .ry-chart .cl { fill: var(--static, #7a838c); font: 11px var(--font-mono, ui-monospace, monospace); }
  .ry-chart .cv { fill: var(--fog, #e8ecef); font: 11px var(--font-mono, ui-monospace, monospace); }
  .ry-chart .left { stroke: var(--fog, #e8ecef); stroke-width: 1.5; stroke-dasharray: 3 2; }
  .ry-legend {
    display: flex; gap: 16px; flex-wrap: wrap; margin-top: 8px;
    font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px; color: var(--static, #7a838c);
  }
  .ry-legend i, .tk-name i { display: inline-block; width: 10px; height: 10px; margin-right: 6px; vertical-align: -1px; }
  .ry-legend .dash { display: inline-block; width: 18px; border-top: 1.5px dashed var(--fog, #e8ecef); vertical-align: 3px; margin-right: 6px; }

  /* One hue per channel, used by the bars, the track marks and the pills. */
  .bar.feedback, i.feedback { fill: var(--signal, #5fd4a8); background: var(--signal, #5fd4a8); }
  .bar.monitoring, i.monitoring { fill: var(--err, #e2685c); background: var(--err, #e2685c); }
  .bar.session, i.session { fill: var(--brew-amber-soft, #e8c37a); background: var(--brew-amber-soft, #e8c37a); }
  .bar.design, i.design { fill: var(--warn, #d9a441); background: var(--warn, #d9a441); }
  .bar.protocol, i.protocol { fill: var(--border-strong, #3a434d); background: var(--border-strong, #3a434d); }
  .bar.unrecorded, i.unrecorded { fill: var(--text-faint, #5c656e); background: var(--text-faint, #5c656e); }
  .pill { display: inline-block; border: 1px solid var(--hairline, #2a3138); padding: 1px 7px; font: 11px var(--font-mono, ui-monospace, monospace); white-space: nowrap; }
  .pill.feedback { border-color: var(--signal, #5fd4a8); }
  .pill.monitoring { border-color: var(--err, #e2685c); }
  .pill.design { border-color: var(--warn, #d9a441); }

  .ry-tracks { border: 1px solid var(--hairline, #2a3138); }
  .ry-track {
    display: grid; grid-template-columns: 280px 130px 1fr; gap: 12px; align-items: center;
    padding: 10px 16px; border-bottom: 1px solid var(--hairline, #2a3138);
  }
  .ry-track:last-child { border-bottom: 0; }
  .tk-name { font-size: 13px; }
  .tk-num { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px; color: var(--static, #7a838c); white-space: nowrap; }
  .tk-cars { display: flex; gap: 4px; flex-wrap: wrap; min-height: 22px; align-items: center; }
  .ry-empty { color: var(--text-faint, #5c656e); font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px; }
  .car {
    display: inline-grid; place-items: center; width: 30px; height: 22px; position: relative;
    font: 11px var(--font-mono, ui-monospace, monospace); text-decoration: none;
    color: var(--void, #0d1014); background: var(--ok, #4fb98a);
  }
  .car.aging { background: var(--warn, #d9a441); }
  .car.stale { background: var(--err, #e2685c); }
  .car.h-human::after { content: ''; position: absolute; right: 2px; top: 2px; width: 5px; height: 5px; border-radius: 50%; background: var(--void, #0d1014); }
  .car.h-nobody { background: transparent; color: var(--fog, #e8ecef); outline: 1px dashed var(--static, #7a838c); outline-offset: -1px; }
  .car.h-nobody.aging { color: var(--warn, #d9a441); outline-color: var(--warn, #d9a441); }
  .car.h-nobody.stale { color: var(--err, #e2685c); outline-color: var(--err, #e2685c); }
  .car:focus-visible { outline: 2px solid var(--signal, #5fd4a8); outline-offset: 1px; }
  .ry-key { display: flex; gap: 18px; flex-wrap: wrap; margin-top: 8px; font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px; color: var(--static, #7a838c); }
  .ry-key .fresh { color: var(--ok, #4fb98a); }
  .ry-key .aging { color: var(--warn, #d9a441); }
  .ry-key .stale { color: var(--err, #e2685c); }

  .ry-tbl { overflow-x: auto; }
  .ry-table { width: 100%; border-collapse: collapse; font-size: 13px; min-width: 760px; }
  .ry-table th {
    text-align: left; font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase;
    color: var(--static, #7a838c); font-weight: 400;
    border-bottom: 1px solid var(--hairline, #2a3138); padding: 4px 12px 4px 0;
  }
  .ry-table td { padding: 7px 12px 7px 0; border-bottom: 1px solid var(--hairline, #2a3138); vertical-align: top; }
  .ry-table th.num, .ry-table td.num { text-align: right; font-family: var(--font-mono, ui-monospace, monospace); font-variant-numeric: tabular-nums; white-space: nowrap; }
  td.num.fresh { color: var(--static, #7a838c); }
  td.num.aging { color: var(--warn, #d9a441); }
  td.num.stale { color: var(--err, #e2685c); }
  td.ttl { max-width: 520px; }
  td .id { display: block; font: 11px var(--font-mono, ui-monospace, monospace); color: var(--text-faint, #5c656e); margin-top: 2px; }
  td.hold { white-space: nowrap; }
  td.hold.h-human { color: var(--signal, #5fd4a8); }
  td.hold.h-nobody { color: var(--warn, #d9a441); }
  .ry-footnote { color: var(--text-faint, #5c656e); font-size: 12px; max-width: 80ch; margin-top: 20px; }
  @media (max-width: 720px) { .ry-track { grid-template-columns: 1fr; } }
</style>
