<script lang="ts">
  // Inbox — port of apps/web/src/inbox/InboxPage.tsx.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { appNow } from '@boss/web-kit/sim-clock';
  import FilterGroup from '@boss/web-kit/ui/FilterGroup.svelte';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import SearchInput from '@boss/web-kit/ui/SearchInput.svelte';
  import WriteGate from '@boss/web-kit/ui/WriteGate.svelte';
  import type { Message, MessageKind } from './types';
  import type { Employee } from '../people/types';
  import { href, navigate } from '../router';
  import { session } from '@boss/web-kit/session/session.svelte';
  import { fetchRemote, type Remote } from '../data/remote';
  import { postEach, postWrite, type BulkOutcome } from './writes';

  /// `needs-you` is the default view, and the reason this file changed.
  ///
  /// The inbox opened on `all`: every message ever addressed to you, in
  /// one flat stream, thousands of them on a running deployment. The
  /// actionable items were in there — "I think I have actionable items
  /// somewhere in my Inbox, but it is unprocessable in its current
  /// form" — but finding them meant reading past every signal the
  /// machine had ever emitted.
  ///
  /// The distinction the data already carries: a `direct` message is a
  /// person (or an agent) addressing YOU and expecting something; a
  /// `signal` is the machine telling you a thing happened. Unread
  /// directs are the only category that is waiting on you, so that is
  /// what the page opens on. Everything else is one click away and
  /// nothing is hidden.
  type KindFilter = MessageKind | 'all' | 'unread' | 'needs-you';

  /// The inbox itself, as a discriminated union — a failed fetch is a
  /// FAILED inbox, never an empty one. The old shape (`messages = []`
  /// plus a swallowed error) made an outage render "Nothing is
  /// waiting on you", which is a claim about the world the page had
  /// no basis for (packet 3fba9c35, the false-empty sweep).
  let inbox = $state<Remote<Message[]>>({ kind: 'loading' });
  let messages = $derived(inbox.kind === 'ready' ? inbox.data : []);
  let employees = $state<Employee[]>([]);
  let kindFilter = $state<KindFilter>('needs-you');
  let query = $state('');
  let composing = $state(false);

  let recipientId = $state('');
  let subject = $state('');
  let body = $state('');
  let sending = $state(false);

  /// A write the server refused, said where it was asked (page audit
  /// 5477d9eb; ./writes.ts has the history). Mark read's answer rides
  /// on its row, keyed by message id; Send's rides in the modal, which
  /// stays open so nothing typed is lost.
  let markReadRefusals = $state<Readonly<Record<string, string>>>({});
  let sendRefusal = $state<string | null>(null);

  /// The page's OUT third (page audit 5477d9eb, GAP 8; backlog
  /// 5963a322). One Mark read at a time was the only way a message
  /// left view — used once in the audit window against 201 messages —
  /// while POST /api/messages/{id}/archive sat unused. Archive takes a
  /// row out of the inbox (the read leaves archived rows out, 8578b91e);
  /// the bulk bar applies Mark read to every shown unread row, or
  /// Archive to the checked ones, one per-row write each, and says how
  /// many landed. A refusal lands on its row, as Mark read's does.
  let archiveRefusals = $state<Readonly<Record<string, string>>>({});
  let selected = $state<ReadonlySet<string>>(new Set());
  let bulkBusy = $state(false);
  let bulkNote = $state<{ text: string; refused: boolean } | null>(null);

  let userId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : '',
  );

  async function refreshInbox(): Promise<void> {
    if (!userId) return;
    inbox = await fetchRemote(
      `/api/messages/inbox/${encodeURIComponent(userId)}`,
      (raw) => (Array.isArray(raw) ? (raw as Message[]) : []),
    );
  }

  $effect(() => {
    const uid = userId;
    if (!uid) return;
    void refreshInbox();
    // Load the roster alongside so the compose modal can offer names.
    (async () => {
      try {
        const r = await fetch('/api/people');
        if (r.ok) employees = (await r.json()) as Employee[];
      } catch {
        // ignore
      }
    })();
  });

  let employeeById = $derived.by(() => {
    const m = new Map<string, Employee>();
    for (const e of employees) m.set(e.id, e);
    return m;
  });

  let unread = $derived(messages.filter((m) => m.read_at === null));
  /// Waiting on you: unread, and from someone rather than from the
  /// machine.
  let needsYou = $derived(
    messages.filter((m) => m.read_at === null && m.kind === 'direct'),
  );
  let directCount = $derived(messages.filter((m) => m.kind === 'direct').length);
  let signalCount = $derived(messages.filter((m) => m.kind === 'signal').length);

  let visible = $derived(
    messages.filter((m) => {
      if (kindFilter === 'needs-you' && (m.read_at !== null || m.kind !== 'direct'))
        return false;
      if (kindFilter === 'unread' && m.read_at !== null) return false;
      if (kindFilter === 'direct' && m.kind !== 'direct') return false;
      if (kindFilter === 'signal' && m.kind !== 'signal') return false;
      if (query) {
        const q = query.toLowerCase();
        const hay = `${m.subject} ${m.body} ${m.sender_id}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    }),
  );

  /// What the bulk bar acts on is always what is SHOWN: a row checked
  /// under one filter and hidden by the next is not archived unseen.
  let visibleUnread = $derived(visible.filter((m) => m.read_at === null));
  let selectedShown = $derived(visible.filter((m) => selected.has(m.id)));
  let allShownSelected = $derived(
    visible.length > 0 && visible.every((m) => selected.has(m.id)),
  );

  /// True once the message is read. A refusal lands on the row and
  /// answers false; an admitted write clears any earlier refusal.
  async function markRead(m: Message): Promise<boolean> {
    if (m.read_at !== null) return true;
    const out = await postWrite(`/api/messages/${encodeURIComponent(m.id)}/read`);
    markReadRefusals = Object.fromEntries(
      Object.entries(markReadRefusals).filter(([id]) => id !== m.id),
    );
    if (out.kind === 'refused') {
      markReadRefusals = { ...markReadRefusals, [m.id]: out.reason };
      return false;
    }
    await refreshInbox();
    return true;
  }

  /// The entity link marks the message read, then goes. It AWAITS the
  /// write: fired unawaited, a refusal would answer on a page already
  /// left, which is the swallow this replaces (129da587). A refused
  /// write keeps the viewer here, where the row says why, and the row
  /// offers the same link without the write.
  async function openEntity(m: Message, path: string): Promise<void> {
    if (await markRead(m)) navigate(href(path));
  }

  /// A refusal map with `done` cleared and `refused` added.
  function settle(
    prior: Readonly<Record<string, string>>,
    out: BulkOutcome,
  ): Readonly<Record<string, string>> {
    const cleared = Object.fromEntries(
      Object.entries(prior).filter(([id]) => !out.done.includes(id)),
    );
    return { ...cleared, ...out.refused };
  }

  /// "Marked 2 of 3 read — 1 refused; each row says why."
  function noteFor(said: string, out: BulkOutcome): typeof bulkNote {
    const refused = Object.keys(out.refused).length;
    return {
      text: said + (refused ? ` — ${refused} refused; each row says why.` : '.'),
      refused: refused > 0,
    };
  }

  async function archive(m: Message): Promise<void> {
    const out = await postEach([m.id], (id) => `/api/messages/${encodeURIComponent(id)}/archive`);
    archiveRefusals = settle(archiveRefusals, out);
    if (out.done.length) await refreshInbox();
  }

  async function markAllRead(): Promise<void> {
    const ids = visibleUnread.map((m) => m.id);
    bulkBusy = true;
    bulkNote = null;
    const out = await postEach(ids, (id) => `/api/messages/${encodeURIComponent(id)}/read`);
    markReadRefusals = settle(markReadRefusals, out);
    bulkNote = noteFor(`Marked ${out.done.length} of ${ids.length} read`, out);
    bulkBusy = false;
    await refreshInbox();
  }

  async function archiveSelected(): Promise<void> {
    const ids = selectedShown.map((m) => m.id);
    bulkBusy = true;
    bulkNote = null;
    const out = await postEach(ids, (id) => `/api/messages/${encodeURIComponent(id)}/archive`);
    archiveRefusals = settle(archiveRefusals, out);
    selected = new Set([...selected].filter((id) => !out.done.includes(id)));
    bulkNote = noteFor(`Archived ${out.done.length} of ${ids.length}`, out);
    bulkBusy = false;
    await refreshInbox();
  }

  function toggleSelected(id: string, on: boolean): void {
    selected = on
      ? new Set([...selected, id])
      : new Set([...selected].filter((s) => s !== id));
  }

  function selectAllShown(on: boolean): void {
    const shown = new Set(visible.map((m) => m.id));
    selected = on
      ? new Set([...selected, ...shown])
      : new Set([...selected].filter((s) => !shown.has(s)));
  }

  function formatAge(iso: string): string {
    const diff = appNow().getTime() - new Date(iso).getTime();
    const hours = Math.floor(diff / (1000 * 60 * 60));
    if (hours < 1) return 'just now';
    if (hours < 24) return `${hours}h`;
    const days = Math.floor(hours / 24);
    return `${days}d`;
  }

  function senderLabel(m: Message): string {
    if (m.sender_id === 'system') return 'System';
    return employeeById.get(m.sender_id)?.name ?? m.sender_id;
  }

  async function send(): Promise<void> {
    if (!recipientId || !subject || !body || !userId) return;
    sending = true;
    sendRefusal = null;
    const out = await postWrite('/api/messages/send', {
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        sender_id: userId,
        recipient_id: recipientId,
        subject,
        body,
      }),
    });
    sending = false;
    if (out.kind === 'refused') {
      sendRefusal = out.reason;
      return;
    }
    composing = false;
    recipientId = '';
    subject = '';
    body = '';
    await refreshInbox();
  }

  function openCompose(): void {
    sendRefusal = null;
    composing = true;
  }
</script>

<div class="catalog theme-exec">
  <!-- The headline is a claim about the world; the page only gets to
       make it from a loaded inbox. While loading or failed it says
       neither "nothing waiting" nor a count. -->
  <PageHeader
    eyebrow="Inbox"
    title={inbox.kind !== 'ready'
      ? 'Inbox'
      : needsYou.length === 0
        ? 'Nothing is waiting on you'
        : `${needsYou.length} waiting on you`}
    subtitle={inbox.kind === 'ready'
      ? `${unread.length} unread · ${directCount} direct · ${signalCount} signals · ${messages.length} total`
      : inbox.kind === 'failed'
        ? 'The message store could not be reached.'
        : 'Loading…'}
  />

  <div style="padding:0 32px 12px">
    <!-- The composer's entry stands behind the readonly gate: a guest
         sees Compose disabled with the sign-in note, not a live modal
         whose Send 403s. -->
    <WriteGate>
      <button class="hr-action-btn" onclick={openCompose}>Compose</button>
    </WriteGate>
  </div>

  {#if composing}
    <div
      class="compose-overlay"
      role="presentation"
      onclick={() => (composing = false)}
    >
      <div
        class="compose-modal"
        role="dialog"
        aria-modal="true"
        tabindex="-1"
        onclick={(e) => e.stopPropagation()}
        onkeydown={(e) => e.stopPropagation()}
      >
        <div class="compose-header">
          <span class="compose-title">New Message</span>
          <button class="debug-close" onclick={() => (composing = false)}>✕</button>
        </div>
        <div class="compose-field">
          <label for="inbox-to">To</label>
          <select
            id="inbox-to"
            bind:value={recipientId}
            class="hr-select"
            style="width:100%"
          >
            <option value="">Select recipient...</option>
            {#each employees as e (e.id)}
              <option value={e.id}>{e.name} ({e.role})</option>
            {/each}
          </select>
        </div>
        <div class="compose-field">
          <label for="inbox-subject">Subject</label>
          <input
            id="inbox-subject"
            type="text"
            bind:value={subject}
            class="compose-input"
            placeholder="Subject..."
          />
        </div>
        <div class="compose-field">
          <label for="inbox-body">Message</label>
          <textarea
            id="inbox-body"
            bind:value={body}
            class="compose-textarea"
            rows="5"
            placeholder="Write your message..."
          ></textarea>
        </div>
        {#if sendRefusal !== null}
          <p class="compose-refused" role="alert">Not sent — {sendRefusal}</p>
        {/if}
        <div class="compose-actions">
          <button
            class="hr-action-btn"
            onclick={send}
            disabled={sending || !recipientId || !subject || !body}
          >
            {sending ? 'Sending...' : 'Send'}
          </button>
          <button class="hr-detail-btn" onclick={() => (composing = false)}>Cancel</button>
        </div>
      </div>
    </div>
  {/if}

  <div class="catalog-layout">
    <aside class="catalog-filters">
      <FilterGroup label="Search">
          <SearchInput bind:value={query} placeholder="Subject, sender…" />
      </FilterGroup>
      <FilterGroup label="Filter">
          <!-- First and default. The other four are still here and
               nothing is hidden — this only decides what you land on. -->
          <FilterButton
            active={kindFilter === 'needs-you'}
            onclick={() => (kindFilter = 'needs-you')}
          >
            Waiting on you ({needsYou.length})
          </FilterButton>
          <FilterButton active={kindFilter === 'all'} onclick={() => (kindFilter = 'all')}>
            All ({messages.length})
          </FilterButton>
          <FilterButton active={kindFilter === 'unread'} onclick={() => (kindFilter = 'unread')}>
            Unread ({unread.length})
          </FilterButton>
          <FilterButton active={kindFilter === 'direct'} onclick={() => (kindFilter = 'direct')}>
            Direct ({directCount})
          </FilterButton>
          <FilterButton active={kindFilter === 'signal'} onclick={() => (kindFilter = 'signal')}>
            Signals ({signalCount})
          </FilterButton>
      </FilterGroup>
    </aside>

    <section class="list-section">
      <!-- Above the list, not in it: a bulk write that empties the
           filter still says what it did. -->
      {#if bulkNote !== null}
        <p
          class="inbox-bulk-note {bulkNote.refused ? 'inbox-bulk-note-refused' : ''}"
          role={bulkNote.refused ? 'alert' : 'status'}
        >
          {bulkNote.text}
        </p>
      {/if}
      {#if inbox.kind === 'loading'}
        <p class="empty">Loading…</p>
      {:else if inbox.kind === 'failed'}
        <!-- A failed load is a failure, distinct from an empty inbox. -->
        <p class="empty load-failed" role="alert">
          Couldn't load your inbox — {inbox.error}
        </p>
        <div style="padding:0 32px">
          <button class="hr-action-btn" onclick={() => void refreshInbox()}>Retry</button>
        </div>
      {:else if visible.length === 0}
        <p class="empty">No messages match those filters.</p>
      {:else}
        <!-- The bulk bar acts on what is shown (backlog 5963a322). -->
        <WriteGate>
          <div class="inbox-bulk">
            <label class="inbox-bulk-all">
              <input
                type="checkbox"
                checked={allShownSelected}
                onchange={(e) => selectAllShown(e.currentTarget.checked)}
              />
              Select all shown
            </label>
            <button
              class="inbox-mark-read"
              onclick={() => void markAllRead()}
              disabled={bulkBusy || visibleUnread.length === 0}
            >
              Mark all read ({visibleUnread.length})
            </button>
            <button
              class="inbox-mark-read"
              onclick={() => void archiveSelected()}
              disabled={bulkBusy || selectedShown.length === 0}
            >
              Archive selected ({selectedShown.length})
            </button>
          </div>
        </WriteGate>
        <div class="inbox-list">
          {#each visible as m (m.id)}
            {@const isUnread = m.read_at === null}
            <div class="inbox-row {isUnread ? 'inbox-row-unread' : ''}">
              <div class="inbox-row-header">
                <input
                  type="checkbox"
                  class="inbox-select"
                  aria-label="Select {m.subject}"
                  checked={selected.has(m.id)}
                  onchange={(e) => toggleSelected(m.id, e.currentTarget.checked)}
                />
                <span class="inbox-kind inbox-kind-{m.kind}">
                  {m.kind === 'signal' ? '⚡' : '✉'}
                </span>
                <span class="inbox-sender {isUnread ? 'inbox-sender-bold' : ''}">
                  {senderLabel(m)}
                </span>
                <span class="inbox-age">{formatAge(m.sent_at)}</span>
                {#if isUnread}
                  <button
                    class="inbox-mark-read"
                    onclick={() => markRead(m)}
                    title="Mark as read"
                  >
                    Mark read
                  </button>
                {/if}
                <button
                  class="inbox-mark-read inbox-archive"
                  onclick={() => void archive(m)}
                  title="Archive: take it out of the inbox"
                >
                  Archive
                </button>
              </div>
              {#if markReadRefusals[m.id]}
                <p class="inbox-write-refused" role="alert">
                  Not marked read — {markReadRefusals[m.id]}
                </p>
              {/if}
              {#if archiveRefusals[m.id]}
                <p class="inbox-write-refused" role="alert">
                  Not archived — {archiveRefusals[m.id]}
                </p>
              {/if}
              <div class="inbox-subject {isUnread ? 'inbox-subject-bold' : ''}">
                {m.subject}
              </div>
              <div class="inbox-body">{m.body}</div>
              <!-- `entity_path` is the producer-owned SPA link: every
                   emitter that attaches an entity_ref populates it, so
                   the inbox never has to know tenant route shapes. A
                   missing path renders as plain text, not a link. -->
              {#if m.entity_ref}
                {@const path = m.entity_ref.entity_path ?? null}
                <div class="inbox-entity">
                  {#if path}
                    <a
                      href={href(path)}
                      class="inbox-entity-link"
                      onclick={(e) => {
                        e.preventDefault();
                        void openEntity(m, path);
                      }}
                    >
                      {m.entity_ref.entity_type}: {m.entity_ref.entity_id}
                    </a>
                    {#if markReadRefusals[m.id]}
                      <a
                        href={href(path)}
                        class="inbox-entity-link inbox-open-anyway"
                        onclick={(e) => {
                          e.preventDefault();
                          navigate(href(path));
                        }}
                      >
                        Open without marking read
                      </a>
                    {/if}
                  {:else}
                    <span class="mono">
                      {m.entity_ref.entity_type}: {m.entity_ref.entity_id}
                    </span>
                  {/if}
                </div>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </section>
  </div>
</div>
