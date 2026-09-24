<script lang="ts">
  // The correction marker — the ONE component that draws a step's
  // corrections, beside the step they correct (design 4105b020,
  // backlog 56727f95).
  //
  // A completed step is never rewritten, so a damaged sentence on one
  // stays exactly as it was stored; what changed is that the job GET
  // now hands each step the corrections that target it. This draws
  // them: each names the field it corrects, quotes what the field reads
  // and what it should read, and says who corrected it and when. The
  // original is NEVER replaced in the render — the reader sees both,
  // joined. A withdrawn correction is still drawn, struck through, with
  // the entry that withdrew it; the list only ever grows.
  //
  // Mounted once in StepSurface, above both sides of the plugin fork,
  // so every surface — platform, generic and plugin-backed alike — is
  // handed the same marker without learning to look for one. Absent
  // corrections draw nothing: an uncorrected step renders as before.
  import { stepCorrections } from './corrections';

  type Props = {
    step: { corrections?: unknown };
  };
  let { step }: Props = $props();

  let corrections = $derived(stepCorrections(step));
</script>

{#if corrections.length > 0}
  <div class="step-corrections" data-testid="step-corrections">
    <div class="sc-head">
      Corrected — the step is shown as it was recorded; each correction
      sits beside it
    </div>
    <ul>
      {#each corrections as c (c.index)}
        <li
          class="sc-entry"
          class:sc-withdrawn={c.kind === 'correction' && c.withdrawnBy !== null}
          data-testid={c.kind === 'correction' ? 'step-correction' : 'step-correction-withdrawal'}
        >
          <div class="sc-line">
            <span class="sc-index">[{c.index}]</span>
            <span class="sc-field">{c.field}</span>
            {#if c.kind === 'withdrawal'}
              <span class="sc-note">withdraws [{c.withdraws}]</span>
            {:else if c.withdrawnBy !== null}
              <span class="sc-note">withdrawn by [{c.withdrawnBy}]</span>
            {/if}
          </div>
          {#if c.kind === 'correction'}
            <div class="sc-row">
              <span class="sc-label">reads</span>
              <q class="sc-text">{c.reads}</q>
            </div>
            <div class="sc-row">
              <span class="sc-label">should read</span>
              <q class="sc-text">{c.shouldRead}</q>
            </div>
          {/if}
          {#if c.why}
            <div class="sc-row">
              <span class="sc-label">why</span>
              <span class="sc-text">{c.why}</span>
            </div>
          {/if}
          <div class="sc-signed">by {c.by} · {c.at}</div>
        </li>
      {/each}
    </ul>
  </div>
{/if}

<style>
  .step-corrections {
    border: 1px solid var(--border);
    border-left: 3px solid var(--warn);
    border-radius: 6px;
    background: var(--warn-wash);
    margin-bottom: 12px;
    padding: 8px 12px;
    font-size: 13px;
  }
  .sc-head {
    font-weight: 600;
    margin-bottom: 6px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .sc-line,
  .sc-row {
    display: flex;
    gap: 6px;
    align-items: baseline;
  }
  .sc-line {
    flex-wrap: wrap;
  }
  .sc-index,
  .sc-signed,
  .sc-note {
    color: var(--text-dim);
    font-size: 12px;
  }
  .sc-field {
    font-family: var(--font-mono);
    font-weight: 600;
  }
  .sc-label {
    flex: 0 0 7em;
    color: var(--text-dim);
  }
  /* The stored prose verbatim — its own line breaks kept, and a long
     unbroken token (an id, a path) wrapped rather than overflowing. */
  .sc-text {
    min-width: 0;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
  .sc-withdrawn q {
    text-decoration: line-through;
  }
</style>
