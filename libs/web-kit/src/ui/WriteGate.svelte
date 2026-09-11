<script lang="ts">
  // The readonly gate — ONE mechanism for every write surface.
  //
  // `session.readonly` is true for the audit-readonly guest: a
  // first-class visitor who may read every projection and change
  // nothing. Before this gate the flag had a single consumer (MePage)
  // while ~43 files issued writes, so a guest saw live composers,
  // pickers and transition buttons everywhere — each one a 403 waiting
  // for the click. The session said "read-only"; the surfaces said
  // "act". They have to agree.
  //
  // Mechanism: children render inside a <fieldset> that is disabled
  // exactly when the session is readonly. Disabling a fieldset
  // natively disables every descendant form control — button, input,
  // select, textarea — which is precisely the write-surface alphabet,
  // with no per-control edits and no divergence for surfaces added
  // later (StepPlugins mount inside the same gate). A quiet note names
  // why, and names the way back in.
  //
  // Wrap write AFFORDANCES (a step surface, a composer trigger, an
  // action row) — never read chrome like filters or navigation.
  import type { Snippet } from 'svelte';
  import { session } from '../session/session.svelte';
  import { loginUrlFor } from '../session/deadSession';

  type Props = Readonly<{ children: Snippet }>;
  let { children }: Props = $props();

  // Sign-in returns the visitor to the page they were reading —
  // /login's `safeNext` admits in-app paths only. Same target and
  // same ?next= encoding the 401 redirect uses, from the same
  // definition, so the two cannot drift.
  let loginHref = $derived(
    loginUrlFor(window.location.pathname, window.location.search),
  );
</script>

<fieldset class="write-gate" disabled={session.readonly}>
  {@render children()}
</fieldset>
{#if session.readonly}
  <p class="write-gate-note">
    Read-only session — <a href={loginHref}>sign in</a> to act.
  </p>
{/if}

<style>
  /* The fieldset is a disabling mechanism, not a layout box: no
     border, no spacing, and `min-inline-size: 0` to undo the
     `min-content` sizing fieldsets impose inside flex/grid parents. */
  .write-gate {
    display: block;
    border: 0;
    margin: 0;
    padding: 0;
    min-inline-size: 0;
  }
  /* One quiet dimming for the whole gated region — individual controls
     also pick up their own native/browser disabled rendering. */
  .write-gate:disabled {
    opacity: 0.65;
  }
  .write-gate-note {
    margin: 6px 0 0;
    font-size: 12px;
    color: var(--text-dim, #78716c);
  }
  .write-gate-note a {
    color: inherit;
    text-decoration: underline;
  }
</style>
