<script lang="ts">
  // `tier` is nullable — identity-first: an account can exist before it
  // is classified into a tier. Render an "untiered" chip until it is.
  //
  // `tier` is an (account, tier) Class code, and the label is that
  // Class's display_name (backlog d2c9e79f). The chip was typed to the
  // seeded platinum | gold | silver and printed the raw code, so a tier
  // a tenant added by one Class row had no type and no label. The
  // Classes are loaded here, once per session (loadClasses dedupes),
  // because four pages render this chip; until they answer, classLabel
  // humanises the code.
  import { classLabel } from '../people/types';
  import { loadClasses, classesFor } from '@boss/web-kit/session/classes.svelte';

  let { tier } = $props<{ tier: string | null }>();

  $effect(() => {
    void loadClasses('account');
  });
  let label = $derived(classLabel(tier, classesFor('account', 'tier')));
</script>

{#if tier}
  <span class="chip chip-tier chip-tier-{tier}">{label}</span>
{:else}
  <span class="chip chip-tier chip-tier-none">untiered</span>
{/if}
