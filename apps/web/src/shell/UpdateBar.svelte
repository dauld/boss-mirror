<script lang="ts">
  // "A new version landed — reload." The persistent, quiet bar that
  // ends the stale-tab trap (72c7c36e): checks on a slow interval and
  // on window focus — focus is the moment that matters, because a
  // stale tab bites exactly when someone returns to it.
  import { onMount } from 'svelte';
  import { deployHasLanded, extractMainAsset } from './deployWatch';

  const CHECK_EVERY_MS = 5 * 60 * 1000;

  let landed = $state(false);

  async function check(booted: string): Promise<void> {
    if (landed) return;
    try {
      const r = await fetch('/', { cache: 'no-store', headers: { accept: 'text/html' } });
      if (!r.ok) return;
      const served = extractMainAsset(await r.text());
      if (deployHasLanded(booted, served)) landed = true;
    } catch {
      // Offline or mid-deploy: stay quiet; the next check will see.
    }
  }

  onMount(() => {
    const booted = extractMainAsset(document.head.innerHTML);
    if (!booted) return; // dev server: unhashed modules, nothing to compare
    const tick = () => void check(booted);
    const id = window.setInterval(tick, CHECK_EVERY_MS);
    window.addEventListener('focus', tick);
    return () => {
      window.clearInterval(id);
      window.removeEventListener('focus', tick);
    };
  });
</script>

{#if landed}
  <div class="update-bar" role="status">
    <span>A new version of BOSS has landed — this tab is running the old one.</span>
    <button type="button" onclick={() => window.location.reload()}>Reload now</button>
  </div>
{/if}

<style>
  .update-bar {
    position: fixed;
    bottom: 16px;
    left: 50%;
    transform: translateX(-50%);
    z-index: 1000;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 16px;
    border: 1px solid var(--accent);
    border-radius: 8px;
    background: var(--card);
    color: var(--text);
    font-size: 13px;
  }
  .update-bar button {
    padding: 4px 12px;
    border: 0;
    border-radius: 5px;
    background: var(--accent);
    color: var(--on-band);
    font-size: 13px;
    cursor: pointer;
  }
</style>
