<script lang="ts">
  // The condensed packet view (David, 2026-08-14, feedback fc67bed2):
  // "Cards link to the job detail. I want a modal to pop with a nice,
  // condensed UX."
  //
  // A card carries what fits on a card. The job page carries
  // everything, behind a route change that loses the queue you were
  // reading. This is the middle: enough to decide whether a packet
  // needs you, without leaving the lens.
  //
  // It lives in web-kit rather than in the yard because PacketCard
  // already made the argument — one visual for a packet anywhere a
  // queue renders. A second modal per surface is how the card grammar
  // would come apart.
  //
  // Deliberately NOT per-protocol. David wondered whether each
  // protocol needs its own modal view; every field below comes off the
  // packet envelope, which every protocol has, and the steps are the
  // program counter, which is the one thing that differs and already
  // renders generically. A per-protocol surface is a StepPlugin's job,
  // one level down. If a protocol genuinely needs its own summary,
  // that is a plugin, not a branch in here.
  import { entityHref } from './entity-href';
  import Link from './Link.svelte';

  export type PacketStep = Readonly<{
    id: string;
    title: string;
    status: string;
    assignee_id?: string | null;
  }>;

  export type PacketJob = Readonly<{
    id: string;
    kind: string;
    title: string;
    status: string;
    opened_on?: string;
    closed_on?: string | null;
    tags?: readonly string[];
    owner_id?: string | null;
    subject?: { subject_kind?: string; id?: string } | null;
    metadata?: Record<string, unknown> | null;
    steps?: readonly PacketStep[];
  }>;

  type Props = Readonly<{
    job: PacketJob | null;
    /** Set while the fetch is in flight, so the panel opens instantly
     *  on click and fills in — a modal that waits for a round trip
     *  before appearing reads as a dropped click. */
    loading?: boolean;
    error?: string | null;
    onClose: () => void;
    /** Resolve an actor id to a display name. Identity by default:
     *  web-kit has no employee directory, and the yard is guest-safe. */
    formatActor?: (id: string) => string;
  }>;
  let { job, loading = false, error = null, onClose, formatActor = (id: string) => id }: Props =
    $props();

  // Escape-to-close via $effect rather than a svelte:window tag: the
  // bun+svelte bundler crashes on that event lookup and takes the
  // whole app down, not just this component. Documented in
  // DebugGear.svelte, repeated in TriageBoard.svelte, and covered by
  // no-svelte-window.test.ts (CLAUDE.md 9a).
  //
  // Write that tag name WITHOUT its angle bracket, as above and as
  // TriageBoard does. The test greps raw source and cannot tell a
  // mention from a use, so spelling it out in full fails the gate —
  // which this comment did, on its first run.
  $effect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === 'Escape') onClose();
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  });

  // `message` renders as prose above; showing it again inside the JSON
  // dump would be the same text twice in one panel.
  const extraMeta = $derived(
    Object.entries(job?.metadata ?? {}).filter(([k]) => k !== 'message'),
  );
  const message = $derived(
    typeof job?.metadata?.message === 'string' ? (job.metadata.message as string) : null,
  );
</script>

<!-- Backdrop. Escape closes it too, so this click target is a
     convenience rather than the only way out, which is what keeps the
     modal reachable without a mouse. -->
<div
  class="pm-back"
  role="presentation"
  onclick={(e) => {
    // Only a click on the backdrop itself closes. Testing the target
    // rather than stopping propagation on the panel means the panel
    // carries no click handler at all, so it needs no keyboard
    // equivalent to match it.
    if (e.target === e.currentTarget) onClose();
  }}
>
  <div class="pm" role="dialog" aria-modal="true" aria-label={job?.title ?? 'Packet'}>
    <header class="pm-head">
      <div class="pm-head-text">
        <h2 class="pm-title">{job?.title ?? (loading ? 'Loading…' : 'Packet')}</h2>
        {#if job}
          <p class="pm-sub">
            {job.kind} · {job.status}
            {#if job.opened_on}· opened {job.opened_on}{/if}
            {#if job.closed_on}· closed {job.closed_on}{/if}
          </p>
        {/if}
      </div>
      <button class="pm-btn" type="button" onclick={onClose} aria-label="Close">Close</button>
    </header>

    {#if error}
      <p class="pm-error">{error}</p>
    {:else if loading && !job}
      <p class="pm-quiet">Loading the packet…</p>
    {:else if job}
      {#if message}
        <p class="pm-message">{message}</p>
      {/if}

      <dl class="pm-facts">
        <dt>Job</dt>
        <dd class="pm-mono">{job.id}</dd>
        {#if job.subject}
          <dt>Subject</dt>
          <dd class="pm-mono">{job.subject.subject_kind ?? '?'} / {job.subject.id ?? '?'}</dd>
        {/if}
        <dt>Owner</dt>
        <dd>{job.owner_id ? formatActor(job.owner_id) : 'unassigned'}</dd>
        {#if job.tags?.length}
          <dt>Tags</dt>
          <dd>{job.tags.join(', ')}</dd>
        {/if}
      </dl>

      <!-- Steps are the program counter, so this is where the packet
           actually is — the one thing a card cannot show. -->
      {#if job.steps?.length}
        <h3 class="pm-h">Steps</h3>
        <ol class="pm-steps">
          {#each job.steps as s (s.id)}
            <li class="pm-step">
              <span class="pm-step-status step-status step-status-{s.status}">{s.status}</span>
              <span class="pm-step-title">{s.title}</span>
              {#if s.assignee_id}<span class="pm-step-who">{formatActor(s.assignee_id)}</span>{/if}
            </li>
          {/each}
        </ol>
      {/if}

      {#if extraMeta.length}
        <h3 class="pm-h">Metadata</h3>
        <pre class="pm-json">{JSON.stringify(Object.fromEntries(extraMeta), null, 2)}</pre>
      {/if}

      <!-- The full job page stays one click away. The modal is for
           deciding; the page is for working. -->
      <footer class="pm-foot">
        <Link to={entityHref('job', job.id)}>Open the full job</Link>
      </footer>
    {/if}
  </div>
</div>

<style>
  .pm-back {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding: 48px 16px;
    z-index: 50;
    overflow-y: auto;
  }
  .pm {
    background: var(--card);
    border: 1px solid var(--hairline);
    border-radius: 4px;
    width: min(640px, 100%);
    padding: 20px 24px 16px;
  }
  .pm-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
  }
  .pm-head-text {
    min-width: 0;
  }
  .pm-title {
    margin: 0;
    font-size: 1.05rem;
    line-height: 1.35;
    color: var(--fog);
  }
  .pm-sub {
    margin: 4px 0 0;
    font-size: 0.8rem;
    color: var(--fog);
  }
  .pm-btn {
    flex: none;
    background: transparent;
    border: 1px solid var(--hairline);
    color: var(--fog);
    padding: 4px 10px;
    cursor: pointer;
    font: inherit;
    font-size: 0.8rem;
  }
  .pm-btn:hover {
    color: var(--fog);
  }
  /* Reading line-height: the message is prose, and prose set at UI
     line-height is the FOG problem the readability sweep fixed. */
  .pm-message {
    margin: 16px 0 0;
    white-space: pre-wrap;
    line-height: 1.55;
    color: var(--fog);
  }
  .pm-facts {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 4px 16px;
    margin: 16px 0 0;
    font-size: 0.85rem;
  }
  .pm-facts dt {
    color: var(--fog);
  }
  .pm-facts dd {
    margin: 0;
    color: var(--fog);
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .pm-mono {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.8rem;
  }
  .pm-h {
    margin: 20px 0 8px;
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fog);
  }
  .pm-steps {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 6px;
  }
  .pm-step {
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-size: 0.85rem;
  }
  /* The status is the step's plate (.step-status-<s> in apps/web's
     styles.css, "States are plates" — backlog 7eb59678 car 2); this rule
     only sets the column, so the plates stand in one aligned row of signs
     down the list. It used to paint ready and active the same green. */
  .pm-step-status {
    flex: none;
    min-width: 88px;
    text-align: center;
  }
  .pm-step-title {
    color: var(--fog);
    min-width: 0;
  }
  .pm-step-who {
    color: var(--fog);
    font-size: 0.78rem;
  }
  /* Wide content scrolls inside its own box; the panel never scrolls
     sideways. */
  .pm-json {
    margin: 0;
    padding: 10px 12px;
    background: var(--ink);
    border: 1px solid var(--hairline);
    font-size: 0.75rem;
    line-height: 1.5;
    overflow-x: auto;
    color: var(--fog);
  }
  .pm-foot {
    margin-top: 18px;
    padding-top: 12px;
    border-top: 1px solid var(--hairline);
    font-size: 0.85rem;
  }
  .pm-quiet {
    margin: 16px 0 0;
    color: var(--fog);
    font-size: 0.85rem;
  }
  .pm-error {
    margin: 16px 0 0;
    color: var(--err);
    font-size: 0.85rem;
  }
</style>
