<script lang="ts">
  // ICS calendar-feed management for a tech. Port of
  // apps/web/src/people/CalendarFeedSection.tsx.

  import Section from '@boss/web-kit/ui/Section.svelte';

  let { empId } = $props<{ empId: string }>();

  type State =
    | { kind: 'loading' }
    | { kind: 'none' }
    | { kind: 'error'; message: string }
    | { kind: 'ready'; token: string; url: string };

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
      const body = (await resp.json()) as { token: string; ics_url: string };
      feedState = { kind: 'ready', token: body.token, url: body.ics_url };
    } catch (e) {
      feedState = { kind: 'error', message: String(e) };
    }
  }

  $effect(() => {
    void empId;
    void load();
  });

  async function rotate(): Promise<void> {
    busy = true;
    try {
      const resp = await fetch(
        `/api/scheduling/techs/${encodeURIComponent(empId)}/calendar-token`,
        { method: 'POST' },
      );
      if (!resp.ok) throw new Error(`${resp.status}`);
      const body = (await resp.json()) as { token: string; ics_url: string };
      feedState = { kind: 'ready', token: body.token, url: body.ics_url };
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
    <p class="prose">
      Subscribe your personal calendar (Apple, Google, Outlook) to this URL.
      BOSS assignments, PTO, sick days, and training blocks will appear in
      your calendar. The URL is the authentication — keep it private;
      rotating invalidates the old link.
    </p>

    {#if feedState.kind === 'loading'}
      <p class="empty">Loading…</p>
    {:else if feedState.kind === 'error'}
      <p class="empty">Couldn't load token ({feedState.message}).</p>
    {:else if feedState.kind === 'none'}
      <button class="btn" disabled={busy} onclick={() => void rotate()}>
        Generate calendar URL
      </button>
    {:else}
      {@const absUrl = absoluteUrl(feedState.url)}
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
        <button
          class="btn"
          disabled={busy}
          onclick={() => {
            if (confirm('Rotate token? The old URL will stop working immediately.')) {
              void rotate();
            }
          }}
        >
          Rotate
        </button>
      </div>
    {/if}
</Section>
