<script lang="ts">
  // A queue of Jobs parked on a human, as a board.
  //
  // Columns come from the Workflow, not from this file. A triage step
  // that forks declares its dispositions as an inline enum field, and
  // each disposition has a successor step gated on it — so the board
  // renders one column per route the workflow actually offers, labelled
  // with that successor's own title. Add a disposition to the registry
  // and the column appears; nothing here changes.
  //
  // That is the whole point of the redesign. The first version had
  // three hardcoded columns ending in "Triaged", which made triage a
  // synonym for closing. Triage's real output is a decision about what
  // happens next, so dropping a card into a column IS choosing that
  // route: it completes the fork step with that disposition, which
  // makes the corresponding next step ready.
  //
  // Columns therefore remain STEP STATE, never a stored field. A
  // Step's `status` is the program counter of the state machine, so a
  // card cannot disagree with the Job behind it.
  //
  // The agent hand-off is an annotation on an untriaged card rather
  // than a column: an agent taking a first pass is not a disposition,
  // and treating it as one was the modelling error. It records a
  // durable request rather than firing something — it survives a
  // reload, and an agent taking an automatic first pass later writes
  // the same record with no human clicking.
  import type { Snippet } from 'svelte';
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import PacketModal from '@boss/web-kit/ui/PacketModal.svelte';
  import { session } from '@boss/web-kit/session/session.svelte';
  import type { Job, Step } from './types';
  // The fork rule lives in one module — it drifted once between this
  // board and the terminal queue reader. See jobs/fork.ts.
  import { type Fork, forkStep as forkStepOf, gatedStep, readFork } from './fork';
  import { formatActor } from '../data/actor';

  type Props = Readonly<{
    /// Which queue this board shows. One Workflow today because that is
    /// what `JobFilter` can push into SQL; a board over "everything
    /// awaiting a human" needs a server-side filter that does not
    /// exist yet, and doing it client-side would silently truncate.
    kind: string;
    title: string;
    subtitle?: string;
    emptyMessage?: string;
    /// How many days of FINISHED work the terminal columns keep.
    ///
    /// A board renders each card in the column of its current step, so
    /// terminal packets have to be fetched or the terminal columns are
    /// empty — which is why this board originally asked for the whole
    /// history and got it. On the feedback queue that meant 173 rows
    /// fetched to show 14 live ones, 27 short of silently truncating at
    /// its own limit, and a board that read as an enormous backlog when
    /// the backlog was fourteen.
    ///
    /// A board is a working surface, not an archive. Everything live is
    /// always here regardless of age; finished work ages out and stays
    /// one click away in the list below.
    terminalWindowDays?: number;
    /// The card body. Defaults to the Job title; callers whose Jobs
    /// carry a better headline supply their own.
    card?: Snippet<[Job]>;
  }>;

  let {
    kind,
    title,
    subtitle = 'Routing an item completes its triage step, which opens the next one — so a card cannot disagree with the Job behind it.',
    emptyMessage = 'Nothing is waiting on a person right now.',
    terminalWindowDays = 14,
    card,
  }: Props = $props();

  let jobs = $state<ReadonlyArray<Job>>([]);
  let fork = $state<Fork | null>(null);

  /// Finished packets older than the window — the ones the board is
  /// deliberately not showing.
  ///
  /// Named rather than merely omitted. A board that quietly drops
  /// history teaches the next person that the history is gone, which
  /// is the same confusion in the opposite direction. `null` means not
  /// counted yet, and renders nothing.
  let archived = $state<number | null>(null);

  /// The Job shown in the detail modal, or null when it is closed.
  ///
  /// A card is deliberately terse — it is a thing you drag between
  /// columns, and a card carrying a paragraph of feedback is a card
  /// you cannot scan. But the detail was then reachable nowhere: the
  /// full text of an item, and the state of the steps behind it, only
  /// existed in the database. Double-click opens it.
  ///
  /// This holds the id rather than the Job so the modal re-reads from
  /// `jobs` on every refresh; holding the object would freeze the
  /// modal on a stale copy the moment a poll lands.
  let detailId = $state<string | null>(null);
  const detail = $derived(detailId ? (jobs.find((j) => j.id === detailId) ?? null) : null);

  // Escape-to-close via $effect rather than a svelte:window tag. The
  // bun+svelte bundler crashes on the svelte:window event lookup
  // ($.window resolves undefined), which takes the WHOLE app down —
  // not just this component: `.app-shell` never mounts and every route
  // that renders a board dies with "Cannot read properties of
  // undefined (reading 'addEventListener')".
  //
  // DebugGear.svelte hit this first and documented it there. A comment
  // in one file does not stop the next person reaching for the obvious
  // construct, which is exactly what happened here, so there is now a
  // lint for it (CLAUDE.md §9a).
  $effect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === 'Escape') detailId = null;
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  });
  // Employee id -> name, so a card says "David Hauld" rather than
  // `emp-bootstrap-admin` (feedback 19896c17). formatActor already
  // knows how to spell every actor kind — machines, agents, humans —
  // and falls back to the raw id when the roster has not arrived or
  // does not contain the id, so a slow or failed fetch degrades to
  // exactly the old behaviour rather than to a blank.
  let empNames = $state<Map<string, string>>(new Map());
  let loading = $state(true);
  let error = $state<string | null>(null);
  let busy = $state<Record<string, boolean>>({});
  let choice = $state<Record<string, string>>({});

  let me = $derived(session.value.kind === 'ready' ? session.value.user.id : '');

  // Items move through triage from outside this tab (an agent
  // routes, a step completes) — without a poll the board shows the
  // world as of page-load and "nothing is moving" reads as a bug.
  // 15s, skipped while any card's action is in flight.
  $effect(() => {
    const t = setInterval(() => {
      if (!loading && !Object.values(busy).some(Boolean)) void load(true);
    }, 15_000);
    return () => clearInterval(t);
  });

  const WAITING = '__waiting__';
  const CLOSED = '__closed__';

  /// `forkStep` bound to this board's fork. The rule itself is in
  /// jobs/fork.ts; this is just the partial application.
  function forkStep(j: Job): Step | undefined {
    return forkStepOf(j, fork);
  }

  function isTerminal(s: Step | undefined): boolean {
    return s?.status === 'completed' || s?.status === 'skipped';
  }

  function agentRequestedAt(j: Job): string | null {
    const v = forkStep(j)?.metadata?.['agent_requested_at'];
    return typeof v === 'string' ? v : null;
  }

  function agentRequestedBy(j: Job): string | null {
    const v = forkStep(j)?.metadata?.['agent_requested_by'];
    return typeof v === 'string' ? v : null;
  }

  let routeIds = $derived(new Set((fork?.options ?? []).map((o) => o.value)));

  /// Which column a Job sits in — derived, never stored. Untriaged
  /// items wait; triaged ones sit under the route they were sent to.
  ///
  /// A triaged item whose disposition is not a current route lands in
  /// `CLOSED` rather than vanishing: Jobs opened before the fork have
  /// no disposition at all, and a route the registry later drops would
  /// otherwise take its cards off the board with it.
  function columnOf(j: Job): string {
    // A closed packet is not queue contents, whatever its triage step
    // says. Placing by fork-step state alone put every closed packet
    // under the route it was once sent to, indistinguishable from live
    // work — measured 2026-08-19: the board showed 198 cards of which
    // 16 were open, and read as "150+ still open" (David). The Job's
    // own status outranks the program counter of one of its steps.
    if (j.status === 'closed' || j.status === 'cancelled') return CLOSED;
    const s = forkStep(j);
    if (!isTerminal(s)) return WAITING;
    const chosen = fork ? s?.metadata?.[fork.field] : undefined;
    return typeof chosen === 'string' && routeIds.has(chosen) ? chosen : CLOSED;
  }

  let columns = $derived.by(() => {
    const head = [
      { id: WAITING, label: 'Waiting on triage', hint: 'Nobody has routed these yet.' },
    ];
    const routes = (fork?.options ?? []).map((o) => ({
      id: o.value,
      label: o.label,
      hint: 'Routed here at triage.',
    }));
    // Only shown when something is in it — an always-empty trailing
    // column is noise on a board that already scrolls.
    const closed = jobs.some((j) => columnOf(j) === CLOSED)
      ? [{ id: CLOSED, label: 'Closed', hint: 'Closed packets (any route), and items triaged before these routes existed.' }]
      : [];
    return [...head, ...routes, ...closed];
  });

  let byColumn = $derived.by(() => {
    const out: Record<string, Job[]> = {};
    for (const c of columns) out[c.id] = [];
    for (const j of jobs) {
      const col = columnOf(j);
      (out[col] ??= []).push(j);
    }
    return out;
  });


  /// How much finished work the window is holding back.
  ///
  /// One extra request, and only on a foreground load — the 15s poll
  /// skips it. The count moves when something closes, which is rare on
  /// the timescale of a poll tick, and the alternative is doubling the
  /// board's request rate forever to keep a footnote current.
  ///
  /// `limit=1` because only `total` is wanted; the rows are discarded.
  /// A failure here is silent: the count is a footnote, and a board
  /// that shows an error because its footnote did not load is worse
  /// than a board with no footnote.
  async function countArchived(shown: number, background: boolean): Promise<void> {
    if (background && archived !== null) return;
    try {
      const res = await fetch(`/api/jobs?kind=${encodeURIComponent(kind)}&limit=1`);
      if (!res.ok) return;
      const total = (await res.json())?.total;
      if (typeof total === 'number') archived = Math.max(0, total - shown);
    } catch {
      // Footnote only — leave it as it was.
    }
  }

  async function load(background = false): Promise<void> {
    // Background refreshes are silent (feedback 15c6004e): flipping
    // `loading` re-renders the whole board into its spinner every
    // poll tick — the flash WAS the poll. First load and explicit
    // reloads keep the spinner; the 15s tick updates data in place.
    if (!background) loading = true;
    error = null;
    try {
      const q = new URLSearchParams({
        kind,
        limit: '200',
        // The retention window, applied SERVER-side. Filtering after
        // the fetch would not help: the limit truncates first, so a
        // board with more history than its limit silently loses live
        // cards off the end and looks fine doing it.
        closed_within: String(terminalWindowDays),
      });
      const [jobsRes, kindsRes] = await Promise.all([
        // The list endpoint enriches each Job with its steps, so the
        // board needs one request rather than one per card.
        fetch(`/api/jobs?${q}`),
        fetch('/api/workflows'),
      ]);
      if (!jobsRes.ok) throw new Error(`${kind} jobs: HTTP ${jobsRes.status}`);
      // The registry read is part of the board's truth, not an
      // enhancement. Placement derives from the fork, so "no fork"
      // (null) files every routed card under WAITING — a column whose
      // hint says nobody has routed them. When the registry genuinely
      // has no fork for this kind that is the right render; when the
      // FETCH failed it is a false statement, so the failure is the
      // board's failure (packet 3fba9c35, the false-empty sweep).
      if (!kindsRes.ok) throw new Error(`workflows registry: HTTP ${kindsRes.status}`);
      const body = await jobsRes.json();
      jobs = Array.isArray(body) ? body : (body.data ?? []);
      countArchived(typeof body.total === 'number' ? body.total : jobs.length, background);

      const kinds = await kindsRes.json();
      const rows: unknown[] = Array.isArray(kinds) ? kinds : (kinds.data ?? []);
      fork = readFork(rows.find((k) => (k as { kind?: string }).kind === kind));
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  /// PUT semantics on a step are read-overlay-write, and top-level
  /// metadata is replaced wholesale — so merge with what is already
  /// there or the other keys are wiped.
  ///
  /// The merge is load-bearing, not hygiene: `authority_role` lives in
  /// this same metadata and is how the fork step is found at all. A
  /// write that replaced metadata would make the card vanish on its
  /// first hand-off.
  async function patchStep(
    j: Job,
    patch: Record<string, unknown>,
    metadata?: Record<string, unknown>,
  ): Promise<void> {
    const step = forkStep(j);
    if (!step || busy[j.id]) return;
    busy = { ...busy, [j.id]: true };
    try {
      const body: Record<string, unknown> = { ...patch };
      if (metadata) body.metadata = { ...(step.metadata ?? {}), ...metadata };
      const r = await fetch(`/api/jobs/${j.id}/steps/${step.id}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
      await load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = { ...busy, [j.id]: false };
    }
  }

  /// Routing IS triaging: completing the fork step with a disposition
  /// is what opens the next step. There is no separate "move".
  function route(j: Job, disposition: string): Promise<void> | void {
    if (!fork) return patchStep(j, { status: 'completed' });
    if (!fork.options.some((o) => o.value === disposition)) return;
    return patchStep(j, { status: 'completed' }, { [fork.field]: disposition });
  }

  const handToAgent = (j: Job) =>
    patchStep(
      j,
      {},
      { agent_requested_at: new Date().toISOString(), agent_requested_by: me || 'anonymous' },
    );

  const recall = (j: Job) =>
    patchStep(j, {}, { agent_requested_at: null, agent_requested_by: null });

  // ---- findings ----------------------------------------------------
  //
  // What triage FOUND, as opposed to where it routed it. The board used
  // to record only that an agent had been ASKED, so a diagnosed item
  // and an untouched one looked identical — three items sat here for a
  // whole session with their causes known and their fixes shipped.
  //
  // Written to the same step metadata as the hand-off record, and in
  // the same shape an automatic agent would write later. That symmetry
  // is the point: the surface does not care whether a person or an
  // agent filled it in.

  let draft = $state<Record<string, string>>({});
  let editing = $state<Record<string, boolean>>({});

  function finding(j: Job): string | null {
    const v = forkStep(j)?.metadata?.['finding'];
    return typeof v === 'string' && v.trim() !== '' ? v : null;
  }
  function findingBy(j: Job): string | null {
    const v = forkStep(j)?.metadata?.['finding_by'];
    return typeof v === 'string' ? v : null;
  }

  async function saveFinding(j: Job): Promise<void> {
    const text = (draft[j.id] ?? '').trim();
    if (text === '') return;
    await patchStep(
      j,
      {},
      {
        finding: text,
        // Provenance: a finding is evidence, and evidence without an
        // author is a rumour.
        finding_by: me || 'anonymous',
        finding_at: new Date().toISOString(),
      },
    );
    editing = { ...editing, [j.id]: false };
  }

  function startEditing(j: Job): void {
    draft = { ...draft, [j.id]: finding(j) ?? '' };
    editing = { ...editing, [j.id]: true };
  }

  // ---- dragging ----------------------------------------------------
  //
  // Dragging a card is the same act as picking a route from the menu —
  // both complete the fork step with that disposition. The menu stays
  // because drag is unusable by keyboard and awkward with a screen
  // reader; removing it would make the board operable only by pointer.

  let dragging = $state<string | null>(null);
  let dragOver = $state<string | null>(null);

  /// Only untriaged cards lift. A completed fork step does not
  /// un-complete, so a routed card cannot be re-routed by dragging —
  /// offering a gesture that silently did nothing would be worse than
  /// offering none.
  /// No routes means nothing to drag TO, so the card does not lift and
  /// the buttons are the only path.
  const canDrag = (j: Job): boolean => routeIds.size > 0 && columnOf(j) === WAITING;

  function startDrag(e: DragEvent, j: Job): void {
    if (!canDrag(j)) return;
    dragging = j.id;
    e.dataTransfer?.setData('text/plain', j.id);
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  }

  function endDrag(): void {
    dragging = null;
    dragOver = null;
  }

  /// Only real routes accept a drop. `Closed` is a place cards END UP,
  /// not a decision anyone can make, so it must not advertise itself
  /// as a target and then quietly ignore the drop.
  const isDropTarget = (col: string): boolean => dragging !== null && routeIds.has(col);

  function onDragOver(e: DragEvent, col: string): void {
    if (!isDropTarget(col)) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
    dragOver = col;
  }

  function onDrop(e: DragEvent, col: string): void {
    e.preventDefault();
    const id = dragging ?? e.dataTransfer?.getData('text/plain');
    const wasTarget = isDropTarget(col);
    endDrag();
    if (!wasTarget) return;
    const j = jobs.find((x) => x.id === id);
    if (j) void route(j, col);
  }

  // The roster is decoration, not data the board depends on: it is
  // fetched alongside `load` rather than inside it, and a failure is
  // swallowed. formatActor falls back to the raw id, so the worst
  // case is the ids we were already showing — a board that refuses to
  // render because /api/people is down would be a strictly worse
  // trade than a card that says `emp-bootstrap-admin`.
  async function loadRoster() {
    try {
      const r = await fetch('/api/people');
      if (!r.ok) return;
      const roster = (await r.json()) as ReadonlyArray<{ id: string; name?: string }>;
      empNames = new Map(roster.map((e) => [e.id, e.name ?? '']));
    } catch {
      /* names stay ids */
    }
  }

  onMount(load);
  onMount(loadRoster);
</script>

<PageHeader {title} {subtitle} />

{#if loading}
  <p class="tb-msg">Loading…</p>
{:else if error}
  <p class="tb-msg tb-err load-failed">{error}</p>
{:else if jobs.length === 0}
  <p class="tb-msg">{emptyMessage}</p>
  {@render archiveNote()}
{:else}
  <div class="tb-board">
    {#each columns as col (col.id)}
      {@const cards = byColumn[col.id] ?? []}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <section
        class="tb-col"
        class:tb-col-over={dragOver === col.id && dragging !== null}
        aria-label={col.label}
        ondragover={(e) => onDragOver(e, col.id)}
        ondragleave={() => (dragOver = null)}
        ondrop={(e) => onDrop(e, col.id)}
      >
        <header class="tb-col-head">
          <h3>{col.label}</h3>
          <span class="tb-count">{cards.length}</span>
        </header>
        <p class="tb-col-hint">{col.hint}</p>

        {#if isDropTarget(col.id)}
          <p class="tb-drop-zone" class:tb-drop-zone-over={dragOver === col.id}>
            Route here
          </p>
        {:else if cards.length === 0}
          <p class="tb-col-empty">Nothing here.</p>
        {/if}

        {#each cards as j (j.id)}
          <article
            class="tb-card"
            class:tb-card-draggable={canDrag(j)}
            class:tb-card-dragging={dragging === j.id}
            draggable={canDrag(j)}
            ondragstart={(e) => startDrag(e, j)}
            ondragend={endDrag}
            ondblclick={() => (detailId = j.id)}
          >
            {#if card}
              {@render card(j)}
            {:else}
              <p class="tb-card-title">{j.title}</p>
            {/if}

            <div class="tb-card-meta">
              <span class="tb-by">{j.owner_id ? formatActor(j.owner_id, empNames) : 'unassigned'}</span>
            </div>

            <!-- The finding renders on EVERY card, routed or not. A
                 card whose cause is known should not look like an
                 untouched one, which is exactly how three diagnosed
                 items sat in "waiting" for a session. -->
            {#if finding(j) && !editing[j.id]}
              <div class="tb-finding">
                <p class="tb-finding-text">{finding(j)}</p>
                {#if findingBy(j)}
                  <span class="tb-finding-by">found by {findingBy(j)}</span>
                {/if}
              </div>
            {/if}

            {#if col.id === WAITING}
              {#if agentRequestedAt(j)}
                <p class="tb-agent">
                  With an agent{#if agentRequestedBy(j)} — {agentRequestedBy(j)}{/if}
                </p>
              {/if}

              {#if editing[j.id]}
                <label class="tb-sr" for={`finding-${j.id}`}>What did you find?</label>
                <textarea
                  id={`finding-${j.id}`}
                  class="tb-finding-input"
                  rows="3"
                  placeholder="What is actually causing this?"
                  bind:value={draft[j.id]}
                ></textarea>
                <div class="tb-actions">
                  <button
                    class="tb-btn tb-btn-primary"
                    type="button"
                    disabled={busy[j.id] || !(draft[j.id] ?? '').trim()}
                    onclick={() => saveFinding(j)}>Save finding</button
                  >
                  <button
                    class="tb-btn"
                    type="button"
                    onclick={() => (editing = { ...editing, [j.id]: false })}>Cancel</button
                  >
                </div>
              {/if}

              <div class="tb-actions">
                {#if fork}
                  <label class="tb-sr" for={`route-${j.id}`}>Route this item</label>
                  <select
                    id={`route-${j.id}`}
                    class="tb-select"
                    bind:value={choice[j.id]}
                    disabled={busy[j.id]}
                  >
                    <option value="" disabled selected>Route to…</option>
                    {#each fork.options as o (o.value)}
                      <option value={o.value}>{o.label}</option>
                    {/each}
                  </select>
                  <button
                    class="tb-btn tb-btn-primary"
                    type="button"
                    disabled={busy[j.id] || !choice[j.id]}
                    onclick={() => route(j, choice[j.id] ?? '')}>Route</button
                  >
                {:else}
                  <button
                    class="tb-btn tb-btn-primary"
                    type="button"
                    disabled={busy[j.id]}
                    onclick={() => route(j, 'done')}>Mark done</button
                  >
                {/if}
              </div>

              <div class="tb-actions">
                {#if agentRequestedAt(j)}
                  <button class="tb-btn" type="button" disabled={busy[j.id]} onclick={() => recall(j)}
                    >Take back</button
                  >
                {:else}
                  <button
                    class="tb-btn"
                    type="button"
                    disabled={busy[j.id]}
                    onclick={() => handToAgent(j)}>Hand to agent</button
                  >
                {/if}
                {#if !editing[j.id]}
                  <button class="tb-btn" type="button" onclick={() => startEditing(j)}
                    >{finding(j) ? 'Edit finding' : 'Record finding'}</button
                  >
                {/if}
              </div>
            {/if}
          </article>
        {/each}
      </section>
    {/each}
  </div>
  {@render archiveNote()}
{/if}

<!-- What the retention window is holding back, said out loud with a
     way through to it.

     The board is a working surface: everything live plus a recent tail
     of finished work. The rest is not gone, and a person who has just
     noticed the board is shorter than they remember should not have to
     ask where it went — that question is exactly what this window was
     built in response to. -->
{#snippet archiveNote()}
  {#if archived !== null && archived > 0}
    <p class="tb-note">
      Showing all live work plus {terminalWindowDays} days of finished items.
      <a href="/jobs?kind={encodeURIComponent(kind)}&status=closed">
        {archived} older {archived === 1 ? 'item' : 'items'}
      </a>
      have closed and aged off the board.
    </p>
  {/if}
{/snippet}

<!-- The shared packet panel (PacketModal, web-kit). This board used to
     carry its own copy — same header, facts list, steps and metadata
     dump — written before the component existed. Two definitions of
     "the condensed view of a packet" is the duplication CLAUDE.md 9a
     is about, so the local one is gone rather than kept beside it. -->
{#if detail}
  <PacketModal
    job={detail}
    onClose={() => (detailId = null)}
    formatActor={(id) => formatActor(id, empNames)}
  />
{/if}

<style>
  /* Columns scroll sideways rather than squeezing: a fork can declare
     six routes, and a 140px column is unreadable. The page body never
     scrolls horizontally — this container does. */
  .tb-board {
    display: grid;
    grid-auto-flow: column;
    grid-auto-columns: minmax(230px, 1fr);
    gap: 16px;
    align-items: start;
    overflow-x: auto;
    padding-bottom: 8px;
  }
  @media (max-width: 900px) {
    .tb-board {
      grid-auto-flow: row;
      grid-auto-columns: auto;
    }
  }
  .tb-col {
    display: flex;
    flex-direction: column;
    gap: 8px;
    background: var(--bg, #f5f5f4);
    border: 1px solid var(--border, #e7e5e4);
    border-radius: 8px;
    padding: 12px;
    /* A column must be a target worth aiming at even with nothing in
       it, or the only droppable columns are the ones that already
       have cards — which is backwards. */
    min-height: 160px;
  }
  .tb-col-over {
    border-color: #78716c;
    background: var(--card, #fff);
  }
  .tb-col-head {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .tb-col-head h3 {
    font-size: 13px;
    margin: 0;
  }
  .tb-count {
    font-size: 11px;
    color: var(--text-dim, #78716c);
    font-variant-numeric: tabular-nums;
  }
  .tb-col-hint {
    font-size: 11px;
    color: var(--text-dim, #a8a29e);
    margin: 0 0 4px;
  }
  .tb-col-empty {
    margin: 0;
    font-size: 11px;
    color: var(--text-dim, #a8a29e);
    font-style: italic;
  }
  .tb-drop-zone {
    margin: 0;
    padding: 14px 10px;
    border: 1px dashed var(--text-dim, #a8a29e);
    border-radius: 6px;
    text-align: center;
    font-size: 12px;
    color: var(--text-dim, #78716c);
  }
  .tb-drop-zone-over {
    border-color: #1c1917;
    border-style: solid;
    color: #1c1917;
    background: var(--card, #fff);
  }

  .tb-card {
    background: var(--card, #fff);
    border: 1px solid var(--border, #e7e5e4);
    border-radius: 6px;
    padding: 10px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .tb-card-draggable {
    cursor: grab;
  }
  .tb-card-draggable:active {
    cursor: grabbing;
  }
  .tb-card-dragging {
    opacity: 0.45;
  }
  @media (prefers-reduced-motion: reduce) {
    .tb-card-dragging {
      opacity: 1;
      outline: 2px dashed #78716c;
    }
  }
  .tb-card-title {
    margin: 0;
    font-size: 13px;
    line-height: 1.45;
  }
  .tb-card-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    font-size: 11px;
    color: var(--text-dim, #78716c);
  }
  .tb-by {
    margin-left: auto;
  }
  .tb-finding {
    border-left: 2px solid #0f766e;
    padding-left: 8px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .tb-finding-text {
    margin: 0;
    font-size: 12px;
    line-height: 1.45;
    white-space: pre-wrap;
  }
  .tb-finding-by {
    font-size: 10px;
    color: var(--text-dim, #a8a29e);
  }
  .tb-finding-input {
    font: inherit;
    font-size: 12px;
    padding: 6px;
    border: 1px solid var(--border, #e7e5e4);
    border-radius: 4px;
    resize: vertical;
    width: 100%;
    box-sizing: border-box;
  }
  .tb-agent {
    margin: 0;
    font-size: 11px;
    color: #b45309;
  }
  .tb-actions {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .tb-select {
    font: inherit;
    font-size: 12px;
    padding: 3px 6px;
    border-radius: 4px;
    border: 1px solid var(--border, #e7e5e4);
    background: var(--card, #fff);
    color: inherit;
    flex: 1 1 auto;
    min-width: 0;
  }
  .tb-btn {
    font: inherit;
    font-size: 12px;
    padding: 3px 8px;
    border-radius: 4px;
    border: 1px solid var(--border, #e7e5e4);
    background: var(--bg, #f5f5f4);
    color: inherit;
    cursor: pointer;
  }
  .tb-btn-primary {
    background: #1c1917;
    color: #fff;
    border-color: #1c1917;
  }
  .tb-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .tb-sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .tb-msg {
    color: var(--text-dim, #78716c);
    font-size: 14px;
  }
  .tb-err {
    color: #b91c1c;
  }
  /* A footnote, not a banner. It answers a question someone might
     have; it is not news. */
  .tb-note {
    margin: 12px 2px 0;
    color: var(--text-dim, #78716c);
    font-size: 12px;
  }
  .tb-note a {
    color: inherit;
  }
</style>
