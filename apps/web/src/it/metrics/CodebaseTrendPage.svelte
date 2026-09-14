<script lang="ts">
  // /it/design/codebase — the codebase trend, read off the daily
  // `maintenance-codebase-metrics` packets (backlog 06048ade).
  //
  // David, 2026-09-11: "once we have code base analysis statistics we
  // can start to get a sense for whether we are getting simpler and
  // more reliable or more complex as we go." The packet is the record
  // and this page reads it: the measuring — what a bucket is, why the
  // series' prod/test split is a bound and the snapshot's is exact,
  // how a code branch on kind is counted — is documented at the top
  // of infra/codebase-metrics.sh and is not restated here.
  //
  // READING ORDER: the reading in words (computed, not written); the
  // two headline ratios the founding ideas commit to — delete:add per
  // landing and registry rows to code branches on kind — with WHICH
  // tree they describe (head, when it landed, when it was measured,
  // so a stale converge and a quiet week look different); the series,
  // net lines per day and per landing; then the context the row
  // carries. Everything is a function of the packet fields (trend.ts,
  // tested); a null in the row stays a null with the row's reason.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { href, navigate } from '../../router';
  import type { Remote } from '../../data/remote';
  import {
    barGeometry,
    deleteAddPct,
    headGapDays,
    lastDays,
    loadMetricsPackets,
    mergeLandings,
    newestMeasured,
    perDay,
    reading,
    type MetricsPage,
    type Verdict,
  } from './trend';

  /** Packets read per load: one per day once the cadence has run a
   *  while; the page says when `total` is past this. */
  const PAGE = 120;
  /** The reading's window: a week of landings ending on the newest. */
  const READ_DAYS = 7;
  /** How many landings the per-train table shows, newest first. */
  const TRAINS = 40;

  let page = $state<Remote<MetricsPage>>({ kind: 'loading' });
  let chartDays = $state<number | null>(30);

  async function refresh(): Promise<void> {
    page = await loadMetricsPackets(PAGE);
  }
  onMount(() => {
    void refresh();
  });

  const ready = $derived(page.kind === 'ready' ? page.data : null);
  const newest = $derived(ready ? newestMeasured(ready.packets) : null);
  const landings = $derived(ready ? mergeLandings(ready.packets) : []);
  const days = $derived(perDay(landings));
  const read = $derived(newest && ready ? reading(newest, landings, READ_DAYS, ready.packets) : null);
  const seriesPct = $derived(
    deleteAddPct(
      landings.reduce((s, l) => s + l.add, 0),
      landings.reduce((s, l) => s + l.del, 0),
    ),
  );
  const gap = $derived(newest ? headGapDays(newest.measured.head_at, newest.measured.at) : null);

  const CH_W = 720;
  const CH_H = 160;
  const chartRows = $derived(lastDays(days, chartDays));
  const chart = $derived(barGeometry(chartRows, CH_W, CH_H));
  const trains = $derived(landings.slice(0, TRAINS));

  const short = (sha: string): string => sha.slice(0, 8);
  const when = (iso: string | null): string => (iso ? `${iso.slice(0, 10)} ${iso.slice(11, 16)}Z` : '—');
  const n = (v: number): string => v.toLocaleString('en-US');
  const signed = (v: number): string => (v > 0 ? `+${n(v)}` : n(v));
  const pct = (v: number | null): string => (v === null ? 'n/a' : `${v}%`);
  const tone = (v: Verdict): string =>
    v === 'simpler' ? 'ok' : v === 'more complex' ? 'warn' : v === 'not counted' ? 'err' : '';
  /** Which buckets are production and which are test, for the prod/test
   *  lines. The names are the script's; a bucket it adds later lands in
   *  "other" here rather than vanishing. */
  const PROD = new Set(['rust_prod', 'web_prod']);
  const TEST = new Set(['rust_test', 'rust_test_inline', 'web_test']);
  const bucketSum = (m: Readonly<Record<string, { add: number; del: number }>>, pick: (k: string) => boolean) =>
    Object.entries(m)
      .filter(([k]) => pick(k))
      .reduce((s, [, v]) => ({ add: s.add + v.add, del: s.del + v.del }), { add: 0, del: 0 });
  const windowProd = $derived(newest ? bucketSum(newest.measured.window.by_bucket, (k) => PROD.has(k)) : null);
  const windowTest = $derived(newest ? bucketSum(newest.measured.window.by_bucket, (k) => TEST.has(k)) : null);
  const buckets = $derived(
    newest?.measured.totals
      ? Object.entries(newest.measured.totals.by_bucket).sort(([, a], [, b]) => b - a)
      : [],
  );
  const registries = $derived(
    newest?.measured.registry ? Object.entries(newest.measured.registry.by_registry).sort(([, a], [, b]) => b - a) : [],
  );
  const classes = $derived(
    newest?.measured.registry
      ? Object.entries(newest.measured.registry.code_branches_by_class).sort(([, a], [, b]) => b - a)
      : [],
  );
</script>

<div class="ct-root">
  <PageHeader
    eyebrow="IT · Design · The codebase states its own trend"
    title="Codebase trend"
    subtitle="Are we getting simpler or more complex as we go? Read off the daily codebase-metrics packets — every landing on main with its adds and deletes, and the registry against the code branches it was meant to retire. Two ratios are the headline; everything else is context."
  />

  {#if page.kind === 'failed'}
    <p class="ct-fail load-failed">
      The metrics packets did not answer: {page.error}. An unreachable read is not an empty series, so
      nothing is drawn.
    </p>
  {:else if page.kind === 'loading'}
    <p class="ct-quiet">Reading the measured packets…</p>
  {:else if !newest || !read || !ready}
    <p class="ct-fail load-failed">
      {ready?.total ?? 0} codebase-metrics packet{ready?.total === 1 ? '' : 's'} exist and none carries a
      measurement{ready && ready.unmeasured > 0 ? ` (${ready.unmeasured} failed run${ready.unmeasured === 1 ? '' : 's'})` : ''}.
      The cadence has not measured yet; this page has nothing it can honestly draw.
    </p>
  {:else}
    <div class="ct-section">00 — THE READING · computed from the numbers below</div>
    <div class="ct-findings">
      <div class="ct-finding {tone(read.volume.verdict)}">
        <div class="k">By volume · last {READ_DAYS} days of landings</div>
        <div class="v">{read.volume.verdict}</div>
        <div class="t">{read.volume.text}</div>
      </div>
      <div class="ct-finding {tone(read.ratio.verdict)}">
        <div class="k">
          By registry ratio · {read.ratio.since ? `since ${read.ratio.since.at.slice(0, 10)}` : 'one measurement so far'}
        </div>
        <div class="v">{read.ratio.verdict}</div>
        <div class="t">{read.ratio.text}</div>
      </div>
    </div>

    <div class="ct-strip">
      <div title="deletes as a share of adds over the last {READ_DAYS} days of landings — 100% would mean the tree held its size">
        <div class="k">delete:add · {READ_DAYS} days</div>
        <div class="v {read.volume.window.deleteAddPct !== null && read.volume.window.deleteAddPct >= 100 ? 'ok' : 'warn'}">
          {pct(read.volume.window.deleteAddPct)}
        </div>
      </div>
      <div title="deletes as a share of adds over every landing the packets carry">
        <div class="k">delete:add · whole series</div>
        <div class="v">{pct(seriesPct)}<small>{n(landings.length)} landings</small></div>
      </div>
      <div title="registry rows (dispatcher rules, workflow rows, step types, step plugins) to match-on-kind sites in crates/core">
        <div class="k">registry rows : code branches</div>
        <div class="v">
          {n(read.ratio.rows)} <small>:</small>
          {read.ratio.branches === null ? 'n/a' : n(read.ratio.branches)}
          {#if read.ratio.rowsPerBranch !== null}<small>{read.ratio.rowsPerBranch}×</small>{/if}
        </div>
      </div>
      {#if windowProd && windowTest}
        <div title="the packet's own window, by path bucket: prod is an upper bound and test a lower one (inline #[cfg(test)] is attributed by path in the series)">
          <div class="k">prod · test lines in window</div>
          <div class="v small">
            <span class="mono">{signed(windowProd.add - windowProd.del)}</span>
            <small>prod {pct(deleteAddPct(windowProd.add, windowProd.del))}</small>
            <br />
            <span class="mono">{signed(windowTest.add - windowTest.del)}</span>
            <small>test {pct(deleteAddPct(windowTest.add, windowTest.del))}</small>
          </div>
        </div>
      {/if}
      <div title="the tree this row describes: its head commit, when that head landed, and when it was measured">
        <div class="k">describes</div>
        <div class="v small">
          <span class="mono">{short(newest.measured.head)}</span>
          <small>{newest.measured.ref ?? ''}</small>
          <br />
          <small>landed {when(newest.measured.head_at)} · measured {when(newest.measured.at)}</small>
          {#if gap !== null}
            <br />
            <small class={gap > 2 ? 'warn' : ''}>
              head is {gap} day{gap === 1 ? '' : 's'} older than its measurement
              {gap > 2 ? ' — a stale converge or a quiet stretch; the yard says which' : ''}
            </small>
          {/if}
        </div>
      </div>
    </div>

    <div class="ct-section">01 — NET LINES PER DAY · adds minus deletes, every landing on main</div>
    <div class="ct-panel">
      <div class="ct-controls">
        {#each [14, 30, 90, null] as d (d ?? 'all')}
          <button type="button" class:on={chartDays === d} onclick={() => (chartDays = d)}>
            {d === null ? 'whole series' : `${d} days`}
          </button>
        {/each}
        <span class="dim">
          {chartRows.length} day{chartRows.length === 1 ? '' : 's'} with landings · tallest {n(chart.max)} lines
        </span>
      </div>
      <svg class="ct-chart" viewBox="0 0 {CH_W} {CH_H}" role="img" aria-label="net lines per day">
        <line x1="0" y1={chart.baseline} x2={CH_W} y2={chart.baseline} class="axis" />
        {#each chart.bars as b (b.day)}
          <g class="bar" class:down={b.net < 0}>
            <title>
              {b.day} · {b.row.landings} landing{b.row.landings === 1 ? '' : 's'} · +{n(b.row.add)} / -{n(b.row.del)} ·
              net {signed(b.net)} · delete:add {pct(b.row.deleteAddPct)} · tests +{n(b.row.testAdd)} / -{n(b.row.testDel)}
            </title>
            <rect x={b.x} y={b.y} width={b.w} height={Math.max(b.h, 0.5)} rx="1" />
          </g>
        {/each}
        {#if chart.bars.length === 0}
          <text x={CH_W / 2} y={chart.baseline - 8} text-anchor="middle" class="tick">no landings in this range</text>
        {:else}
          <text x="2" y={CH_H - 4} class="tick">{chart.bars[0]?.day}</text>
          <text x={CH_W - 2} y={CH_H - 4} text-anchor="end" class="tick">{chart.bars[chart.bars.length - 1]?.day}</text>
        {/if}
      </svg>
      <div class="ct-key">
        <span><i class="sw up"></i> grew</span>
        <span><i class="sw dn"></i> shrank</span>
        <span>hover a bar for the day's landings, adds, deletes and ratio</span>
      </div>
    </div>

    <div class="ct-section">02 — PER LANDING · newest {trains.length} of {n(landings.length)}</div>
    <div class="ct-tbl">
      <table class="ct-table">
        <thead>
          <tr>
            <th>landed</th>
            <th>sha</th>
            <th>subject</th>
            <th class="num">added</th>
            <th class="num">deleted</th>
            <th class="num">net</th>
            <th class="num">delete:add</th>
            <th class="num">tests +/−</th>
          </tr>
        </thead>
        <tbody>
          {#each trains as l (l.sha)}
            {@const r = deleteAddPct(l.add, l.del)}
            <tr>
              <td class="mono">{when(l.at)}</td>
              <td class="mono">{short(l.sha)}</td>
              <td class="subj">{l.subject}</td>
              <td class="num mono">+{n(l.add)}</td>
              <td class="num mono">−{n(l.del)}</td>
              <td class="num mono" class:ok={l.add - l.del < 0} class:warn={l.add - l.del > 0}>{signed(l.add - l.del)}</td>
              <td class="num mono" class:ok={r !== null && r >= 100}>{pct(r)}</td>
              <td class="num mono dim">+{n(l.test_add)} / −{n(l.test_del)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>

    <div class="ct-section">03 — CONTEXT · the rest of the row, at head {short(newest.measured.head)}</div>
    <div class="ct-context">
      {#if newest.measured.totals}
        {@const t = newest.measured.totals}
        <div class="ct-panel">
          <div class="h">Lines by bucket · snapshot at head</div>
          <div class="ct-kv">
            <span>prod</span><span class="mono">{n(t.prod_lines)}</span>
            <span>test</span><span class="mono">{n(t.test_lines)}<small> · {pct(t.test_prod_pct)} of prod</small></span>
            <span>all</span><span class="mono">{n(t.lines)}</span>
          </div>
          <div class="ct-kv fine">
            {#each buckets as [k, v] (k)}
              <span>{k}</span><span class="mono">{n(v)}</span>
            {/each}
          </div>
        </div>
      {/if}
      <div class="ct-panel">
        <div class="h">Window by bucket · adds / deletes since {newest.measured.since ? short(newest.measured.since) : 'the first commit (backfill)'}</div>
        <div class="ct-kv fine">
          {#each Object.entries(newest.measured.window.by_bucket).sort(([, a], [, b]) => b.add - a.add) as [k, v] (k)}
            <span>{k}</span>
            <span class="mono">+{n(v.add)} / −{n(v.del)}<small> · {pct(deleteAddPct(v.add, v.del))}</small></span>
          {/each}
        </div>
      </div>
      {#if newest.measured.registry}
        {@const reg = newest.measured.registry}
        <div class="ct-panel">
          <div class="h">Registry rows · {n(reg.rows)}</div>
          <div class="ct-kv fine">
            {#each registries as [k, v] (k)}
              <span>{k}</span><span class="mono">{n(v)}</span>
            {/each}
          </div>
          <div class="h" style="margin-top: 12px">
            Code branches on kind in core ·
            {reg.code_branches_on_kind === null ? 'not counted' : n(reg.code_branches_on_kind)}
          </div>
          {#if reg.code_branches_not_counted_why}
            <p class="ct-note">{reg.code_branches_not_counted_why}</p>
          {/if}
          <div class="ct-kv fine">
            {#each classes as [k, v] (k)}
              <span>{k}</span><span class="mono">{n(v)}</span>
            {/each}
          </div>
          {#if reg.leaked_sites.length > 0}
            <div class="h" style="margin-top: 12px">The leaked sites, by file and line</div>
            <ul class="ct-sites">
              {#each reg.leaked_sites as s (`${s.file}:${s.line}`)}
                <li class="mono">{s.file}:{s.line} <span class="dim">match {s.scrutinee} · {s.literals.join(', ')}</span></li>
              {/each}
            </ul>
          {/if}
        </div>
      {/if}
      <div class="ct-panel">
        <div class="h">Counts at head</div>
        <div class="ct-kv fine">
          {#each Object.entries(newest.measured.counts) as [k, v] (k)}
            <span>{k}</span><span class="mono">{n(v)}</span>
          {/each}
        </div>
      </div>
    </div>

    <p class="ct-footnote">
      Read from {ready.packets.length} measured packet{ready.packets.length === 1 ? '' : 's'}
      {#if ready.unmeasured > 0}
        and {ready.unmeasured} that carried no measurement (failed runs){/if}{#if ready.total > ready.packets.length + ready.unmeasured}
        — {ready.total} exist and {PAGE} were read, so the series is a floor{/if}. The newest is
      <a href={href(`/jobs/${newest.id}`)} onclick={(e) => { e.preventDefault(); navigate(`/jobs/${newest.id}`); }}>{newest.title}</a>.
      The method — what each bucket is, why the series' prod/test split is a bound and the snapshot's exact,
      how a code branch on kind is counted — is stated at the top of
      <span class="mono">infra/codebase-metrics.sh</span> and rides in <span class="mono">measured.method</span> on every packet.
      Which ratios count as "simpler" is a choice, not a fact; these two are the ones the founding ideas commit to.
    </p>
  {/if}
</div>

<style>
  .ct-root { padding: 0 32px 32px; }
  .ct-section {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px; letter-spacing: var(--ls-eyebrow, 0.3em);
    color: var(--signal, #5fd4a8); margin: 28px 0 8px;
    display: flex; align-items: center; gap: 12px;
  }
  .ct-section::after { content: ''; flex: 1; border-top: 1px solid var(--hairline, #2a3138); }
  .ct-quiet { color: var(--static, #7a838c); font-size: 13px; }
  .ct-fail {
    color: var(--warn, #d9a441); border: 1px solid var(--warn, #d9a441);
    padding: 8px 12px; font-size: 13px;
  }
  .mono { font-family: var(--font-mono, ui-monospace, monospace); font-variant-numeric: tabular-nums; }
  .dim { color: var(--static, #7a838c); }
  .ok { color: var(--ok, #4fb98a); }
  .warn { color: var(--warn, #d9a441); }
  .err { color: var(--err, #e2685c); }

  .ct-findings { display: grid; gap: 10px; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr)); }
  .ct-finding { border-left: 3px solid var(--static, #7a838c); padding: 4px 0 4px 14px; font-size: 13px; }
  .ct-finding.ok { border-left-color: var(--ok, #4fb98a); }
  .ct-finding.warn { border-left-color: var(--warn, #d9a441); }
  .ct-finding.err { border-left-color: var(--err, #e2685c); }
  .ct-finding .k {
    font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: 0.1em; text-transform: uppercase; color: var(--static, #7a838c);
  }
  .ct-finding .v { font-size: 22px; font-weight: 500; line-height: 1.2; margin: 2px 0; }
  .ct-finding.ok .v { color: var(--ok, #4fb98a); }
  .ct-finding.warn .v { color: var(--warn, #d9a441); }
  .ct-finding.err .v { color: var(--err, #e2685c); }
  .ct-finding .t { color: var(--fog, #e8ecef); }

  .ct-strip {
    display: grid; grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    border: 1px solid var(--hairline, #2a3138); margin-top: 16px;
  }
  .ct-strip > div { padding: 12px 16px; border-right: 1px solid var(--hairline, #2a3138); min-width: 0; }
  .ct-strip > div:last-child { border-right: 0; }
  .ct-strip .k {
    font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: 0.1em; text-transform: uppercase; color: var(--static, #7a838c);
  }
  .ct-strip .v { font-size: 28px; font-weight: 500; font-variant-numeric: tabular-nums; line-height: 1.1; margin-top: 4px; }
  .ct-strip .v.small { font-size: 14px; line-height: 1.5; font-weight: 400; }
  .ct-strip .v small { font-size: 12px; color: var(--static, #7a838c); font-weight: 400; margin-left: 5px; }
  .ct-strip .v small.warn { color: var(--warn, #d9a441); }

  .ct-panel { border: 1px solid var(--hairline, #2a3138); padding: 14px 16px; min-width: 0; }
  .ct-panel .h { font-size: 12px; font-weight: 600; margin-bottom: 8px; }
  .ct-controls { display: flex; gap: 6px; align-items: center; flex-wrap: wrap; margin-bottom: 8px; font-size: 12px; }
  .ct-controls button {
    background: transparent; color: var(--static, #7a838c); border: 1px solid var(--hairline, #2a3138);
    padding: 2px 9px; font: 11px var(--font-mono, ui-monospace, monospace); cursor: pointer;
  }
  .ct-controls button.on { color: var(--fog, #e8ecef); border-color: var(--signal, #5fd4a8); }
  .ct-chart { width: 100%; height: auto; display: block; }
  .axis { stroke: var(--hairline, #2a3138); stroke-width: 1; }
  .bar rect { fill: var(--warn, #d9a441); opacity: 0.8; }
  .bar.down rect { fill: var(--ok, #4fb98a); }
  .bar:hover rect { opacity: 1; }
  .tick { fill: var(--static, #7a838c); font: 9px var(--font-mono, ui-monospace, monospace); }
  .ct-key { display: flex; gap: 18px; flex-wrap: wrap; margin-top: 8px; font: 11px var(--font-mono, ui-monospace, monospace); color: var(--static, #7a838c); }
  .sw { display: inline-block; width: 10px; height: 10px; vertical-align: -1px; margin-right: 4px; }
  .sw.up { background: var(--warn, #d9a441); }
  .sw.dn { background: var(--ok, #4fb98a); }

  .ct-tbl { overflow-x: auto; }
  .ct-table { width: 100%; border-collapse: collapse; font-size: 12px; min-width: 760px; }
  .ct-table th { text-align: left; font-weight: 500; color: var(--static, #7a838c); padding: 4px 8px; border-bottom: 1px solid var(--hairline, #2a3138); }
  .ct-table td { padding: 4px 8px; border-bottom: 1px solid var(--hairline, #2a3138); vertical-align: top; white-space: nowrap; }
  .ct-table .num { text-align: right; }
  .ct-table .subj { white-space: normal; max-width: 40ch; }

  .ct-context { display: grid; gap: 10px; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr)); }
  .ct-kv { display: grid; grid-template-columns: max-content 1fr; gap: 3px 14px; font-size: 13px; }
  .ct-kv.fine { font-size: 12px; color: var(--static, #7a838c); margin-top: 8px; }
  .ct-kv small { color: var(--static, #7a838c); }
  .ct-note { font-size: 12px; color: var(--warn, #d9a441); margin: 4px 0 8px; }
  .ct-sites { margin: 0; padding-left: 16px; font-size: 11px; }
  .ct-footnote { color: var(--text-faint, #5c656e); font-size: 12px; max-width: 90ch; margin-top: 20px; }
  .ct-footnote a { color: var(--signal, #5fd4a8); }
</style>
