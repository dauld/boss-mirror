<script lang="ts">
  // ICS calendar-feed management for a tech. Port of
  // apps/web/src/people/CalendarFeedSection.tsx.
  //
  // `access` decides what renders (backlog 7ae9ccec): the employee
  // manages their own feed; an operator on someone else's page gets
  // only a revoke, whose response carries no token. The parent renders
  // nothing for everyone else.
  //
  // THE URL IS SHOWN ONCE (backlog 4aaff4dc, design 3101c506, Q1
  // accepted 2026-09-26). The server keeps the token's SHA-256, never
  // the token, so the read answers only that a feed exists and when it
  // was made. The URL arrives in the response to the employee's own
  // rotate and lives in this page's state until they leave it — like a
  // forge's personal access token. Lost, it is rotated, not recovered.

  import Section from '@boss/web-kit/ui/Section.svelte';
  import { formatDateTime } from '@boss/web-kit/ui/date';
  import type { CalendarFeedAccess } from './calendarFeedAccess';

  let { empId, access } = $props<{
    empId: string;
    access: Exclude<CalendarFeedAccess, 'none'>;
  }>();

  type State =
    | { kind: 'loading' }
    | { kind: 'none' }
    | { kind: 'error'; message: string }
    | { kind: 'exists'; createdAt: string }
    | { kind: 'shown-once'; url: string }
    | { kind: 'revocable' }
    | { kind: 'revoked' }
    | { kind: 'nothing-to-revoke' };

  let feedState: State = $state<State>({ kind: 'loading' });
  let busy = $state(false);
  let copied = $state(false);

  function absoluteUrl(relative: string): string {
    const base = typeof window !== 'undefined' ? window.location.origin : '';
    return `${base}${relative}`;
  }

  async function load(): Promise<void> {
    try {
      const resp = await fetch(
        `/api/scheduling/techs/${encodeURIComponent(empId)}/calendar-token`,
      );
      if (resp.status === 404) {
        feedState = { kind: 'none' };
        return;
      }
      if (!resp.ok) throw new Error(`${resp.status}`);
      const body = (await resp.json()) as { created_at: string };
      feedState = { kind: 'exists', createdAt: body.created_at };
    } catch (e) {
      feedState = { kind: 'error', message: String(e) };
    }
  }

  $effect(() => {
    void empId;
    if (access === 'owner') {
      void load();
    } else {
      feedState = { kind: 'revocable' };
    }
  });

  async function rotate(): Promise<void> {
    busy = true;
    try {
      const resp = await fetch(
        `/api/scheduling/techs/${encodeURIComponent(empId)}/calendar-token`,
        { method: 'POST' },
      );
      if (access !== 'owner' && resp.status === 404) {
        feedState = { kind: 'nothing-to-revoke' };
        return;
      }
      if (!resp.ok) throw new Error(`${resp.status}`);
      if (access !== 'owner') {
        // An operator's rotate is a revocation; the server hands the
        // new feed only to its employee.
        feedState = { kind: 'revoked' };
        return;
      }
      const body = (await resp.json()) as { ics_url: string };
      feedState = { kind: 'shown-once', url: body.ics_url };
    } catch (e) {
      feedState = { kind: 'error', message: String(e) };
    } finally {
      busy = false;
    }
  }

  async function copy(url: string): Promise<void> {
    await navigator.clipboard.writeText(url);
    copied = true;
    setTimeout(() => (copied = false), 1500);
  }
</script>

<Section title="Calendar feed" wide>
  {#if access === 'owner'}
    <p class="prose">
      Subscribe your personal calendar (Apple, Google, Outlook) to your feed
      URL. BOSS assignments, PTO, sick days, and training blocks will appear
      in your calendar. The URL is the authentication — keep it private.
      BOSS shows it once, when you make it, and keeps no copy; if you lose
      it, rotate for a new one, which stops the old link working.
    </p>
  {:else}
    <p class="prose">
      This employee's calendar feed URL is theirs alone. Revoking it stops
      the current link working at once; they rotate their own feed on their
      own page for a new one.
    </p>
  {/if}

    {#if feedState.kind === 'revocable'}
      <button
        class="btn"
        disabled={busy}
        onclick={() => {
          if (confirm("Revoke this employee's calendar feed? Their current URL will stop working immediately.")) {
            void rotate();
          }
        }}
      >
        Revoke calendar URL
      </button>
    {:else if feedState.kind === 'revoked'}
      <p class="empty">Revoked. The old URL no longer works.</p>
    {:else if feedState.kind === 'nothing-to-revoke'}
      <p class="empty">This employee has no calendar feed to revoke.</p>
    {:else if feedState.kind === 'loading'}
      <p class="empty">Loading…</p>
    {:else if feedState.kind === 'error'}
      <p class="empty load-failed" role="alert">Couldn't load the feed ({feedState.message}).</p>
    {:else if feedState.kind === 'none'}
      <button class="btn" disabled={busy} onclick={() => void rotate()}>
        Generate calendar URL
      </button>
    {:else if feedState.kind === 'exists'}
      <p class="empty">
        Your feed was made {formatDateTime(feedState.createdAt)}. Its URL was
        shown once, then; BOSS keeps no copy.
      </p>
      <button
        class="btn"
        disabled={busy}
        onclick={() => {
          if (confirm('Rotate your feed? The URL your calendar holds will stop working immediately, and you will subscribe to the new one.')) {
            void rotate();
          }
        }}
      >
        Rotate for a new URL
      </button>
    {:else}
      {@const absUrl = absoluteUrl(feedState.url)}
      <p class="prose" role="status">
        Copy this URL into your calendar app now. It is shown this once;
        BOSS keeps no copy.
      </p>
      <div style="display:flex; gap:8px; align-items:center; margin-top:8px">
        <input
          readonly
          value={absUrl}
          style="flex:1; font-family:var(--font-mono); font-size:13px; padding:6px 8px"
          onfocus={(e: FocusEvent) => (e.target as HTMLInputElement).select()}
        />
        <button class="btn" onclick={() => void copy(absUrl)}>
          {copied ? 'Copied ✓' : 'Copy'}
        </button>
      </div>
    {/if}
</Section>
