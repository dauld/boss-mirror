<script lang="ts">
  // Header block mirroring apps/web/src/ui/index.tsx PageHeader.
  //
  // Optional `motif` prop renders a small inline-SVG glyph next to
  // the title — brewery whimsy on hub pages (/me, /jobs, /accounts,
  // …). Glyphs are inlined so there's no asset round-trip and the
  // amber accent picks up the brewery palette via currentColor.
  type Motif = 'hops' | 'glass' | 'tap' | 'barrel';

  let { eyebrow, title, subtitle, motif } = $props<{
    eyebrow?: string;
    title: string;
    subtitle?: string;
    motif?: Motif;
  }>();
</script>

<header class="exec-header" style="margin-bottom: 24px">
  <div class="exec-header-inner">
    {#if motif}
      <span class="exec-motif" aria-hidden="true">
        {#if motif === 'hops'}
          <!-- Hop cone — overlapping leaf scales. -->
          <svg viewBox="0 0 32 40" fill="none" xmlns="http://www.w3.org/2000/svg">
            <path d="M16 4c-3 2-5 5-5 9 0 1 .3 2 .8 3 -2-.5-3.5-.2-4.8.8 1.5.5 2.6 1.5 3.3 2.7-1.7-.2-3.2.4-4.2 1.6 1.7.2 3 1 3.7 2.2-1.5.1-2.7.8-3.4 2 1.6.1 2.8.7 3.4 1.7-1.4.2-2.4 1-2.8 2 1.5 0 2.6.5 3.2 1.4-1 .4-1.7 1.1-2 2H22c-.3-.9-1-1.6-2-2 .6-.9 1.7-1.4 3.2-1.4-.4-1-1.4-1.8-2.8-2 .6-1 1.8-1.6 3.4-1.7-.7-1.2-1.9-1.9-3.4-2 .7-1.2 2-2 3.7-2.2-1-1.2-2.5-1.8-4.2-1.6.7-1.2 1.8-2.2 3.3-2.7-1.3-1-2.8-1.3-4.8-.8.5-1 .8-2 .8-3 0-4-2-7-5-9z"
                  stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" fill="var(--brew-amber-soft)" opacity="0.85"/>
            <path d="M16 4v32" stroke="currentColor" stroke-width="0.8" stroke-dasharray="2 2"/>
          </svg>
        {:else if motif === 'glass'}
          <!-- Pint glass with foam crown. -->
          <svg viewBox="0 0 32 40" fill="none" xmlns="http://www.w3.org/2000/svg">
            <path d="M8 8h16l-2 28H10z" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" fill="var(--brew-amber-soft)"/>
            <path d="M9 16h14" stroke="currentColor" stroke-width="0.8"/>
            <path d="M8 8c0-1.5 1.2-3 3-3 1 0 1.5.6 2.4.6.9 0 1.4-.8 2.6-.8 1.2 0 1.7.8 2.6.8.9 0 1.4-.6 2.4-.6 1.8 0 3 1.5 3 3"
                  stroke="currentColor" stroke-width="1.4" fill="var(--ink)" stroke-linejoin="round"/>
          </svg>
        {:else if motif === 'tap'}
          <!-- Tap handle — wood paddle on a brass nozzle. -->
          <svg viewBox="0 0 32 40" fill="none" xmlns="http://www.w3.org/2000/svg">
            <rect x="11" y="4" width="10" height="16" rx="2" stroke="currentColor" stroke-width="1.4" fill="var(--brew-amber-soft)"/>
            <path d="M14 20v6h4v-6" stroke="currentColor" stroke-width="1.4" fill="var(--ink)"/>
            <path d="M9 26h14l-3 6H12z" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" fill="var(--brew-amber)"/>
            <circle cx="16" cy="36" r="1.5" fill="currentColor"/>
          </svg>
        {:else if motif === 'barrel'}
          <!-- Oak barrel with two iron hoops. -->
          <svg viewBox="0 0 32 40" fill="none" xmlns="http://www.w3.org/2000/svg">
            <path d="M8 8c0-2 3.5-4 8-4s8 2 8 4v24c0 2-3.5 4-8 4s-8-2-8-4z"
                  stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" fill="var(--brew-amber-soft)"/>
            <path d="M7 14h18M7 26h18" stroke="currentColor" stroke-width="2"/>
            <path d="M16 8v24" stroke="currentColor" stroke-width="0.8" stroke-dasharray="2 2"/>
          </svg>
        {/if}
      </span>
    {/if}
    <div class="exec-header-text">
      {#if eyebrow}<div class="exec-eyebrow">{eyebrow}</div>{/if}
      <h1 class="exec-title">{title}</h1>
      {#if subtitle}<p style="color: var(--static); margin: 4px 0 0">{subtitle}</p>{/if}
    </div>
  </div>
</header>

<style>
  .exec-header-inner {
    display: flex;
    align-items: center;
    gap: 16px;
  }
  .exec-motif {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 56px;
    height: 56px;
    color: var(--brew-malt);
    background: var(--brew-amber-bg);
    border: 1.5px solid var(--brew-amber);
    border-radius: 12px;
  }
  .exec-motif svg {
    width: 36px;
    height: 36px;
  }
  .exec-header-text { flex: 1 1 auto; min-width: 0; }
</style>
