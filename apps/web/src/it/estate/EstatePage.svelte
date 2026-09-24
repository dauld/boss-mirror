<script lang="ts">
  // The estate — the hardware registry rendered instead of prose
  // (59ef456a: three hand-written accounts of the machines were wrong
  // the same way on 2026-08-30; this page reads the system so nobody
  // writes that doc again). Declared beside observed beside the
  // difference, then the loops that keep the estate (0d9b2960), and the
  // dev-workspace door at the bottom.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { formatRelative } from '@boss/web-kit/ui/date';
  import {
    comparisonVerdict,
    DEV_DOOR_HOST,
    devDoorSteps,
    fetchEstate,
    latestByScope,
    latestComparison,
    LOOP_OK_OUTCOMES,
    loopAge,
    loopHost,
    type EstateState,
  } from './estate';

  let estate = $state<EstateState | null>(null);
  // One clock for every relative stamp on the page, taken when the
  // data arrived — formatRelative takes `now` explicitly, no hidden
  // wallclock.
  let loadedAt = $state<Date>(new Date());

  async function refresh(): Promise<void> {
    estate = await fetchEstate();
    loadedAt = new Date();
  }

  onMount(() => {
    void refresh();
    // Slow refresh: the estate changes on the order of days; 60s keeps
    // the relative stamps honest without hammering guest reads.
    const t = setInterval(() => void refresh(), 60_000);
    return () => clearInterval(t);
  });

  const clusterObs = $derived(
    estate?.observations.kind === 'ready' ? (latestByScope(estate.observations.data).get('kubernetes-nodes') ?? null) : null,
  );
  const hostObs = $derived(
    estate?.observations.kind === 'ready' ? (latestByScope(estate.observations.data).get('host') ?? null) : null,
  );
  const clusterCmp = $derived(
    estate?.comparisons.kind === 'ready' ? latestComparison(estate.comparisons.data, 'kubernetes-nodes') : null,
  );
  // The one-time terminal setup, spelled by the module that holds the
  // hostname — no second address typed into this file.
  const doorSteps = devDoorSteps();
</script>

<div class="estate-root">
  <PageHeader
    eyebrow="IT · Hardware"
    title="The estate"
    subtitle="Declared beside observed — what we meant to have, what a look found, and the difference"
  />

  {#if !estate}
    <p class="estate-quiet">Reading the registry…</p>
  {:else}
    <div class="estate-section">00 — THE MACHINES</div>
    {#if estate.nodes.kind === 'failed'}
      <p class="estate-fail load-failed">The registry did not answer: {estate.nodes.error}. This page refuses to guess — an unreachable registry is not an empty estate.</p>
    {:else if estate.nodes.kind === 'ready'}
      <table class="estate-table">
        <thead>
          <tr><th>machine</th><th>role</th><th>address</th><th>cpu</th><th>mem</th><th>disk</th></tr>
        </thead>
        <tbody>
          {#each estate.nodes.data.filter((n) => !n.retired) as n (n.id)}
            <tr title={n.notes ?? ''}>
              <td class="estate-id">{n.id}</td>
              <td>
                {n.role}{#if n.roles.length > 0}<span class="estate-roles"> · {n.roles.join(' · ')}</span>{/if}
              </td>
              <td class="estate-addr">{n.address ?? '—'}</td>
              <td class="estate-num">{n.cpu ?? '—'}</td>
              <td class="estate-num">{n.memory_gb != null ? `${n.memory_gb}G` : '—'}</td>
              <td class="estate-num">{n.disk_gb != null ? `${n.disk_gb}G` : '—'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    <div class="estate-section">01 — OBSERVED vs DECLARED</div>
    {#if estate.observations.kind === 'failed'}
      <p class="estate-fail load-failed">Observations unavailable: {estate.observations.error}</p>
    {:else if estate.observations.kind === 'ready'}
      <div class="estate-obs">
        {#if clusterObs}
          <div class="estate-obs-row">
            <span class="estate-scope">kubernetes-nodes</span>
            <span>
              {clusterObs.nodes.length} machines seen by {clusterObs.observer}
              <!-- Free space per node, same idiom as the host row below.
                   Until a520737f this scope recorded capacity only, so the
                   disk floor could never fire for a cluster node — w-1
                   included, the node every gate compiles on. A node whose
                   kubelet read failed says so rather than going quiet. -->
              {#each clusterObs.nodes as cn (cn.id)}
                — {cn.id}: {cn.disk_free_gb != null ? `${cn.disk_free_gb}G free` : 'free space unread'}
              {/each}
            </span>
            <span class="estate-when">{formatRelative(clusterObs.observed_at, loadedAt)}</span>
          </div>
        {:else}
          <div class="estate-obs-row"><span class="estate-scope">kubernetes-nodes</span><span>no observation recorded yet</span></div>
        {/if}
        {#if hostObs}
          <div class="estate-obs-row">
            <span class="estate-scope">host</span>
            <span>
              {hostObs.nodes.length} host{hostObs.nodes.length === 1 ? '' : 's'} seen by {hostObs.observer}
              {#each hostObs.nodes as hn (hn.id)}
                {#if hn.disk_free_gb != null}
                  — {hn.id}: {hn.disk_free_gb}G free
                {/if}
              {/each}
            </span>
            <span class="estate-when">{formatRelative(hostObs.observed_at, loadedAt)}</span>
          </div>
        {:else}
          <div class="estate-obs-row"><span class="estate-scope">host</span><span>no observation recorded yet</span></div>
        {/if}
        {#if estate.comparisons.kind === 'failed'}
          <p class="estate-fail load-failed">Comparisons unavailable: {estate.comparisons.error}</p>
        {:else if clusterCmp}
          {@const v = comparisonVerdict(clusterCmp)}
          <div class="estate-obs-row">
            <span class="estate-scope">comparison</span>
            <span class={v.ok ? 'estate-ok' : 'estate-drift'}>{v.text}</span>
            <span class="estate-when">{formatRelative(clusterCmp.observed_at, loadedAt)}</span>
          </div>
        {/if}
      </div>
    {/if}

    <!-- THE LOOPS (backlog 0d9b2960, page audit 2cff1d6e GAP 10): the
         packets the estate's own loops leave, so "did the loop run" is
         answered here. Every cell is its own read, and a failed read says
         so in that cell — an unread loop is not a loop that did not run. -->
    <div class="estate-section">02 — THE LOOPS</div>
    <p class="estate-hint">
      Did each loop run: its newest finished packet (outcome and age) and any packet still open,
      each linked. The host is the one the packet names; where a packet names none, the page says so
      rather than guess.
    </p>
    <table class="estate-table estate-loops">
      <thead>
        <tr><th>loop</th><th>host</th><th>newest finished</th><th>open</th></tr>
      </thead>
      <tbody>
        {#each estate.loops as l (`${l.kind}:${l.host ?? ''}`)}
          <tr data-loop={l.kind}>
            <td class="estate-id">{l.label}</td>
            <td class="estate-addr">{loopHost(l)}</td>
            <td class="estate-loop-latest">
              {#if l.latest.kind === 'failed'}
                <span class="estate-drift load-failed">unread: {l.latest.error}</span>
              {:else if l.latest.kind === 'ready'}
                {#if l.latest.data}
                  {@const p = l.latest.data}
                  <a class={LOOP_OK_OUTCOMES.has(p.outcome ?? '') ? 'estate-ok' : 'estate-drift'} href={`/ux/jobs/${p.id}`}>{p.outcome ?? p.status}</a>
                  <span class="estate-age">{loopAge(p.at, loadedAt)}</span>
                {:else}
                  <span class="estate-drift">no finished run recorded</span>
                {/if}
              {/if}
            </td>
            <td class="estate-loop-open">
              {#if l.open.kind === 'failed'}
                <span class="estate-drift load-failed">unread: {l.open.error}</span>
              {:else if l.open.kind === 'ready'}
                {#each l.open.data as o (o.id)}
                  <a href={`/ux/jobs/${o.id}`}>open</a>
                  <span class="estate-age">{loopAge(o.at, loadedAt)}</span>
                {:else}
                  <span class="estate-quiet">none</span>
                {/each}
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>

    <div class="estate-section">03 — THE DEV WORKSPACE</div>
    <div class="estate-door">
      <p class="estate-hint">
        The workspace answers on <code>{DEV_DOOR_HOST}</code>, from anywhere, behind Cloudflare
        Access. There is no VPN to join and no key to install: the edge asks who you are and issues
        a certificate that lasts the session. Three lines, the first two once per machine.
      </p>
      {#each doorSteps as step, i (step.command)}
        <p class="estate-hint"><strong>{i + 1}. {step.what}</strong> — {step.why}</p>
        <pre class="estate-snippet">{step.command}</pre>
      {/each}
      <p class="estate-hint">
        Inside: the durable tmux session is <code>dev</code> — attach with
        <code>/work/dev-session.sh</code>, detach with <code>ctrl-b d</code>. For the browser
        instead, run <code>claude remote-control</code> inside the session and drive it from
        claude.ai.
      </p>
    </div>
  {/if}
</div>

<style>
  .estate-root { padding: 0 32px 32px; }
  .estate-section {
    font-family: var(--font-mono);
    font-size: 12px; letter-spacing: var(--ls-eyebrow);
    color: var(--signal); margin: 28px 0 8px;
    display: flex; align-items: center; gap: 12px;
  }
  .estate-section::after { content: ''; flex: 1; border-top: 1px solid var(--hairline); }
  .estate-quiet { color: var(--static); }
  .estate-fail {
    color: var(--warn);
    border: 1px solid var(--warn);
    padding: 8px 12px; font-size: 13px;
  }
  .estate-table { width: 100%; border-collapse: collapse; font-size: 13px; }
  .estate-table th {
    text-align: left; font-family: var(--font-mono);
    font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase;
    color: var(--static); font-weight: 400;
    border-bottom: 1px solid var(--hairline); padding: 4px 12px 4px 0;
  }
  .estate-table td { padding: 6px 12px 6px 0; border-bottom: 1px solid var(--hairline); }
  .estate-id { font-family: var(--font-mono); }
  .estate-addr, .estate-num { font-family: var(--font-mono); color: var(--static); }
  .estate-roles { font-family: var(--font-mono); font-size: 11px; color: var(--static); }
  .estate-obs { display: flex; flex-direction: column; gap: 6px; font-size: 13px; }
  .estate-obs-row { display: flex; gap: 16px; align-items: baseline; }
  .estate-scope {
    font-family: var(--font-mono); font-size: 11px;
    letter-spacing: 0.1em; text-transform: uppercase; color: var(--static);
    min-width: 150px;
  }
  .estate-when { color: var(--static); font-size: 12px; margin-left: auto; }
  .estate-age { color: var(--static); font-size: 12px; margin-left: 8px; }
  .estate-ok { color: var(--signal); }
  .estate-drift { color: var(--warn); }
  .estate-door { display: flex; flex-direction: column; gap: 8px; }
  .estate-hint { color: var(--static); font-size: 12px; max-width: 60ch; }
  .estate-hint code { font-family: var(--font-mono); }
  .estate-hint strong { color: var(--fog); font-weight: 500; }
  /* One click selects the whole snippet — copyable without a button. */
  .estate-snippet {
    font-family: var(--font-mono); font-size: 12px;
    color: var(--signal); border: 1px solid var(--hairline);
    padding: 8px 14px; margin: 0; width: fit-content; user-select: all;
  }
</style>
