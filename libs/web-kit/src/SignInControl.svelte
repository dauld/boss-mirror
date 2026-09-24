<script lang="ts">
  // Sign-in / sign-out control for the top perspective bar. Probes
  // /api/auth/me to decide which to show — the gateway 401s anonymous
  // demo sessions, so a demo visitor sees "Sign in" and a real
  // operator sees "Sign out". Styled for the dark bar. Render it once,
  // inside the perspective tab bar.
  import { requestLogout, type LogoutOutcome } from './session/logout';

  let isLoggedIn = $state<boolean>(false);
  /** The last sign-out that did NOT land, shown beside the button.
   *  Cleared on the next attempt. */
  let refusal = $state<Exclude<LogoutOutcome, { kind: 'signed-out' }> | null>(null);
  let signingOut = $state(false);
  $effect(() => {
    (async () => {
      try {
        const r = await fetch('/api/auth/me');
        isLoggedIn = r.ok;
      } catch {
        isLoggedIn = false;
      }
    })();
  });

  // Navigation follows a CONFIRMED sign-out. Until 2026-09-18 this
  // redirected whatever the gateway answered ("best-effort"), so a
  // refused logout looked exactly like a successful one with the
  // session still live — the interaction crawl's refused-write leg
  // found it (car f09aafe1, backlog a5dff6f1). A refusal now stays on
  // the page and says so.
  async function signOut(): Promise<void> {
    signingOut = true;
    refusal = null;
    const outcome = await requestLogout();
    signingOut = false;
    if (outcome.kind === 'signed-out') {
      window.location.href = '/login';
      return;
    }
    refusal = outcome;
  }
</script>

{#if isLoggedIn}
  <button class="signin-btn" onclick={signOut} disabled={signingOut}>Sign out</button>
  {#if refusal}
    <span class="signin-refusal" role="alert">
      {#if refusal.kind === 'refused'}
        sign out refused (HTTP {refusal.status}){refusal.detail ? `: ${refusal.detail}` : ''}
      {:else}
        sign out did not reach the gateway: {refusal.detail}
      {/if}
    </span>
  {/if}
{:else}
  <a class="signin-btn" href="/login">Sign in</a>
{/if}

<style>
  /* Ghost button, §04: square corners, hairline border, mono caps.
     Hover inverts rather than tinting — the spec's one hover rule for
     buttons, and it keeps SIGNAL free for state that means something. */
  .signin-btn {
    background: transparent;
    border: 1px solid var(--hairline);
    border-radius: var(--radius);
    padding: 5px 12px;
    font-family: var(--font-mono);
    font-size: 11px;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: var(--ls-nav);
    color: var(--fog);
    text-decoration: none;
    cursor: pointer;
    line-height: 1.4;
    white-space: nowrap;
    transition: background 0.1s, color 0.1s, border-color 0.1s;
  }
  .signin-btn:hover {
    background: var(--fog);
    color: var(--void);
    border-color: var(--fog);
  }
  /* The refusal beside the button: the bar's mono voice, in the
     error tone, so a sign-out that did not land looks like one. */
  .signin-refusal {
    margin-left: 8px;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--err);
    white-space: nowrap;
  }
</style>
