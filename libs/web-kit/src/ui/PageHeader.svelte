<script lang="ts">
  // A page's header: a small label, the title, and a subtitle.
  //
  // The look is Enamel's (backlog 6f471ff6, car 1) and lives in the host
  // app's stylesheet under .exec-header / .exec-eyebrow / .exec-title, so
  // the classes stay what 76 call sites and the mocked specs already read.
  // The hop, glass, tap and barrel glyphs this component used to draw were
  // Design Language v1.0's brewery whimsy; the round-2 board's composed
  // page has none, and they were the last amber in the header.
  let { eyebrow, title, subtitle } = $props<{
    eyebrow?: string;
    title: string;
    subtitle?: string;
  }>();
</script>

<header class="exec-header" style="margin-bottom: 24px">
  <div class="exec-header-text">
    {#if eyebrow}<div class="exec-eyebrow">{eyebrow}</div>{/if}
    <h1 class="exec-title">{title}</h1>
    {#if subtitle}<p style="color: var(--static); margin: 4px 0 0">{subtitle}</p>{/if}
  </div>
</header>

<style>
  .exec-header-text { flex: 1 1 auto; min-width: 0; }
</style>
