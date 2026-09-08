<script lang="ts">
  // Passkey enrolment — the identity half of the presence ceremony
  // (docs/design/presence.md; packet 7218c3f1). Enrolment happens
  // behind the already-authenticated session: the gateway mints the
  // creation challenge, the browser's authenticator answers, and the
  // credential lands in BOSS's own table. Presence-gated steps then
  // verify fresh assertions against these credentials, bound to each
  // step's shape hash.
  import { enrollPasskey } from '../steps/presence';
  import { formatDate } from '@boss/web-kit/ui/date';

  type CredentialRow = Readonly<{
    credential_id: string;
    label: string;
    registered_at: string;
    last_used_at: string | null;
  }>;

  type PanelState =
    | { kind: 'loading' }
    | { kind: 'unavailable' }
    | { kind: 'ready'; credentials: readonly CredentialRow[] };

  let panel = $state<PanelState>({ kind: 'loading' });
  let label = $state('');
  let busy = $state(false);
  let error = $state('');

  async function refresh(): Promise<void> {
    const resp = await fetch('/api/auth/passkey/credentials').catch(() => null);
    if (!resp || !resp.ok) {
      // 401 (no session), 403 (guest), or the ceremony not mounted:
      // this panel simply isn't for this viewer.
      panel = { kind: 'unavailable' };
      return;
    }
    const credentials = (await resp.json()) as CredentialRow[];
    panel = { kind: 'ready', credentials };
  }

  async function add(): Promise<void> {
    busy = true;
    error = '';
    try {
      await enrollPasskey(label.trim() || 'passkey');
      label = '';
      await refresh();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  $effect(() => {
    void refresh();
  });
</script>

{#if panel.kind === 'ready'}
  <div class="passkeys-panel">
    <h3>Passkeys</h3>
    <p class="passkeys-blurb">
      Steps that demand proof of presence ask your passkey to sign the
      exact content being approved. Enrol one here; approvals then
      prompt for it in place.
    </p>
    {#if panel.credentials.length === 0}
      <p class="passkeys-empty">No passkey enrolled yet.</p>
    {:else}
      <ul class="passkeys-list">
        {#each panel.credentials as cred (cred.credential_id)}
          <li>
            <span class="passkeys-label">{cred.label}</span>
            <span class="passkeys-meta">
              enrolled {formatDate(cred.registered_at)}
              {#if cred.last_used_at}
                · last used {formatDate(cred.last_used_at)}
              {:else}
                · never used
              {/if}
            </span>
          </li>
        {/each}
      </ul>
    {/if}
    <div class="passkeys-add">
      <input
        type="text"
        placeholder="label (e.g. yubikey)"
        bind:value={label}
        disabled={busy}
      />
      <button onclick={() => void add()} disabled={busy}>
        {busy ? 'Waiting for authenticator…' : 'Add passkey'}
      </button>
    </div>
    {#if error}
      <p class="passkeys-error">{error}</p>
    {/if}
  </div>
{/if}

<style>
  .passkeys-panel {
    margin-top: 1rem;
  }
  .passkeys-panel h3 {
    margin: 0 0 0.25rem;
    font-size: 0.95rem;
  }
  .passkeys-blurb,
  .passkeys-empty {
    margin: 0 0 0.5rem;
    font-size: 0.85rem;
    color: var(--dl-text-muted, #667);
  }
  .passkeys-list {
    list-style: none;
    margin: 0 0 0.5rem;
    padding: 0;
  }
  .passkeys-list li {
    display: flex;
    gap: 0.5rem;
    align-items: baseline;
    padding: 0.15rem 0;
  }
  .passkeys-label {
    font-weight: 600;
  }
  .passkeys-meta {
    font-size: 0.8rem;
    color: var(--dl-text-muted, #667);
  }
  .passkeys-add {
    display: flex;
    gap: 0.5rem;
  }
  .passkeys-error {
    margin: 0.5rem 0 0;
    font-size: 0.85rem;
    color: var(--dl-danger, #b3261e);
  }
</style>
