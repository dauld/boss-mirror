<script lang="ts">
  // The production panel — what the floor produced today, from the
  // arrivals the page already computes (yard-production.ts). Tiles,
  // one bar an hour to scale, and the stops in force right now. The
  // page keeps no history, so the stops list is the alerts strip and
  // the dock's hold as they stand this poll — a stop that cleared is
  // gone, and the label says so rather than pretend to a day's log.
  import { landedBars, type Production } from './yard-production';
  import type { Alert } from './yard-alerts';
  import { clockText, journeyText } from './yard-status';
  import { sinceText } from './yard-floor';

  type Props = Readonly<{
    production: Production;
    alerts: readonly Alert[];
    /** The dock's hold, in the server's words, when it sends one. */
    dockHold: Readonly<{ text: string; since: string | null }> | null;
    nowMs: number;
    onselect: (key: string) => void;
  }>;
  let { production, alerts, dockHold, nowMs, onselect }: Props = $props();

  const chart = $derived(landedBars(production.perHour, new Date(nowMs).getUTCHours()));
  const ge = (windowed: boolean): string => (windowed ? '≥ ' : '');
  const median = $derived(production.medianJourneyS !== null ? journeyText(production.medianJourneyS) : '—');
  const sinceOf = (s: string | null): string => {
    if (!s) return '';
    const clock = clockText(s);
    return clock ?? sinceText(s, nowMs);
  };
</script>

<h2 class="head">
  Production · today
  <small class="mono">{production.day} UTC</small>
</h2>

<div class="tiles">
  <div class="tile" title={production.carsWindowed ? 'the arrivals window is full and every train in it landed today — there may be more' : 'cars landed today, from the arrivals window'}>
    <div class="n mono">{ge(production.carsWindowed)}{production.carsLanded}</div>
    <div class="l">cars landed</div>
  </div>
  <div class="tile" title={production.trainsWindowed ? 'the window is full — there may be more' : 'trains that arrived / were cancelled today'}>
    <div class="n mono">{ge(production.trainsWindowed)}{production.trainsArrived} <small>/</small> {production.trainsCancelled}</div>
    <div class="l">trains arrived / cancelled</div>
  </div>
  <div class="tile" title={production.journeySamples > 0 ? `median of ${production.journeySamples} recent trains' journey_seconds` : 'no journey stamps on the recent trains yet'}>
    <div class="n mono">{median}{#if production.journeySamples > 0}<small>n={production.journeySamples}</small>{/if}</div>
    <div class="l">median car lead time</div>
  </div>
  <div class="tile" title="merged and deployed, not yet proven in production">
    <div class="n mono">{production.awaitingProof}</div>
    <div class="l">awaiting proof</div>
  </div>
</div>

<svg class="chart" viewBox="0 0 {chart.w} {chart.h}" role="img" aria-label="cars landed per hour today, UTC">
  <line x1="0" y1={chart.baseline} x2={chart.w} y2={chart.baseline} class="axis" />
  {#each chart.bars as b (b.hour)}
    <g class="bar" class:current={b.current} class:empty={b.n === 0}>
      <title>{String(b.hour).padStart(2, '0')}:00 UTC · {b.n} car{b.n === 1 ? '' : 's'} landed</title>
      <rect x={b.x} y={b.y} width={b.w} height={b.h} rx="2" />
      <rect x={b.x} y={chart.baseline - 1} width={b.w} height="1" class="foot" />
      {#if b.n > 0}
        <text x={b.x + b.w / 2} y={b.y - 3} text-anchor="middle" class="value">{b.n}</text>
      {/if}
      {#if b.hour % 6 === 0}
        <text x={b.x + b.w / 2} y={chart.h - 4} text-anchor="middle" class="tick">{String(b.hour).padStart(2, '0')}</text>
      {/if}
    </g>
  {/each}
  {#if chart.max === 0}
    <text x={chart.w / 2} y={chart.baseline - 8} text-anchor="middle" class="tick">nothing landed yet today</text>
  {/if}
</svg>

<div class="label">Stops right now · no history — a cleared stop is gone</div>
<div class="stops">
  {#each alerts as a (a.id)}
    <button type="button" class="stop {a.sev}" onclick={() => onselect(a.subject)}>
      <span class="t mono">{sinceOf(a.since)}</span>
      <span>{a.text}</span>
    </button>
  {/each}
  {#if dockHold}
    <button type="button" class="stop hold" onclick={() => onselect('dock')}>
      <span class="t mono">{sinceOf(dockHold.since)}</span>
      <span>dock held: {dockHold.text}</span>
    </button>
  {/if}
  {#if alerts.length === 0 && !dockHold}
    <span class="empty">no stops — every machine is working or idle by design</span>
  {/if}
</div>

<style>
  .head {
    margin: 0;
    font-size: 11px;
    letter-spacing: var(--ls-label, 0.1em);
    text-transform: uppercase;
    color: var(--static, #7a838c);
    font-weight: 600;
    display: flex;
    justify-content: space-between;
    gap: var(--s3, 12px);
  }
  .head small { font-weight: 500; color: var(--text-faint, #5c656e); }
  .mono { font-family: var(--font-mono, ui-monospace, monospace); font-variant-numeric: tabular-nums; }
  .tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(120px, 1fr)); gap: var(--s2, 8px); margin-top: var(--s3, 12px); }
  .tile { border: 1px solid var(--hairline, #2a3138); padding: var(--s2, 8px) var(--s3, 12px); background: var(--void, #0d1014); min-width: 0; }
  .tile .n { font-size: 22px; font-weight: 600; line-height: 1.1; overflow-wrap: anywhere; }
  .tile .n small { font-size: 11px; color: var(--static, #7a838c); font-weight: 500; }
  .tile .l { font-size: 11px; color: var(--static, #7a838c); letter-spacing: var(--ls-label, 0.1em); text-transform: uppercase; margin-top: 2px; }

  /* The chart: one series, one hue; text in text tokens; the axis a
     hairline; the current hour's column brighter so the eye finds
     "now". */
  .chart { width: 100%; height: auto; display: block; margin: var(--s3, 12px) 0; font-family: var(--font-mono, ui-monospace, monospace); }
  .axis { stroke: var(--hairline, #2a3138); stroke-width: 1; }
  .bar rect { fill: var(--signal, #5fd4a8); opacity: 0.75; }
  .bar.current rect { opacity: 1; }
  .bar rect.foot { fill: var(--border-strong, #3a434d); opacity: 1; }
  .bar.current rect.foot { fill: var(--signal, #5fd4a8); }
  .bar.empty rect.foot { fill: var(--hairline, #2a3138); }
  .bar.current.empty rect.foot { fill: var(--signal, #5fd4a8); }
  .bar:hover rect { opacity: 1; }
  .value { fill: var(--fog, #e8ecef); font-size: 9px; }
  .tick { fill: var(--static, #7a838c); font-size: 9px; }

  .label { font-size: 11px; letter-spacing: var(--ls-label, 0.1em); text-transform: uppercase; color: var(--static, #7a838c); font-weight: 600; margin-top: var(--s3, 12px); }
  .stops { display: grid; gap: 3px; margin-top: 6px; }
  .stop {
    display: grid; grid-template-columns: 76px 1fr; gap: var(--s2, 8px); align-items: baseline;
    background: transparent; border: 0; padding: 2px 0; font: inherit; font-size: 12.5px; color: inherit;
    text-align: left; cursor: pointer; border-radius: 0;
  }
  .stop:hover, .stop:focus-visible { color: var(--signal, #5fd4a8); }
  .stop .t { color: var(--static, #7a838c); font-size: 11px; }
  .stop.err span:last-child { color: var(--err, #e2685c); }
  .stop.warn span:last-child { color: var(--warn, #d9a441); }
  .stop.hold span:last-child { color: var(--static, #7a838c); }
  .empty { color: var(--text-faint, #5c656e); font-size: 12px; }
</style>
