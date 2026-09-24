<script lang="ts">
  // Notes / interactions compose + list. Optimistic prepend on post.

  import Section from '@boss/web-kit/ui/Section.svelte';
  import { appNow } from '@boss/web-kit/sim-clock';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import { formatActor, isHumanActor } from '../data/actor';
  import { loadOwnerNames, personIdsOf } from '../data/ownerNames';
  import { okRead, type ReadState } from '../data/readState';
  import { createAccountNote } from './api';
  import type { AccountNote } from './types';
  import { session } from '@boss/web-kit/session/session.svelte';

  let { accountId, notes } = $props<{
    accountId: string;
    notes: ReadonlyArray<AccountNote>;
  }>();

  type Kind = 'note' | 'call' | 'meeting' | 'email' | 'interaction';

  let empNames = $state<ReadonlyMap<string, string>>(new Map());
  let namesRead = $state<ReadState>(okRead);
  let appended = $state<AccountNote[]>([]);
  let draft = $state('');
  let kind = $state<Kind>('note');
  let posting = $state(false);
  let error = $state<string | null>(null);

  let merged = $derived([...appended, ...notes]);

  // Names only the people who wrote the ten notes shown, one row each,
  // and says so when a name cannot load. Until backlog 1e73bd93 this
  // read the WHOLE roster and dropped a refusal or a network error, so
  // the authors silently became ids. Machine authors are never asked
  // about (see ../data/ownerNames.ts). Keyed on the id list as a string
  // so posting a note by an author already shown does not re-ask.
  let authorKey = $derived(personIdsOf(merged.slice(0, 10).map((n) => n.actor_id)).join('\n'));
  $effect(() => {
    const ids = authorKey ? authorKey.split('\n') : [];
    let cancelled = false;
    (async () => {
      const out = await loadOwnerNames(ids);
      if (!cancelled) {
        empNames = out.names;
        namesRead = out.read;
      }
    })();
    return () => {
      cancelled = true;
    };
  });
  let canPost = $derived(draft.trim().length > 0 && !posting);

  let actorId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : '',
  );

  async function post(): Promise<void> {
    if (!canPost) return;
    posting = true;
    error = null;
    try {
      const created = await createAccountNote({
        account_id: accountId,
        actor_id: actorId,
        body: draft.trim(),
        kind,
      });
      appended = [created, ...appended];
      draft = '';
    } catch (e) {
      error = String(e);
    } finally {
      posting = false;
    }
  }

  function daysAgo(iso: string): string {
    const then = new Date(iso).getTime();
    const now = appNow().getTime();
    const d = Math.floor((now - then) / 86_400_000);
    if (d < 1) return 'today';
    if (d === 1) return '1d';
    if (d < 30) return `${d}d`;
    if (d < 365) return `${Math.floor(d / 30)}mo`;
    return `${Math.floor(d / 365)}y`;
  }
</script>

<Section title={`Notes & interactions (${merged.length})`}>
    <div class="pp-note-compose">
      <textarea
        class="pp-note-textarea"
        rows="2"
        placeholder="Add a note, call summary, or interaction…"
        bind:value={draft}
      ></textarea>
      <div class="pp-note-compose-row">
        <select
          class="pp-note-kind"
          bind:value={kind}
          disabled={posting}
        >
          <option value="note">Note</option>
          <option value="call">Call</option>
          <option value="meeting">Meeting</option>
          <option value="email">Email</option>
          <option value="interaction">Interaction</option>
        </select>
        <button
          class="pp-note-post"
          onclick={post}
          disabled={!canPost}
        >
          {posting ? 'Posting…' : 'Add note'}
        </button>
      </div>
      {#if error}<div class="pp-note-error">{error}</div>{/if}
    </div>

    {#if merged.length === 0}
      <p class="empty">No notes logged.</p>
    {:else}
      {#if namesRead.kind === 'failed'}
        <p class="empty load-failed" role="alert">
          Couldn't load who wrote these notes — {namesRead.error}. Authors show as ids.
        </p>
      {/if}
      <ul class="pp-note-list">
        {#each merged.slice(0, 10) as n (n.id)}
          <li class="pp-note-item">
            <div class="pp-note-item-header">
              <span class="pp-note-kind-chip">{n.kind}</span>
              <span class="pp-note-item-author">
                {#if isHumanActor(n.actor_id)}
                  <EntityLink
                    kind="employee"
                    id={n.actor_id}
                    label={empNames.get(n.actor_id)}
                    mono={false}
                  />
                {:else}
                  <!-- A machine CPU (automation or `<mode>:<model>` agent) has
                       no employee page; linking one produced a dead link
                       labelled with the raw actor id. -->
                  {formatActor(n.actor_id)}
                {/if}
              </span>
              <span class="pp-note-item-when">{daysAgo(n.created_at)} ago</span>
            </div>
            <div class="pp-note-item-body">{n.body}</div>
          </li>
        {/each}
      </ul>
    {/if}
</Section>
