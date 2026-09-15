<script lang="ts">
  // /it/registry/drift — what is live that the tree does not say, and
  // what the tree says that is not live, read off the newest daily
  // `maintenance-protocol-drift` packet (4ae9969e, car 2 of 8f4e9cc0).
  //
  // David, 2026-09-11: "we might need some sort of doc diff view for me
  // to approve." Car 1 measures and files (infra/protocol-drift.sh, on
  // boss-gcp at 05:20 UTC, train #376); this tab is the view; car 3b
  // (approve.ts, ApprovePublish.svelte) is the approve: one control per
  // adrift kind that files the publish-workflow ops-request and shows
  // the host's answer beside the drift it was approved against. The
  // packet is the record and this page reads it: what a compared field
  // is, why structural fields are deliberately not compared, what the
  // 90-character windows are, is documented at the top of that script
  // and in the lint it runs, and is not restated here.
  //
  // READING ORDER: the measured header — which tree (head, when it
  // landed, when it was measured — a stale converge must look stale,
  // not current), which registry, and the four counts; then one row
  // per drifted field with both excerpts; then the two other
  // directions the script names, unauthored and pending; then what the
  // file makes no claim about. Everything is a field of the packet
  // (drift.ts, tested); a null head is said with the row's reason.
  //
  // THREE EMPTY STATES, told apart: a read that failed (load-failed,
  // never an empty table); no packet at all (the cadence has not filed
  // — what renders before the first 05:20 run); packets that exist and
  // carry no measurement (the script refused, exit 3, and PATCHed
  // nothing). Each is a different fact; "0 adrift" is none of them.
  //
  // THE APPROVE SECTION reads two more things, each its own Remote so a
  // failure there is said there and does not blank the measurement:
  // this verb's ops-requests (the answer each kind's control shows —
  // re-read every 10 s while one is in flight, so the answer arrives
  // without a reload) and the ops-request row's `execute` role, which
  // is who the control admits.
  //
  // A ROW THE VERB HAS PUBLISHED SINCE the measurement reads superseded
  // (car 3c): greyed in the table, its control labelled with the
  // versions and the instant, and its fields out of the header's
  // adrift count — until the next 05:20 run re-measures it. That is
  // rowState (approve.ts, tested) over the requests already read here,
  // never a new fetch; a later refusal is the newer answer and un-greys
  // the row. While the requests are loading or failed nothing is
  // superseded: the count is then what the packet said.
  import { onDestroy, onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { session } from '@boss/web-kit/session/session.svelte';
  import type { Remote } from '../../data/remote';
  import { loadDriftPackets, newestMeasured, type DriftPage } from './drift';
  import {
    adriftKinds,
    latestFor,
    loadExecuteAuthorityRole,
    loadPublishRequests,
    PUBLISH_HOST,
    rowState,
    supersededFieldCount,
    type PublishRequest,
    type RowState,
  } from './approve';
  import ApprovePublish from './ApprovePublish.svelte';

  /** Packets read per load: one a day, so a fortnight; the page says
   *  when `total` is past this. */
  const PAGE = 14;

  let page = $state<Remote<DriftPage>>({ kind: 'loading' });
  let requests = $state<Remote<ReadonlyArray<PublishRequest>>>({ kind: 'loading' });
  let authorityRole = $state<Remote<string | null>>({ kind: 'loading' });

  async function refresh(): Promise<void> {
    page = await loadDriftPackets(PAGE);
  }
  async function refreshRequests(): Promise<void> {
    requests = await loadPublishRequests();
  }
  /** While a request is open the runner's answer is a minute away;
   *  re-read until it lands, then stop. */
  const POLL_MS = 10_000;
  let poll: ReturnType<typeof setInterval> | null = null;
  $effect(() => {
    const inFlight = requests.kind === 'ready' && requests.data.some((r) => r.status === 'open');
    if (inFlight && poll === null) poll = setInterval(() => void refreshRequests(), POLL_MS);
    if (!inFlight && poll !== null) {
      clearInterval(poll);
      poll = null;
    }
  });
  onMount(() => {
    void refresh();
    void refreshRequests();
    void loadExecuteAuthorityRole().then((r) => {
      authorityRole = r;
    });
  });
  onDestroy(() => {
    if (poll !== null) clearInterval(poll);
  });

  const ready = $derived(page.kind === 'ready' ? page.data : null);
  const newest = $derived(ready ? newestMeasured(ready.packets) : null);
  const kinds = $derived(newest ? adriftKinds(newest.drift.fields) : []);
  const states = $derived<ReadonlyMap<string, RowState>>(
    new Map(
      kinds.map((k) => [
        k.kind,
        newest && requests.kind === 'ready' ? rowState(k, latestFor(requests.data, k.kind), newest.measured.at) : { kind: 'adrift' },
      ]),
    ),
  );
  const supersededFields = $derived(supersededFieldCount(kinds, states));
  const isSuperseded = (kind: string): boolean => states.get(kind)?.kind === 'superseded';
  const viewerId = $derived(session.value.kind === 'ready' ? session.value.user.id : null);
  const viewerRole = $derived(session.value.kind === 'ready' ? session.value.user.role : null);

  const short = (sha: string | null): string => (sha ? sha.slice(0, 8) : '—');
  const when = (iso: string | null): string => (iso ? `${iso.slice(0, 10)} ${iso.slice(11, 16)}Z` : '—');
  const n = (v: number | null): string => (v === null ? 'n/a' : v.toLocaleString('en-US'));
  /** The lint's verdict in its own vocabulary — the exit the gate
   *  would have given, recorded on the row so nobody re-derives it. */
  const verdict = (exit: number | null): string =>
    exit === 0 ? 'agreement' : exit === 1 ? 'a live kind the tree does not author' : exit === 2 ? 'field drift' : exit === null ? 'no verdict recorded' : `lint exit ${exit}`;
  const tone = (exit: number | null): string => (exit === 0 ? 'ok' : exit === null ? '' : 'warn');
</script>

<div class="pd-root">
  <PageHeader
    eyebrow="IT · Registry · what is live that the tree does not say"
    title="Protocol drift"
    subtitle="The authored bundle (infra/platform/workflows) against the live registry, compared by the gate's own lint once a day and filed as a packet. Each row is a field an operator reads — label, description, category — where the file and the active live row disagree; both full copies stay at their homes, and the excerpts here are the first place they differ."
  />

  {#if page.kind === 'failed'}
    <p class="pd-fail load-failed">
      The drift packets did not answer: {page.error}. An unreachable read is not a registry in agreement
      with its tree, so nothing is drawn.
    </p>
  {:else if page.kind === 'loading'}
    <p class="pd-quiet">Reading the measured packets…</p>
  {:else if !ready || ready.total === 0}
    <p class="pd-notice">
      No protocol-drift packet exists yet — the 05:20 measurement has not filed. The first run was expected
      2026-09-15 05:20Z on boss-gcp (infra/protocol-drift.sh, train #376); until a packet carries a
      measurement this tab has nothing it can honestly draw, and an empty table would read as agreement.
    </p>
  {:else if !newest}
    <p class="pd-fail load-failed">
      {ready.total} protocol-drift packet{ready.total === 1 ? '' : 's'} exist and none carries a measurement
      ({ready.unmeasured} failed run{ready.unmeasured === 1 ? '' : 's'}: the script refused before comparing,
      or filed nothing). That is a failed measurement, not a registry in agreement.
    </p>
  {:else}
    <div class="pd-section">
      00 — THE MEASUREMENT · at head {short(newest.measured.head)}, measured {when(newest.measured.at)}
    </div>
    <div class="pd-strip">
      <div title="the checkout the bundle was read from: its head commit and when that head landed">
        <div class="k">tree</div>
        <div class="v small">
          {#if newest.measured.head}
            <span class="mono">{short(newest.measured.head)}</span>
            <br />
            <small>landed {when(newest.measured.head_at)}</small>
          {:else}
            <span class="warn">head unknown</span>
            <br />
            <small>{newest.measured.head_why ?? 'no reason recorded'} — the comparison still ran</small>
          {/if}
        </div>
      </div>
      <div title="the registry the live rows were read from">
        <div class="k">registry</div>
        <div class="v small">
          <span class="mono">{newest.measured.target ?? '—'}</span>
          <br />
          <small>measured {when(newest.measured.at)}</small>
        </div>
      </div>
      <div title="kinds the live registry admits · kinds the tree authors">
        <div class="k">admitted · authored</div>
        <div class="v">{n(newest.measured.live_admitted)} <small>·</small> {n(newest.measured.authored)}</div>
      </div>
      <div title="kinds with both a file and an active live row, whose operator-read fields were compared">
        <div class="k">compared</div>
        <div class="v">
          {n(newest.measured.fields_compared)}
          <small>of {n(newest.measured.fields_parsed)} parsed</small>
        </div>
      </div>
      <div title="compared fields where the file and the live row disagree, less those a publish has answered since the measurement">
        <div class="k">adrift</div>
        <div class="v {newest.drift.counts.fields - supersededFields > 0 ? 'warn' : 'ok'}">
          {n(newest.drift.counts.fields - supersededFields)}
          {#if supersededFields > 0}
            <small>of {n(newest.drift.counts.fields)} measured; {n(supersededFields)} published since</small>
          {/if}
        </div>
      </div>
      <div title="the lint's own exit on this run — the verdict the gate would have given">
        <div class="k">verdict</div>
        <div class="v small {tone(newest.measured.lint_exit)}">{verdict(newest.measured.lint_exit)}</div>
      </div>
    </div>

    <div class="pd-section">01 — FIELDS ADRIFT · {newest.drift.fields.length} row{newest.drift.fields.length === 1 ? '' : 's'}, one per kind and field</div>
    {#if newest.drift.fields.length === 0}
      <p class="pd-quiet">
        Every compared field agrees with its file at head {short(newest.measured.head)} — {n(newest.measured.fields_compared)}
        kinds compared, none adrift.
      </p>
    {:else}
      <div class="pd-tbl">
        <table class="pd-table">
          <thead>
            <tr>
              <th>kind</th>
              <th>field</th>
              <th class="num">live</th>
              <th class="num">differs at</th>
              <th>authored (the file in the tree)</th>
              <th>live (the active row)</th>
            </tr>
          </thead>
          <tbody>
            {#each newest.drift.fields as f (`${f.kind}.${f.field}`)}
              <tr class:superseded={isSuperseded(f.kind)} title={isSuperseded(f.kind) ? 'published since this measurement — see the approve below' : undefined}>
                <td class="mono">{f.kind}</td>
                <td class="mono">{f.field}</td>
                <td class="num mono">{f.live_version === null ? '—' : `v${f.live_version}`}</td>
                <td class="num mono" title="offset of the first differing character · file length vs live length">
                  {f.at === null ? '—' : f.at}
                  <small>{n(f.tree_len)} vs {n(f.live_len)} chars</small>
                </td>
                <td class="excerpt">{f.tree_window}</td>
                <td class="excerpt">{f.live_window}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}

    <div class="pd-section">02 — APPROVE · publish the tree's row, one control per adrift kind</div>
    {#if kinds.length === 0}
      <p class="pd-quiet">Nothing to approve: no compared field is adrift.</p>
    {:else if requests.kind === 'failed'}
      <p class="pd-fail load-failed">
        The publish requests did not answer: {requests.error}. Without them a kind's control cannot know whether
        a request is already in flight or what the host last said, so none is offered.
      </p>
    {:else if authorityRole.kind === 'failed'}
      <p class="pd-fail load-failed">
        The ops-request protocol did not answer: {authorityRole.error}. The control admits the role that row's
        execute step names, so without it none is offered.
      </p>
    {:else if requests.kind === 'loading' || authorityRole.kind === 'loading'}
      <p class="pd-quiet">Reading the publish requests…</p>
    {:else}
      <p class="pd-approve-note">
        Each approve files an ops-request for {PUBLISH_HOST} — the same packet a terminal's boss ops files — and the
        runner there publishes the tree's row at head {short(newest.measured.head)} behind the verb's own refusals;
        the packet's answer, exit code and full output, is shown here when it closes. A refusal that the live row
        carries what the tree never said opens the second, named confirmation.
      </p>
      {#each kinds as k (k.kind)}
        <ApprovePublish
          kind={k}
          against={{ packet: newest.id, head: newest.measured.head }}
          latest={latestFor(requests.data, k.kind)}
          standing={states.get(k.kind) ?? { kind: 'adrift' }}
          authorityRole={authorityRole.data}
          {viewerId}
          {viewerRole}
          onFiled={() => void refreshRequests()}
        />
      {/each}
    {/if}

    <div class="pd-section">03 — THE OTHER TWO DIRECTIONS</div>
    <div class="pd-context">
      <div>
        <div class="h">
          Live, unauthored · {n(newest.drift.counts.unauthored)}
          <small>what is live that the tree does not say — the lint FAILS on these</small>
        </div>
        {#if newest.drift.unauthored.length === 0}
          <p class="pd-quiet">every admitted kind is authored somewhere in the tree</p>
        {:else}
          <ul class="pd-list">
            {#each newest.drift.unauthored as k (k)}
              <li class="mono warn">{k}</li>
            {/each}
          </ul>
        {/if}
      </div>
      <div>
        <div class="h">
          Authored, not yet live · {n(newest.drift.counts.pending)}
          <small>what the tree says that is not live — the window between a merge and its seed</small>
        </div>
        {#if newest.drift.pending.length === 0}
          <p class="pd-quiet">every bundle kind has a live row</p>
        {:else}
          <ul class="pd-list">
            {#each newest.drift.pending as k (k)}
              <li class="mono">{k}</li>
            {/each}
          </ul>
        {/if}
      </div>
      <div>
        <div class="h">
          No claim in the file · {n(newest.drift.counts.absent)}
          <small>fields the file leaves out; not drift, and not counted as it</small>
        </div>
        {#if newest.drift.absent.length === 0}
          <p class="pd-quiet">every file claims every compared field</p>
        {:else}
          <ul class="pd-list">
            {#each newest.drift.absent as a (`${a.kind}.${a.field}`)}
              <li class="mono">{a.kind}.{a.field}</li>
            {/each}
          </ul>
        {/if}
      </div>
    </div>

    <p class="pd-footnote">
      Newest of {ready.packets.length} measured packet{ready.packets.length === 1 ? '' : 's'}
      {ready.unmeasured > 0 ? ` (${ready.unmeasured} more exist without a measurement — failed runs)` : ''}
      {ready.total > PAGE ? `; ${ready.total} exist in all, the newest ${PAGE} were read` : ''}.
      {#if newest.measured.exempt.length > 0}
        Exempt from the authored check: {newest.measured.exempt.join(', ')}.
      {/if}
      The comparator is the same lint every gate runs; this page re-derives nothing.
    </p>
  {/if}
</div>

<style>
  .pd-root { padding: 0 32px 32px; }
  .pd-section {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px; letter-spacing: var(--ls-eyebrow, 0.3em);
    color: var(--signal, #5fd4a8); margin: 28px 0 8px;
    display: flex; align-items: center; gap: 12px;
  }
  .pd-section::after { content: ''; flex: 1; border-top: 1px solid var(--hairline, #2a3138); }
  .pd-quiet { color: var(--static, #7a838c); font-size: 13px; }
  .pd-fail {
    color: var(--warn, #d9a441); border: 1px solid var(--warn, #d9a441);
    padding: 8px 12px; font-size: 13px;
  }
  /* Not a failure: the cadence has not run yet, and the page says so
     in its own voice rather than the failure's. */
  .pd-notice {
    color: var(--fog, #e8ecef); border: 1px solid var(--hairline, #2a3138);
    padding: 8px 12px; font-size: 13px; max-width: 90ch;
  }
  .mono { font-family: var(--font-mono, ui-monospace, monospace); font-variant-numeric: tabular-nums; }
  .ok { color: var(--ok, #4fb98a); }
  .warn { color: var(--warn, #d9a441); }

  .pd-strip {
    display: grid; grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    border: 1px solid var(--hairline, #2a3138);
  }
  .pd-strip > div { padding: 12px 16px; border-right: 1px solid var(--hairline, #2a3138); min-width: 0; }
  .pd-strip > div:last-child { border-right: 0; }
  .pd-strip .k {
    font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: 0.1em; text-transform: uppercase; color: var(--static, #7a838c);
  }
  .pd-strip .v { font-size: 28px; font-weight: 500; font-variant-numeric: tabular-nums; line-height: 1.1; margin-top: 4px; }
  .pd-strip .v.small { font-size: 14px; line-height: 1.5; font-weight: 400; }
  .pd-strip .v small { font-size: 12px; color: var(--static, #7a838c); font-weight: 400; margin-left: 5px; }

  .pd-tbl { overflow-x: auto; }
  .pd-table { width: 100%; border-collapse: collapse; font-size: 12px; min-width: 760px; }
  .pd-table th { text-align: left; font-weight: 500; color: var(--static, #7a838c); padding: 4px 8px; border-bottom: 1px solid var(--hairline, #2a3138); }
  .pd-table td { padding: 6px 8px; border-bottom: 1px solid var(--hairline, #2a3138); vertical-align: top; white-space: nowrap; }
  .pd-table td small { display: block; color: var(--static, #7a838c); font-size: 11px; }
  .pd-table .num { text-align: right; }
  /* Published since the measurement: still the packet's row, drawn as
     history rather than as work. */
  .pd-table tr.superseded td { color: var(--text-faint, #5c656e); }
  .pd-table tr.superseded .excerpt { color: var(--text-faint, #5c656e); }
  .pd-table .excerpt {
    white-space: pre-wrap; overflow-wrap: anywhere; max-width: 40ch;
    font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px; color: var(--fog, #e8ecef);
  }

  .pd-context { display: grid; gap: 14px; grid-template-columns: repeat(auto-fit, minmax(280px, 1fr)); }
  .pd-context .h { font-size: 13px; font-weight: 600; margin-bottom: 4px; }
  .pd-context .h small { display: block; font-weight: 400; font-size: 11px; color: var(--static, #7a838c); }
  .pd-list { margin: 0; padding-left: 16px; font-size: 12px; }
  .pd-footnote { color: var(--text-faint, #5c656e); font-size: 12px; max-width: 90ch; margin-top: 20px; }
  .pd-approve-note { color: var(--static, #7a838c); font-size: 12px; max-width: 90ch; margin: 0 0 10px; }
</style>
