<script lang="ts">
  // /login — OSS quickstart login form. Email +
  // password against the file-backed credential store. POSTs to
  // /api/auth/login; the gateway sets the boss_session cookie
  // and the SPA navigates to /.
  //
  // Surfaces a "this is the eval setup" banner front-and-center
  // so the operator knows they're not on the production-grade
  // IAM path. Onboarding new users + reset-token consumption
  // share this component's state via the inline mode toggle.

  import { onMount } from 'svelte';

  let email = $state<string>('');
  let password = $state<string>('');
  let resetToken = $state<string>('');
  let mode = $state<'login' | 'reset'>('login');
  let busy = $state<boolean>(false);
  let error = $state<string | null>(null);
  let guestOffered = $state<boolean>(false);
  // Same ask-before-offering contract as guest: the IdP button only
  // renders where the gateway says OIDC is configured (idm-kanidm) -
  // a deployment without the front door never shows a door.
  let oidcOffered = $state<boolean>(false);
  // Passwords are not offered. The form renders only if BOTH the IdP and
  // guest access are unavailable — a last-resort surface for a deployment
  // with neither configured, not a path anyone here is meant to take.
  // Break-glass for this deployment is POST /api/auth/login via curl.
  let showLocalAuth = $state<boolean>(false);

  // If the SPA already has a session (someone hit /login while
  // logged in), redirect to home rather than bury them in a form.
  //
  // A guest is the exception: they are signed in, but arriving here
  // is how they ask to stop being a guest, so bouncing them home
  // would strand them in read-only with no way out but the logout
  // menu. `employee_id` is the structural difference — every
  // credential login carries one, a guest never does.
  onMount(async () => {
    try {
      const r = await fetch('/api/auth/me');
      if (r.ok) {
        const me = await r.json();
        if (me.employee_id) window.location.href = '/';
      }
    } catch {
      // ignore — auth provider may not be local-auth, in which
      // case this endpoint 404s and we just show the form.
    }
    // Ask before offering: a tenant running BOSS on their own data
    // does not enable guest access, and their people should never
    // be shown a button that answers 404.
    try {
      const r = await fetch('/api/auth/guest');
      if (r.ok) guestOffered = (await r.json()).enabled === true;
    } catch {
      // no availability endpoint — no button.
    }
    try {
      const r = await fetch('/api/auth/oidc/available');
      if (r.ok) oidcOffered = (await r.json()).enabled === true;
    } catch {
      // gateway predates the front door — no button.
    }
  });

  async function continueAsGuest(): Promise<void> {
    busy = true;
    error = null;
    try {
      const r = await fetch('/api/auth/guest', { method: 'POST' });
      if (!r.ok) {
        error = (await r.text()) || `HTTP ${r.status}`;
        return;
      }
      const params = new URLSearchParams(window.location.search);
      window.location.href = safeNext(params);
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function login(): Promise<void> {
    busy = true;
    error = null;
    try {
      const r = await fetch('/api/auth/login', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ email: email.trim(), password }),
      });
      if (!r.ok) {
        error = (await r.text()) || `HTTP ${r.status}`;
        return;
      }
      // Gateway set the boss_session cookie. Drop the next-page
      // intent into the URL if a 401-redirect put one there.
      const params = new URLSearchParams(window.location.search);
      window.location.href = safeNext(params);
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }


  // The `next` param is attacker-reachable (anything can link to
  // /login?next=...), so it admits in-app paths only — the same rule
  // router.ts applies to `from`, where the comment names the attack:
  // a value on another origin turns sign-in into an open redirect.
  // `//evil.example` is scheme-relative and just as hostile as
  // `https://`, hence the second test.
  function safeNext(params: URLSearchParams): string {
    const next = params.get('next');
    return next?.startsWith('/') && !next.startsWith('//') ? next : '/';
  }

  async function consumeReset(): Promise<void> {
    busy = true;
    error = null;
    try {
      const r = await fetch('/api/auth/reset', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          email: email.trim(),
          token: resetToken.trim(),
          password,
        }),
      });
      if (!r.ok) {
        error = (await r.text()) || `HTTP ${r.status}`;
        return;
      }
      // Reset succeeded. Flip back to login mode + auto-fill the
      // email so the user just types their (new) password.
      mode = 'login';
      resetToken = '';
      password = '';
      error = '✓ Password reset. Sign in with the new password.';
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  function submit(e: Event): void {
    e.preventDefault();
    if (mode === 'login') void login();
    else void consumeReset();
  }
</script>

<style>
  .login-shell {
    min-height: 100vh;
    display: flex;
    align-items: center;
    justify-content: center;
    background: linear-gradient(180deg, #fafaf9 0%, #f5f5f4 100%);
    padding: 32px 16px;
  }
  .login-card {
    width: 100%;
    max-width: 420px;
    background: #fff;
    border: 1px solid #e7e5e4;
    border-radius: 12px;
    padding: 32px 28px;
    box-shadow: 0 1px 3px rgba(0,0,0,0.05);
  }
  .login-eyebrow {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.6px;
    color: #78716c;
    margin-bottom: 6px;
  }
  .login-title {
    font-size: 22px;
    font-weight: 600;
    color: #1c1917;
    margin: 0 0 4px;
  }
  .login-subtitle {
    font-size: 13px;
    color: #57534e;
    margin: 0 0 20px;
  }
  .login-banner {
    background: #fef3c7;
    border: 1px solid #fde68a;
    border-radius: 8px;
    padding: 10px 12px;
    font-size: 12px;
    line-height: 1.5;
    color: #44403c;
    margin-bottom: 18px;
  }
  .login-banner strong { color: #1c1917; }
  .form-row { margin-bottom: 14px; }
  .form-row label {
    display: block;
    font-size: 12px;
    font-weight: 500;
    color: #44403c;
    margin-bottom: 4px;
  }
  .form-row input {
    width: 100%;
    padding: 9px 10px;
    border: 1px solid #d6d3d1;
    border-radius: 6px;
    font-size: 14px;
    background: #fff;
    box-sizing: border-box;
  }
  .form-row input:focus {
    outline: none;
    border-color: #1c1917;
  }
  .submit-btn {
    width: 100%;
    background: #1c1917;
    color: #fff;
    border: none;
    border-radius: 6px;
    padding: 10px 14px;
    font-size: 14px;
    font-weight: 500;
    cursor: pointer;
  }
  .submit-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .submit-btn:hover:not(:disabled) { background: #44403c; }
  .mode-toggle {
    background: none;
    border: none;
    color: #57534e;
    font-size: 12px;
    cursor: pointer;
    padding: 4px 0;
    text-decoration: underline;
  }
  .mode-toggle:hover { color: #1c1917; }
  .login-error {
    margin-top: 12px;
    padding: 8px 10px;
    border-radius: 6px;
    font-size: 12px;
    line-height: 1.4;
  }
  .login-error.bad { background: #fee2e2; color: #991b1b; }
  .login-error.ok  { background: #dcfce7; color: #166534; }
  .footer-row {
    margin-top: 18px;
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 12px;
  }
  .guest-block {
    margin-top: 16px;
  }
  .guest-divider {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 4px 0 14px;
    font-size: 11px;
    color: #a8a29e;
  }
  .guest-divider::before,
  .guest-divider::after {
    content: '';
    flex: 1;
    height: 1px;
    background: #e7e5e4;
  }
  .oidc-btn {
    display: block;
    text-align: center;
    text-decoration: none;
    box-sizing: border-box;
  }
  .guest-btn {
    width: 100%;
    background: #fff;
    color: #1c1917;
    border: 1px solid #d6d3d1;
    border-radius: 6px;
    padding: 10px 14px;
    font-size: 14px;
    font-weight: 500;
    cursor: pointer;
  }
  .guest-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .guest-btn:hover:not(:disabled) { background: #f5f5f4; border-color: #a8a29e; }
  .guest-note {
    margin: 8px 0 0;
    font-size: 11px;
    line-height: 1.5;
    color: #78716c;
  }
  .local-toggle {
    display: block;
    margin: 1.5rem auto 0;
    padding: 0;
    background: none;
    border: none;
    color: #a8a29e;
    font-size: 0.78rem;
    text-decoration: underline;
    cursor: pointer;
  }
  .local-toggle:hover { color: #78716c; }

  .guest-note code {
    font-size: 11px;
    background: #f5f5f4;
    padding: 1px 4px;
    border-radius: 3px;
  }
</style>

<div class="login-shell">
  <div class="login-card">
    <div class="login-eyebrow">BOSS · {mode === 'login' ? 'Sign in' : 'Reset password'}</div>
    <h1 class="login-title">{mode === 'login' ? 'Welcome back' : 'Set a new password'}</h1>
    <p class="login-subtitle">
      {#if mode === 'login'}
        {#if oidcOffered}
          Sign in with your passkey — or look around as a guest.
        {:else}
          Sign in with your work email + password.
        {/if}
      {:else}
        Enter the one-time token an admin shared with you.
      {/if}
    </p>

    {#if (!oidcOffered && !guestOffered) || showLocalAuth || mode !== 'login'}
    <div class="login-banner">
      <strong>This is the OSS evaluation setup.</strong> Credentials live in a local file;
      there's no email-based reset, no MFA, no account lockout. Production deployments
      use Authelia or another OIDC IDP — see TODO.md.
    </div>

    <form on:submit={submit}>
      <div class="form-row">
        <label for="login-email">Email</label>
        <input
          id="login-email"
          type="email"
          autocomplete="email"
          required
          bind:value={email}
          disabled={busy}
        />
      </div>

      {#if mode === 'reset'}
        <div class="form-row">
          <label for="login-token">Reset token</label>
          <input
            id="login-token"
            type="text"
            autocomplete="off"
            required
            bind:value={resetToken}
            disabled={busy}
          />
        </div>
      {/if}

      <div class="form-row">
        <label for="login-password">{mode === 'login' ? 'Password' : 'New password'}</label>
        <input
          id="login-password"
          type="password"
          autocomplete={mode === 'login' ? 'current-password' : 'new-password'}
          required
          bind:value={password}
          disabled={busy}
        />
      </div>

      <button type="submit" class="submit-btn" disabled={busy}>
        {#if busy}…{:else if mode === 'login'}Sign in{:else}Set password{/if}
      </button>

      {#if error}
        <div class="login-error {error.startsWith('✓') ? 'ok' : 'bad'}">{error}</div>
      {/if}

      <div class="footer-row">
        <button
          type="button"
          class="mode-toggle"
          on:click={() => { mode = mode === 'login' ? 'reset' : 'login'; error = null; }}
        >
          {mode === 'login' ? 'Have a reset token?' : 'Back to sign in'}
        </button>
      </div>
    </form>
    {/if}

    {#if oidcOffered && mode === 'login'}
      <div class="guest-block">
        {#if showLocalAuth}<div class="guest-divider"><span>or</span></div>{/if}
        <!-- A NAVIGATION, not a fetch: the auth-code flow is a round
             trip through the IdP and back to the callback. -->
        <a class="guest-btn oidc-btn" href="/api/auth/oidc/login">
          Sign in with Algedonic ID
        </a>
        <p class="guest-note">
          Your company passkey, via the identity server. Same account,
          stronger door.
        </p>
      </div>
    {/if}
    {#if guestOffered && mode === 'login'}
      <div class="guest-block">
        <div class="guest-divider"><span>or</span></div>
        <button type="button" class="guest-btn" disabled={busy} on:click={continueAsGuest}>
          Browse as a guest
        </button>
        <p class="guest-note">
          Signs you in as <code>guest@algedonic.dev</code> — read-only. You can open
          any record and follow the audit log; nothing you do changes anything.
        </p>
      </div>
    {/if}

    <!-- Break-glass. Deliberately understated and deliberately present:
         idm-kanidm.md keeps local auth precisely so an IdP outage cannot
         lock operators out. Hiding it entirely would trade one lockout
         risk for another. -->
    <!-- No password affordance. Passwords are not an accepted factor on
         this network; identity is a passkey at the IdP. Break-glass is
         still reachable for an operator with curl (POST /api/auth/login)
         — deliberately not a button, so nobody drifts back to it. -->
    <!-- The hardware-key ceremony (break-glass-is-a-key-you-hold.md)
         lives at /break-glass — a page the gateway binary serves
         itself, unlinked from here on purpose. Same posture as the
         curl path above: a typed URL an operator knows, not an
         affordance a user finds. During the Q6 soak both doors exist;
         the curl path retires in the follow-up car. -->
  </div>
</div>
