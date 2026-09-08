<script lang="ts">
  // Phase-0 port of apps/web/src/me/MePage.tsx.
  //
  // Scope-reduced for the spike: hero + "My Jobs" list + at-a-glance
  // count. Sub-panels (bulletins, next-actions, messages, certs) are
  // deferred to phase 1. The point is to exercise the hot paths —
  // state, $effect, $derived, fetch — not to match feature parity.
  //
  // Conceptual mapping (see docs/design/human-powered-state-machine.md):
  //   $state    → a cell of machine memory
  //   $derived  → a projection of that memory
  //   $effect   → a transition that reads/writes the world
  // This page uses all three.

  import { session } from '@boss/web-kit/session/session.svelte';
  import GuestHome from './GuestHome.svelte';
  import { appNow } from '@boss/web-kit/sim-clock';
  import {
    fetchMyDay,
    claimStep,
    assignmentPacket,
    filterByProtocol,
    protocolCounts,
    type MyDayQueues,
    type AssignmentRow,
  } from './assignments';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import {
    dismissFromWatchlist,
    fetchWatchlist,
    watchlistTrack,
    windowNote,
    type WatchlistState,
  } from './watchlist';
  import PacketCard from '@boss/web-kit/ui/PacketCard.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import DecideModal from './DecideModal.svelte';
  import PasskeysPanel from './PasskeysPanel.svelte';

  // The session rune exposes the current user. The fetch effect
  // tracks exactly (id, role) — the two halves of the lens query:
  // the personal queue and the role's group queue.
  let userId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : null,
  );
  let userRole = $derived(
    session.value.kind === 'ready' ? session.value.user.role : null,
  );

  // My Day is the assignments lens now (queue-visibility Q1): one
  // indexed call whose WHERE clause IS the queue definition, instead
  // of the capped jobs?status=open scan filtered client-side. The
  // page just presents the queues.
  let queues = $state<MyDayQueues | null>(null);
  let loading = $state(true);
  // The last failed fetch (HTTP status; 0 = the network itself), null
  // while the view is current. Separate from `queues` so a failed
  // REFETCH keeps the last good view on screen and says it is stale —
  // the same honesty convention the watchlist section keeps: an error
  // is said in words, never mimed as an empty day.
  let loadError = $state<number | null>(null);
  let claimNote = $state<string | null>(null);

  // Protocol filter (David, 2026-08-14). One chip per protocol present,
  // counted across every queue, applied to every queue — the question
  // is "show me the approvals", and an approval up for grabs is still
  // an approval.
  //
  // The selection is deliberately NOT reset when the queues refetch: a
  // 10s poll that cleared the filter would fight whoever set it.
  // `protocolCounts` drops a drained protocol from the chips, and
  // `filterByProtocol` then renders an empty queue rather than
  // silently widening back to everything. It IS reset when the
  // signed-in person changes — a filter is one reader's intent, not
  // the page's (the fetch effect below owns that transition).
  let protocol = $state<string | null>(null);
  const protocols = $derived(protocolCounts(queues));
  const shown = $derived({
    verdicts: filterByProtocol(queues?.verdicts ?? [], protocol),
    mine: filterByProtocol(queues?.mine ?? [], protocol),
    upForGrabs: filterByProtocol(queues?.upForGrabs ?? [], protocol),
    notMineToDo: filterByProtocol(queues?.notMineToDo ?? [], protocol),
    inFlightElsewhere: filterByProtocol(queues?.inFlightElsewhere ?? [], protocol),
  });
  const totalShown = $derived(
    shown.verdicts.length +
      shown.mine.length +
      shown.upForGrabs.length +
      shown.notMineToDo.length +
      shown.inFlightElsewhere.length,
  );

  // Every path that refreshes the queues — mount, claim, decide —
  // lands through here. The sequence number and the uid captured at
  // issue are the landing guard: a slower older response must not
  // overwrite a newer one, and a persona switch mid-flight must not
  // bleed rows split against the old uid into the new person's page.
  let queueSeq = 0;
  async function loadQueues(uid: string, role: string): Promise<void> {
    const seq = ++queueSeq;
    const res = await fetchMyDay(uid, role);
    if (seq !== queueSeq || uid !== userId) return;
    loading = false;
    if (res.kind === 'ready') {
      queues = res.queues;
      loadError = null;
    } else {
      // Keep whatever was last known-good; the template says which
      // failure this is (nothing yet vs. stale view).
      loadError = res.status;
    }
  }

  // Tracks exactly (id, role) — the two halves of the lens query. A
  // changed uid is a different person: their day starts with no
  // filter, no stale queues, and no inherited error.
  let lastUid: string | null = null;
  $effect(() => {
    const uid = userId;
    const role = userRole;
    if (!uid || !role) return;
    if (lastUid !== null && lastUid !== uid) {
      protocol = null;
      queues = null;
      loadError = null;
    }
    lastUid = uid;
    loading = true;
    loadQueues(uid, role);
    return () => {
      queueSeq++;
    };
  });

  function retryLoad() {
    if (!userId || !userRole) return;
    loading = true;
    loadError = null;
    loadQueues(userId, userRole);
  }

  // "HTTP 0" is not a thing a person should have to decode.
  function errLabel(status: number): string {
    return status === 0 ? 'network error' : `HTTP ${status}`;
  }

  // The watchlist: packets THIS person filed, and what became of them.
  // A second station read, deliberately not folded into fetchMyDay —
  // the two answer different questions (work assigned to me vs. work I
  // reported) and one failing must not blank the other.
  //
  // Keyed on the user id only. The server binds `@me` from the session
  // itself, so this call carries no identity of its own; the id is here
  // to refetch when the signed-in person changes.
  let watchlist = $state<WatchlistState>({ kind: 'loading' });
  // Packets this reader has dismissed since the page loaded. Held
  // locally so the row disappears on click instead of after a refetch;
  // a failed write puts it straight back, because a row that vanished
  // and quietly came back later is worse than one that never left.
  let dismissing = $state<ReadonlySet<string>>(new Set());

  async function dismiss(jobId: string) {
    dismissing = new Set([...dismissing, jobId]);
    const ok = await dismissFromWatchlist(jobId);
    if (!ok) {
      const back = new Set(dismissing);
      back.delete(jobId);
      dismissing = back;
    }
  }

  // Same landing guard as the queues. A named function rather than
  // effect bodywork because onDecided calls it too: deciding a packet
  // you also filed moves both panels, and they share one screen — they
  // must not disagree about what just happened.
  let watchSeq = 0;
  async function loadWatchlist(): Promise<void> {
    const seq = ++watchSeq;
    const uid = userId;
    const w = await fetchWatchlist();
    if (seq !== watchSeq || uid !== userId) return;
    watchlist = w;
  }

  $effect(() => {
    if (!userId) return;
    // A different person's page starts loading, not showing the
    // previous reader's receipts.
    watchlist = { kind: 'loading' };
    loadWatchlist();
    return () => {
      watchSeq++;
    };
  });

  // The verdict being decided in the modal (feedback 0ab5fa3a). One at
  // a time by construction; closing without deciding changes nothing.
  let deciding = $state<AssignmentRow | null>(null);

  // The step reached a terminal status inside the modal: the row is no
  // longer a verdict, so refetch the queues. The modal stays up — the
  // surface is showing its receipt — and Close hands back a list the
  // decided row has already left. The watchlist refetches too: the
  // decided packet may be one this same person filed.
  async function onDecided() {
    if (userId && userRole) {
      loadWatchlist();
      await loadQueues(userId, userRole);
    }
  }

  // The claim hop. A 409 names the winner — losing a race is
  // ordinary queue life, so it reads as information, not an error.
  async function onClaim(row: AssignmentRow) {
    claimNote = null;
    const res = await claimStep(row.job_id, row.step.id);
    if (res.kind === 'conflict') {
      claimNote = `"${row.step.title}" was taken by ${res.holder ?? 'someone else'} first`;
    } else if (res.kind === 'error') {
      claimNote = `Claim failed (${res.message})`;
    }
    if (userId && userRole) {
      await loadQueues(userId, userRole);
    }
  }

  function timeOfDay(): string {
    const h = new Date().getHours();
    if (h < 12) return 'morning';
    if (h < 17) return 'afternoon';
    return 'evening';
  }

  function tenureYears(hireDate: string): number {
    return (
      (appNow().getTime() - new Date(hireDate).getTime()) /
      (1000 * 60 * 60 * 24 * 365)
    );
  }
</script>

{#if session.value.kind === 'loading'}
  <div class="theme-exec" style="padding: 32px">Loading session…</div>
{:else if session.value.kind === 'unauthenticated'}
  <div class="theme-exec" style="padding: 32px">
    <!-- True copy for a reachable state: the way in is the login
         page, linked. The old line said "reload the page to log in"
         — a reload only re-ran the session probe and landed back
         here. After sign-in, / renders this person's day. -->
    <p class="empty">
      Not signed in. <a href="/login">Sign in</a> to see your day.
    </p>
  </div>
{:else if session.value.kind === 'unrecognized'}
  <div class="theme-exec" style="padding: 32px">
    <p class="empty">
      Signed in as <strong>{session.value.username}</strong>, but no
      matching employee in the roster.
    </p>
  </div>
{:else if session.readonly}
  <!-- A visitor is signed in, so the session resolves — but My Day is
       an employee's board and renders for them as three empty panels
       and a failed watchlist under "0.0 years · visitor". They get the
       front door instead (David, feedback cef0f06f). -->
  <GuestHome greeting={`Good ${timeOfDay()}`} />
{:else}
  {@const user = session.value.user}
  <div class="theme-exec" style="padding: 0 32px 32px">
    <PageHeader
      eyebrow={`Good ${timeOfDay()}`}
      title={user.name}
      subtitle={`${user.role} · ${tenureYears(user.hire_date).toFixed(1)} years · ${user.department}`}
      motif="glass"
    />

    <!-- Worth showing when there is a choice to make — and ALWAYS
         while a filter is set, because the chips are also the way out
         of it. A filtered protocol draining to nothing used to unmount
         this row and leave the filter unclearable short of a reload. -->
    {#if protocols.length > 1 || protocol !== null}
      <div class="myday-protocols" role="group" aria-label="Filter by protocol">
        <FilterButton active={protocol === null} onclick={() => (protocol = null)}>
          All ({protocols.reduce((n, p) => n + p.count, 0)})
        </FilterButton>
        {#each protocols as p (p.workflow)}
          <FilterButton
            active={protocol === p.workflow}
            onclick={() => (protocol = protocol === p.workflow ? null : p.workflow)}
          >
            {p.workflow} ({p.count})
          </FilterButton>
        {/each}
      </div>
      {#if protocol !== null && totalShown === 0}
        <div class="myday-empty">
          Nothing left under <strong>{protocol}</strong> — it may have drained
          since you filtered. Pick All to see the rest.
        </div>
      {/if}
    {/if}

    {#if loadError !== null && queues !== null}
      <!-- A refetch failed. The queues below are the last good read,
           and the page says so instead of swapping them for a fake
           empty day. Dismissable: the next successful refetch clears
           it anyway. -->
      <div class="myday-stale-note" role="alert">
        <span>
          Couldn't refresh ({errLabel(loadError)}) — showing the last
          good view.
        </span>
        <button
          type="button"
          class="myday-note-dismiss"
          onclick={() => (loadError = null)}
        >
          Dismiss
        </button>
      </div>
    {/if}

    <div class="me-grid">
      {#if loadError !== null && queues === null}
        <!-- The FIRST read failed: there is no last good view to keep,
             so the page says error. Four sections of "you have no
             work" here would be the exact lie this state replaces. -->
        <Section title="Your queues" wide>
          <div class="myday-fetch-error" role="alert">
            <span>
              Couldn't load your queues ({errLabel(loadError)}). Your
              day is unknown, not empty.
            </span>
            <button type="button" class="myday-retry-btn" onclick={retryLoad}>
              Try again
            </button>
          </div>
        </Section>
      {:else}
        <!-- Verdicts first (d598681f, accepted 2026-08-19): the steps
             that need YOUR decision — sign-offs, reviews, corrections —
             separated from work you merely own, because 47 rows of
             "mine" made the two indistinguishable. -->
        <Section title="Yours to decide" wide>
          {#if loading}
            <div class="myday-loading">Loading…</div>
          {:else if shown.verdicts.length === 0}
            <div class="myday-empty">No verdicts waiting on you.</div>
          {:else}
            <div class="myday-jobs-list">
              {#each shown.verdicts as row (row.step.id)}
                <!-- Decide beside the card, the same grammar as Claim:
                     the card is the packet, the button is the queue
                     mechanic. It opens the step's real surface in a
                     modal, so a docket of verdicts is decide → next
                     without leaving the queue (feedback 0ab5fa3a). -->
                <div class="myday-grab-row">
                  <PacketCard card={assignmentPacket(row)} />
                  <button class="myday-claim-btn" onclick={() => (deciding = row)}>
                    Decide
                  </button>
                </div>
              {/each}
            </div>
          {/if}
        </Section>

        <Section title="My queue" wide>
          {#if loading}
            <div class="myday-loading">Loading your queue…</div>
          {:else if shown.mine.length === 0}
            <div class="myday-empty">
              Nothing in your personal queue right now.
            </div>
          {:else}
            <!-- The same packet card the train yard deals — one card
                 grammar across the network (d69033dd). Double-click or
                 Enter opens the job detail. -->
            <div class="myday-jobs-list">
              {#each shown.mine as row (row.step.id)}
                <PacketCard card={assignmentPacket(row)} />
              {/each}
            </div>
          {/if}
        </Section>

        <Section title="Up for grabs" wide>
          {#if claimNote}
            <div class="myday-claim-note" role="status">{claimNote}</div>
          {/if}
          {#if shown.upForGrabs.length === 0}
            <div class="myday-empty">
              Nothing on your role's queue needs a person.
            </div>
          {:else}
            <div class="myday-jobs-list">
              {#each shown.upForGrabs as row (row.step.id)}
                <!-- The claim hop stays a button beside the card: the
                     card is the packet, the claim is queue mechanics. -->
                <div class="myday-grab-row">
                  <PacketCard card={assignmentPacket(row)} />
                  <button class="myday-claim-btn" onclick={() => onClaim(row)}>
                    Claim
                  </button>
                </div>
              {/each}
            </div>
            {#if shown.inFlightElsewhere.length > 0}
              <div class="myday-inflight-note">
                {shown.inFlightElsewhere.length} role-matched step{shown.inFlightElsewhere.length === 1 ? '' : 's'} in flight with teammates
              </div>
            {/if}
          {/if}
        </Section>

        <!-- Role-matched, unclaimed, and NOT a person's job. The
             protocol says an agent (or a child job, or an external
             event) completes these, so a row here is the automation
             having not run — not work anybody should quietly pick up.
             Rendered as its own section rather than mixed into "Up for
             grabs" because the reader's question is different: not
             "what should I do next" but "what is stuck". Kept out of
             the queue above entirely, since a human claiming an
             agent-completion step is how a protocol silently becomes
             manual. -->
        {#if shown.notMineToDo.length > 0}
          <Section title="Waiting on automation" wide>
            <div class="myday-automation-note">
              The protocol says these complete without a person. They are
              on your role's queue because nothing has picked them up.
            </div>
            <div class="myday-jobs-list">
              {#each shown.notMineToDo as row (row.step.id)}
                <PacketCard card={assignmentPacket(row)} />
              {/each}
            </div>
          </Section>
        {/if}
      {/if}

      <!-- Beneath the two work queues, because it is not work: it is
           the receipt. Read-only by construction — the packets here
           belong to whoever is handling them, and the only affordance
           is the card's own double-click to open the job. -->
      <Section title="My watchlist" wide>
        {#if watchlist.kind === 'loading'}
          <div class="myday-loading">Loading your watchlist…</div>
        {:else if watchlist.kind === 'unavailable'}
          <div class="myday-empty">
            The watchlist station hasn't reached this deployment yet.
          </div>
        {:else if watchlist.kind === 'error'}
          <div class="myday-empty">Couldn't load your watchlist.</div>
        {:else if watchlist.entries.length === 0}
          <div class="myday-empty">
            You haven't filed anything. Feedback you send from the
            Feedback button appears here, and stays until its outcome
            has been visible for a while.
          </div>
        {:else}
          {@const note = windowNote(watchlist.windowDays)}
          {@const live = watchlist.entries.filter((e) => !dismissing.has(e.card.id))}
          {@const track = watchlistTrack(live)}
          {#if note}
            <div class="watch-window">{note}</div>
          {/if}
          {#if track.placed}
            <!-- Cards standing at the stop they reached, rather than a
                 flat list (David, 2026-08-15: "show that more as job
                 cards moving through stations"). Needs the queue
                 envelope to carry steps, which this station's lens asks
                 for; a registry that predates that falls through to the
                 list below, same packets, just not placed. -->
            <div class="watch-track">
              {#each track.stops as stop (stop.key)}
                <div class="watch-stop">
                  <div class="watch-stop-head">
                    <span class="watch-stop-dot" class:lit={stop.entries.length > 0}></span>
                    <span class="watch-stop-label">{stop.label}</span>
                  </div>
                  {#each stop.entries as entry (entry.card.id)}
                    <div class="watch-stop-card">
                      <PacketCard card={entry.card} onDismiss={dismiss} />
                    </div>
                  {:else}
                    <p class="watch-stop-empty">—</p>
                  {/each}
                </div>
              {/each}
            </div>
            {#if track.offTrack.length > 0}
              <div class="watch-offtrack">
                <span class="watch-offtrack-h">Read, not taken up</span>
                {#each track.offTrack as entry (entry.card.id)}
                  <div class="watch-row">
                    <PacketCard card={entry.card} onDismiss={dismiss} />
                    {#if entry.outcome}
                      <span class="watch-outcome watch-{entry.outcome.tone}">
                        {entry.outcome.label}
                      </span>
                    {/if}
                  </div>
                {/each}
              </div>
            {/if}
          {:else}
          <div class="myday-jobs-list">
            {#each watchlist.entries.filter((e) => !dismissing.has(e.card.id)) as entry (entry.card.id)}
              <div class="watch-row">
                <PacketCard card={entry.card} onDismiss={dismiss} />
                {#if entry.outcome}
                  <!-- The terminal state IS the information this
                       section exists for, so it sits beside the card
                       rather than blending into the card's tag chips
                       (all --static by design). -->
                  <span class="watch-outcome watch-{entry.outcome.tone}">
                    {entry.outcome.label}
                  </span>
                {/if}
              </div>
            {/each}
          </div>
          {/if}
        {/if}
      </Section>

      <Section title="At a glance">
        <div class="me-stats">
          <div class="me-stat-card">
            <!-- Counts what the cards above actually show — filtered,
                 verdicts included. The unfiltered mine-only count
                 contradicted the page whenever a chip was active or a
                 verdict was waiting, and "—" is the truthful figure
                 while the queues are unknown. -->
            <div class="me-stat-num">
              {queues ? shown.verdicts.length + shown.mine.length : '—'}
            </div>
            <div class="me-stat-label">steps shown in your queues</div>
          </div>
        </div>
        <PasskeysPanel />
      </Section>
    </div>
  </div>

  {#if deciding}
    <DecideModal
      jobId={deciding.job_id}
      stepId={deciding.step.id}
      onClose={() => (deciding = null)}
      {onDecided}
    />
  {/if}
{/if}

<style>
  /* Protocol chips sit above the queues, not inside one, because they
     filter all three. Wraps rather than scrolls — the count grows with
     the number of protocols in flight, and a hidden chip is a filter
     nobody knows they have. */
  .myday-protocols {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin: 4px 0 18px;
  }

  /* Packet card + claim button side by side; the card takes the row. */
  .myday-grab-row {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 8px;
    align-items: center;
  }

  .myday-claim-btn,
  .myday-retry-btn {
    margin-left: auto;
    font-size: 11px;
    font-weight: 600;
    padding: 2px 10px;
    border: 1px solid var(--accent, #0284c7);
    color: var(--accent, #0284c7);
    background: transparent;
    border-radius: 4px;
    cursor: pointer;
  }
  .myday-claim-btn:hover,
  .myday-retry-btn:hover {
    background: var(--accent, #0284c7);
    color: #fff;
  }
  /* Fetch failure is words, never a mimed empty queue. The stale note
     rides above a kept last-good view; the fetch-error block stands in
     for queues the page never got. */
  .myday-stale-note {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 8px 12px;
    border: 1px solid var(--warn, #d9a441);
    border-radius: 6px;
    font-size: 13px;
    margin: 0 0 12px 0;
  }
  .myday-note-dismiss {
    font-size: 11px;
    font-weight: 600;
    padding: 2px 10px;
    border: 1px solid var(--border, #d6d3d1);
    color: inherit;
    background: transparent;
    border-radius: 4px;
    cursor: pointer;
  }
  .myday-fetch-error {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    font-size: 13px;
  }
  .myday-claim-note {
    padding: 8px 12px;
    background: var(--bg, #f5f5f4);
    border: 1px solid var(--border, #d6d3d1);
    border-radius: 6px;
    font-size: 13px;
    margin: 0 0 12px 0;
  }
  .myday-inflight-note {
    margin-top: 8px;
    font-size: 12px;
    color: var(--text-dim, #78716c);
  }

  /* Card + outcome side by side, the same shape the claim row uses. */
  /* Cards standing at the stop they reached. Same five stops the
     guest track uses — one definition in packetTrack.ts — but rendered
     with the real PacketCard, because an operator reading their own
     watchlist wants the packet's protocol and provenance, not a
     visitor's simplified card. */
  .watch-track {
    display: grid;
    grid-auto-flow: column;
    /* 150, not 180: five stops plus gaps have to fit the section's
       width or the last card is clipped at the scroll boundary and
       reads as broken rather than as scrollable. PacketCard shrinks
       to its container (min-width: 0), so narrower columns cost
       ellipsis on the title, not a cut card. */
    grid-auto-columns: minmax(150px, 1fr);
    gap: 10px;
    overflow-x: auto;
    padding-bottom: 6px;
  }
  .watch-stop-head {
    display: flex;
    align-items: center;
    gap: 7px;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--hairline, #2A3138);
    margin-bottom: 10px;
  }
  /* Lit only where something is standing. */
  .watch-stop-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--hairline, #2A3138);
    flex: 0 0 auto;
  }
  .watch-stop-dot.lit {
    background: var(--signal, #29C7B0);
  }
  .watch-stop-label {
    font-size: 12px;
    line-height: 1.3;
    color: var(--fog, #E8ECEF);
  }
  .watch-stop-card {
    margin-bottom: 8px;
  }
  .watch-stop-empty {
    color: var(--static, #7A838C);
    margin: 0;
    font-size: 13px;
  }
  /* Off the track, not off the board: a packet we read and turned down
     is an answer the filer is owed, and burying it would make the
     track a scoreboard. */
  .watch-offtrack {
    margin-top: 16px;
    padding-top: 12px;
    border-top: 1px solid var(--hairline, #2A3138);
  }
  .watch-offtrack-h {
    display: block;
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
    letter-spacing: var(--ls-nav, 0.14em);
    text-transform: uppercase;
    color: var(--static, #7A838C);
    margin-bottom: 8px;
  }
  .watch-row {
    /* Card, then its outcome chip. The dismiss × now lives inside the
       card itself (PacketCard's opt-in onDismiss), so it rides along
       wherever the card is drawn — the flat list here, the track's
       stop cards, and the off-track rows alike — instead of being a
       sibling only this one list carried. */
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 8px;
    align-items: center;
  }
  .watch-window {
    font-size: 12px;
    color: var(--static, #7a838c);
    margin: 0 0 8px 0;
  }
  /* Mono + caps, matching the packet card's own chip treatment; only
     the color changes, and only to a declared status token. */
  .watch-outcome {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10px;
    letter-spacing: var(--ls-label, 0.1em);
    text-transform: uppercase;
    padding: 1px 6px;
    border: 1px solid currentColor;
    white-space: nowrap;
  }
  .watch-ok {
    color: var(--ok, #4fb98a);
  }
  .watch-warn {
    color: var(--warn, #d9a441);
  }
  .watch-static {
    color: var(--static, #7a838c);
  }
</style>
