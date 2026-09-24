<script lang="ts">
  // The train's landing report — what the conductor wrote into the
  // arrived step's metadata when the train reached the playground:
  // which cars it carried, which it left behind and why, and how long
  // each leg took.
  //
  // Data-keyed, not kind-keyed (CLAUDE.md §9): the panel renders
  // because an `arrival_report` object is on the record, not because
  // some branch says `kind === 'pr-train'`. A Job without one — every
  // train that landed before the conductor started writing reports —
  // renders nothing at all.
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { arrivalReport, itemAnswer, type WithSteps } from './yard';

  type Props = Readonly<{ job: WithSteps }>;
  let { job }: Props = $props();

  const report = $derived(arrivalReport(job));

  /** A leg duration, at the granularity that reads: 42s, 25m 00s, 1h 04m. */
  function dur(s: number | null): string {
    if (s === null) return '—';
    const total = Math.max(Math.round(s), 0);
    if (total < 60) return `${total}s`;
    const m = Math.floor(total / 60);
    const rest = total % 60;
    if (m < 60) return `${m}m ${String(rest).padStart(2, '0')}s`;
    return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, '0')}m`;
  }

  /** An instant, in the reader's own clock. Never invented: an absent
   *  stamp stays a dash. */
  function instant(at: string | null): string {
    if (at === null) return '—';
    const ms = Date.parse(at);
    if (Number.isNaN(ms)) return at;
    return new Date(ms).toLocaleString([], {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    });
  }
</script>

{#if report}
  {@const r = report}
  <Section title="Landing report" wide>
    <div class="ar-head">
      {#if r.generation !== null}
        <span class="ar-chip" title="conductor generation">gen {r.generation}</span>
      {/if}
      {#if r.merged_sha !== null}
        <span class="ar-chip" title="the merge commit that landed">{r.merged_sha}</span>
      {/if}
      <span class="ar-chip">{r.consist.length} cars</span>
    </div>

    <!-- The consist as a list, not packet cards: the report carries
         each car's SHORT id, and `/api/jobs/{id}` wants the full one —
         a card would offer a click that dead-ends. Same facts, no
         promise the data can't keep.

         The item answer rides each line (backlog 7f90c2ce). Drawing
         nothing made every car read as unlinked, which is a WRONG
         answer rather than an empty one: the gate refuses to launch
         without one of the three, so a blank line here means a car
         older than that refusal and nothing else. -->
    {#if r.consist.length > 0}
      <div class="ar-label">CONSIST</div>
      <ul class="ar-list">
        {#each r.consist as c, i (c.car_id_short ?? i)}
          {@const answer = itemAnswer(c)}
          <li>
            <span class="ar-id">{c.car_id_short ?? '—'}</span>
            <span class="ar-title">{c.title ?? '(untitled car)'}</span>
            {#if c.branch !== null}<span class="ar-branch">{c.branch}</span>{/if}
            {#if answer}<span class="ar-item ar-item-{answer.kind}">{answer.text}</span>{/if}
          </li>
        {/each}
      </ul>
    {/if}

    {#if r.left_behind.length > 0}
      <div class="ar-label">LEFT BEHIND</div>
      <ul class="ar-list">
        {#each r.left_behind as c, i (c.car_id_short ?? i)}
          <li>
            <span class="ar-id">{c.car_id_short ?? '—'}</span>
            <span class="ar-skip">{c.reason ?? '(no reason recorded)'}</span>
          </li>
        {/each}
      </ul>
    {/if}

    {#if r.timings}
      {@const t = r.timings}
      <div class="ar-label">TIMINGS</div>
      <div class="ar-timings">
        <div><span>boarded</span><span class="ar-num">{instant(t.boarded_at)}</span></div>
        <div><span>merged</span><span class="ar-num">{instant(t.merged_at)}</span></div>
        <div><span>deployed</span><span class="ar-num">{instant(t.deployed_at)}</span></div>
        <div><span>arrived</span><span class="ar-num">{instant(t.arrived_at)}</span></div>
        <div><span>board → merge</span><span class="ar-num">{dur(t.board_to_merge_s)}</span></div>
        <div><span>merge → deploy</span><span class="ar-num">{dur(t.merge_to_deploy_s)}</span></div>
        <div><span>total</span><span class="ar-num">{dur(t.total_s)}</span></div>
      </div>
    {/if}
  </Section>
{/if}

<style>
  .ar-head {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 10px;
  }
  .ar-chip {
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0.1em;
    font-variant-numeric: tabular-nums;
    color: var(--text-dim);
    border: 1px solid var(--border);
    padding: 2px 8px;
  }
  .ar-label {
    font-family: var(--font-mono);
    font-size: 10.5px;
    letter-spacing: var(--ls-nav);
    color: var(--text-dim);
    margin: 14px 0 6px;
  }
  .ar-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .ar-list li {
    display: flex;
    align-items: baseline;
    gap: 10px;
    min-width: 0;
    font-size: 13px;
  }
  .ar-id,
  .ar-branch {
    font-family: var(--font-mono);
    font-size: 11.5px;
    font-variant-numeric: tabular-nums;
    color: var(--text-dim);
    flex: none;
  }
  .ar-title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* The item answer: a closing link reads as an ordinary fact, one
     piece of an item reads as a link that does NOT close, and a
     stated no-item reason reads as prose because that is what it is. */
  .ar-item {
    font-family: var(--font-mono);
    font-size: 11.5px;
    font-variant-numeric: tabular-nums;
    color: var(--text-dim);
    flex: none;
    margin-left: auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 40%;
  }
  .ar-item-part {
    color: var(--accent);
  }
  .ar-item-none {
    font-family: inherit;
    font-size: 12.5px;
    font-style: italic;
  }
  .ar-skip {
    font-size: 12.5px;
    color: var(--warn);
  }
  /* The instrument face: DM Mono, tabular figures, so the columns of
     stamps and durations line up digit for digit. */
  .ar-timings {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(210px, 1fr));
    gap: 4px 20px;
  }
  .ar-timings div {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    border-bottom: 1px solid var(--border);
    padding: 3px 0;
    font-size: 12.5px;
    color: var(--text-dim);
  }
  .ar-num {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    color: var(--text);
  }
</style>
