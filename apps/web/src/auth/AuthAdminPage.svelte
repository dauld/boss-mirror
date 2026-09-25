<script lang="ts">
  // /auth-admin — admin UX for the file-backed credential store.
  // Two actions:
  //
  //   1. Onboard a new user — calls POST /api/auth/onboard with
  //      email + initial password. The user logs in with that
  //      pair; admin shares it out-of-band (chat / printed sheet)
  //      and instructs them to rotate via the reset-token flow
  //      below on first login.
  //
  //   2. Issue a reset token for an existing user — calls
  //      POST /api/auth/issue-reset; the response carries a
  //      one-time plaintext token (1h TTL). Admin shares it
  //      out-of-band; user consumes it on /login (mode=reset).
  //
  // Server-side gating requires platform-admin or break-glass
  // (boss_core::roles::can_administer_auth; backlog 34242f9a took
  // the executives and audit-readonly out of it). The SPA gate below
  // is just for affordance — a non-admin who navigates here sees the
  // "no access" notice instead of forms that would 403 anyway.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';

  type ActionResult =
    | { kind: 'idle' }
    | { kind: 'busy' }
    | { kind: 'ok'; message: string; token?: string; expires?: string }
    | { kind: 'err'; message: string };

  let role = $state<string | null>(null);
  let me = $state<string | null>(null);
  let onboardEmail = $state<string>('');
  let onboardPassword = $state<string>('');
  let onboardResult = $state<ActionResult>({ kind: 'idle' });
  let resetEmail = $state<string>('');
  let resetResult = $state<ActionResult>({ kind: 'idle' });

  // Pull the authenticated identity on mount so the page can
  // show "you're not an admin" instead of forms that would
  // server-side-403.
  $effect(() => {
    void (async () => {
      try {
        const r = await fetch('/api/auth/me');
        if (r.ok) {
          const body = (await r.json()) as { email: string; role: string | null };
          me = body.email;
          role = body.role;
        }
      } catch {
        // ignore — auth provider may not be local-auth.
      }
    })();
  });

  let isAdmin = $derived(role === 'platform-admin' || role === 'break-glass');

  async function onboard(e: Event): Promise<void> {
    e.preventDefault();
    onboardResult = { kind: 'busy' };
    try {
      const r = await fetch('/api/auth/onboard', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          email: onboardEmail.trim(),
          password: onboardPassword,
        }),
      });
      if (!r.ok) {
        onboardResult = { kind: 'err', message: (await r.text()) || `HTTP ${r.status}` };
        return;
      }
      onboardResult = {
        kind: 'ok',
        message: `Credential created for ${onboardEmail.trim()}. Share the password out-of-band; ask them to rotate via /login on first sign-in.`,
      };
      onboardEmail = '';
      onboardPassword = '';
    } catch (e) {
      onboardResult = { kind: 'err', message: String(e) };
    }
  }

  async function issueReset(e: Event): Promise<void> {
    e.preventDefault();
    resetResult = { kind: 'busy' };
    try {
      const r = await fetch('/api/auth/issue-reset', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ email: resetEmail.trim() }),
      });
      if (!r.ok) {
        resetResult = { kind: 'err', message: (await r.text()) || `HTTP ${r.status}` };
        return;
      }
      const body = (await r.json()) as { token: string; expires_at: string };
      resetResult = {
        kind: 'ok',
        message: `One-time token for ${resetEmail.trim()} (expires ${body.expires_at}):`,
        token: body.token,
        expires: body.expires_at,
      };
      resetEmail = '';
    } catch (e) {
      resetResult = { kind: 'err', message: String(e) };
    }
  }
</script>

<style>
  .stack { display: flex; flex-direction: column; gap: 24px; max-width: 640px; }
  .form-row { margin-bottom: 12px; }
  .form-row label {
    display: block; font-size: 12px; font-weight: 500;
    color: var(--static); margin-bottom: 4px;
  }
  .form-row input {
    width: 100%; padding: 8px 10px; border: 1px solid var(--hairline);
    border-radius: 6px; font-size: 13px; box-sizing: border-box;
  }
  .form-row input:focus { outline: none; border-color: var(--border-strong); }
  .submit-btn {
    background: var(--band); color: var(--on-band); border: none; border-radius: 6px;
    padding: 8px 14px; font-size: 13px; font-weight: 500; cursor: pointer;
  }
  .submit-btn:hover { background: var(--signal); }
  .submit-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .result { margin-top: 10px; padding: 10px 12px; border-radius: 6px; font-size: 12px; line-height: 1.5; }
  .result.ok { background: var(--ok-wash); color: var(--ok); }
  .result.err { background: var(--err-wash); color: var(--err); }
  .token-box {
    margin-top: 6px; padding: 8px 10px; background: var(--ink);
    border: 1px solid var(--clear); border-radius: 4px;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 13px; word-break: break-all; user-select: all;
  }
  .no-access {
    background: var(--warn-wash); border: 1px solid var(--busy); border-radius: 8px;
    padding: 14px 16px; font-size: 13px; line-height: 1.55;
  }
</style>

<div class="catalog">
  <PageHeader
    eyebrow="Auth admin"
    title="Onboard + reset"
    subtitle="File-backed credential store actions for the OSS quickstart"
  />

  {#if !isAdmin}
    <div class="no-access">
      <strong>Admin only.</strong> The onboard + reset flows require a
      <code>platform-admin</code> or <code>break-glass</code> session. {#if me}You're signed in as <code>{me}</code>{#if role}
      with role <code>{role}</code>{/if}.{:else}You're not signed in.{/if}
    </div>
  {:else}
    <div class="stack">
      <Section title="Onboard a new user">
        <p style="margin: 0 0 16px; font-size: 13px; color: var(--static)">
          Creates a credential row in <code>/var/lib/boss/auth/credentials.toml</code>
          for the email below. The password is hashed with Argon2id; the
          plaintext is never persisted. Share the email + password
          out-of-band; the user signs in at <code>/login</code> and (per
          policy) rotates the password on first login.
        </p>
        <form on:submit={onboard}>
          <div class="form-row">
            <label for="onboard-email">Email</label>
            <input
              id="onboard-email"
              type="email"
              required
              bind:value={onboardEmail}
              disabled={onboardResult.kind === 'busy'}
            />
          </div>
          <div class="form-row">
            <label for="onboard-password">Initial password</label>
            <input
              id="onboard-password"
              type="text"
              required
              bind:value={onboardPassword}
              disabled={onboardResult.kind === 'busy'}
            />
          </div>
          <button class="submit-btn" type="submit" disabled={onboardResult.kind === 'busy'}>
            {#if onboardResult.kind === 'busy'}…{:else}Create credential{/if}
          </button>
          {#if onboardResult.kind === 'ok'}
            <div class="result ok">{onboardResult.message}</div>
          {:else if onboardResult.kind === 'err'}
            <div class="result err">{onboardResult.message}</div>
          {/if}
        </form>
      </Section>

      <Section title="Issue a one-time reset token">
        <p style="margin: 0 0 16px; font-size: 13px; color: var(--static)">
          Returns a 24-character token (1h TTL) the user enters at
          <code>/login</code> (Have a reset token? mode) along with their
          new password. Token is shown once below; only the SHA-256 hash
          is persisted.
        </p>
        <form on:submit={issueReset}>
          <div class="form-row">
            <label for="reset-email">Email</label>
            <input
              id="reset-email"
              type="email"
              required
              bind:value={resetEmail}
              disabled={resetResult.kind === 'busy'}
            />
          </div>
          <button class="submit-btn" type="submit" disabled={resetResult.kind === 'busy'}>
            {#if resetResult.kind === 'busy'}…{:else}Issue token{/if}
          </button>
          {#if resetResult.kind === 'ok'}
            <div class="result ok">
              {resetResult.message}
              {#if resetResult.token}
                <div class="token-box">{resetResult.token}</div>
              {/if}
            </div>
          {:else if resetResult.kind === 'err'}
            <div class="result err">{resetResult.message}</div>
          {/if}
        </form>
      </Section>
    </div>
  {/if}
</div>
