<script lang="ts">
  // The estate — the hardware registry rendered instead of prose
  // (59ef456a: three hand-written accounts of the machines were wrong
  // the same way on 2026-08-30; this page reads the system so nobody
  // writes that doc again). Declared beside observed beside the
  // difference, and the dev-workspace door at the bottom.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { formatRelative } from '@boss/web-kit/ui/date';
  import {
    comparisonVerdict,
    DEV_SSH_LABEL,
    DEV_SSH_URL,
    fetchEstate,
    latestByScope,
    latestComparison,
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
      <p class="estate-fail">The registry did not answer: {estate.nodes.error}. This page refuses to guess — an unreachable registry is not an empty estate.</p>
    {:else if estate.nodes.kind === 'ready'}
      <table class="estate-table">
        <thead>
          <tr><th>machine</th><th>role</th><th>address</th><th>cpu</th><th>mem</th><th>disk</th></tr>
        </thead>
        <tbody>
          {#each estate.nodes.data.filter((n) => !n.retired) as n (n.id)}
            <tr title={n.notes ?? ''}>
              <td class="estate-id">{n.id}</td>
              <td>{n.role}</td>
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
      <p class="estate-fail">Observations unavailable: {estate.observations.error}</p>
    {:else if estate.observations.kind === 'ready'}
      <div class="estate-obs">
        {#if clusterObs}
          <div class="estate-obs-row">
            <span class="estate-scope">kubernetes-nodes</span>
            <span>{clusterObs.nodes.length} machines seen by {clusterObs.observer}</span>
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
          <p class="estate-fail">Comparisons unavailable: {estate.comparisons.error}</p>
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

    <div class="estate-section">02 — THE DEV WORKSPACE</div>
    <div class="estate-door">
      <a class="estate-launch" href={DEV_SSH_URL}>Open the dev session — {DEV_SSH_LABEL}</a>
      <p class="estate-hint">
        Opens your terminal straight into the workspace (key auth). Inside: the durable tmux
        session is <code>dev</code> — attach with <code>/work/dev-session.sh</code>, detach with
        <code>ctrl-b d</code>. For the browser instead, run <code>claude remote-control</code>
        inside the session and drive it from claude.ai.
      </p>
    </div>
  {/if}
</div>

<style>
  .estate-root { padding: 0 32px 32px; }
  .estate-section {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px; letter-spacing: var(--ls-eyebrow, 0.3em);
    color: var(--signal, #5FD4A8); margin: 28px 0 8px;
    display: flex; align-items: center; gap: 12px;
  }
  .estate-section::after { content: ''; flex: 1; border-top: 1px solid var(--hairline, #2A3138); }
  .estate-quiet { color: var(--static, #7A838C); }
  .estate-fail {
    color: var(--warn, #d9a441);
    border: 1px solid var(--warn, #d9a441);
    padding: 8px 12px; font-size: 13px;
  }
  .estate-table { width: 100%; border-collapse: collapse; font-size: 13px; }
  .estate-table th {
    text-align: left; font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase;
    color: var(--static, #7A838C); font-weight: 400;
    border-bottom: 1px solid var(--hairline, #2A3138); padding: 4px 12px 4px 0;
  }
  .estate-table td { padding: 6px 12px 6px 0; border-bottom: 1px solid var(--hairline, #2A3138); }
  .estate-id { font-family: var(--font-mono, ui-monospace, monospace); }
  .estate-addr, .estate-num { font-family: var(--font-mono, ui-monospace, monospace); color: var(--static, #7A838C); }
  .estate-obs { display: flex; flex-direction: column; gap: 6px; font-size: 13px; }
  .estate-obs-row { display: flex; gap: 16px; align-items: baseline; }
  .estate-scope {
    font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: 0.1em; text-transform: uppercase; color: var(--static, #7A838C);
    min-width: 150px;
  }
  .estate-when { color: var(--static, #7A838C); font-size: 12px; margin-left: auto; }
  .estate-ok { color: var(--signal, #5FD4A8); }
  .estate-drift { color: var(--warn, #d9a441); }
  .estate-door { display: flex; flex-direction: column; gap: 8px; }
  .estate-launch {
    font-family: var(--font-mono, ui-monospace, monospace);
    color: var(--signal, #5FD4A8); text-decoration: none;
    border: 1px solid var(--signal, #5FD4A8); border-radius: 0;
    padding: 8px 14px; width: fit-content; letter-spacing: 0.06em;
  }
  .estate-launch:hover, .estate-launch:focus { background: var(--signal, #5FD4A8); color: var(--ink-inverse, #0d1117); }
  .estate-hint { color: var(--static, #7A838C); font-size: 12px; max-width: 60ch; }
  .estate-hint code { font-family: var(--font-mono, ui-monospace, monospace); }
</style>
