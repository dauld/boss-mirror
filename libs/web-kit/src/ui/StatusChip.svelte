<script lang="ts">
  // Generic status chip — the closed-tone counterpart to the app's
  // zoo of hand-rolled `chip chip-<domain>-<state>` spans. The tone
  // set is deliberately closed (registry-style: five semantic tones,
  // not one class per domain state) and the classes are static
  // strings so grep finds every user of a tone.
  //
  // A tone is a state, so it renders an Enamel PLATE — a solid ground
  // that always carries its word (backlog 7eb59678 car 2): ok is the
  // clear plate, warn busy, err troubled, active the ready blue. `muted`
  // is not a state, so it is the quiet frame. The .plate-* rules live in
  // apps/web/src/styles.css, "States are plates".
  type Tone = 'ok' | 'warn' | 'err' | 'muted' | 'active';

  let { value, tone } = $props<{
    /// Raw status value; kebab/snake-case is humanized for display
    /// ("in-repair" → "in repair"). The plate sets it in caps, the way
    /// a sign does; the text itself stays as the record spells it.
    value: string;
    tone: Tone;
  }>();

  let label = $derived(value.replace(/[-_]/g, ' '));
</script>

{#if tone === 'ok'}
  <span class="plate plate-clear">{label}</span>
{:else if tone === 'warn'}
  <span class="plate plate-busy">{label}</span>
{:else if tone === 'err'}
  <span class="plate plate-troubled">{label}</span>
{:else if tone === 'active'}
  <span class="plate plate-ready">{label}</span>
{:else}
  <span class="plate plate-quiet">{label}</span>
{/if}
