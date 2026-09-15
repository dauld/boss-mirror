<script lang="ts">
  // One adrift kind's approve — the control that files the
  // publish-workflow ops-request and shows what the host answered
  // (car 3b of 8f4e9cc0). The decisions are approve.ts's, tested; this
  // renders them: which mode the kind is in, whether the viewer is
  // admitted, the packet link once filed, and the answer — exit code
  // and the verb's FULL output, never a tail (§Diagnosis) — when the
  // runner closes it.
  //
  // The force path is deliberately two clicks apart from the plain
  // one: it appears only after the verb itself refused with exit 6
  // (the live row carries what the tree never said), and its button
  // enables only when the approver has typed the name of the field
  // that will be erased. What they typed is what the record says.
  import WriteGate from '@boss/web-kit/ui/WriteGate.svelte';
  import {
    approveAuthority,
    approveBody,
    fileApprove,
    forceConfirmed,
    modeFor,
    PUBLISH_HOST,
    type AdriftKind,
    type ApproveAgainst,
    type PublishRequest,
  } from './approve';

  type Props = Readonly<{
    kind: AdriftKind;
    against: ApproveAgainst;
    /** This kind's latest publish-workflow request, or null. */
    latest: PublishRequest | null;
    /** The role the ops-request `execute` step names; null = none. */
    authorityRole: string | null;
    viewerId: string | null;
    viewerRole: string | null;
    /** Called with the new packet id once the POST returns one. */
    onFiled: (id: string) => void;
  }>;
  let { kind, against, latest, authorityRole, viewerId, viewerRole, onFiled }: Props = $props();

  const mode = $derived(modeFor(latest));
  const authority = $derived(approveAuthority(authorityRole, viewerRole));
  const refusal = $derived(authority.kind === 'refused' ? authority.why : null);
  const fieldNames = $derived(kind.fields.map((f) => f.field));

  let typed = $state('');
  let filing = $state(false);
  let error = $state<string | null>(null);

  async function file(which: 'plain' | 'force'): Promise<void> {
    if (filing || viewerId === null) return;
    filing = true;
    error = null;
    try {
      const id = await fileApprove(approveBody(kind, which, against, viewerId));
      typed = '';
      onFiled(id);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      filing = false;
    }
  }

  const when = (iso: string | null): string => (iso ? `${iso.slice(0, 10)} ${iso.slice(11, 16)}Z` : 'unstamped');
</script>

<div class="ap-kind">
  <div class="ap-head">
    <span class="mono">{kind.kind}</span>
    <small>
      {kind.live_version === null ? 'live version unrecorded' : `live v${kind.live_version}`} ·
      adrift: {fieldNames.join(', ')}
    </small>
  </div>

  {#if latest}
    <div class="ap-answer">
      <div class="ap-answer-head">
        <a href={`/jobs/${latest.id}`} class="mono">{latest.title}</a>
        <small>filed {when(latest.opened_at)}{latest.requested_from ? ` from ${latest.requested_from}` : ''}</small>
        {#if latest.status === 'open'}
          <span class="ap-pending">in flight — the runner on {PUBLISH_HOST} polls about once a minute</span>
        {:else if latest.disposition === 'answered'}
          <span class={latest.exit_code === '0' ? 'ok' : 'warn'}>
            answered · exit {latest.exit_code ?? '?'}{latest.runner_host ? ` on ${latest.runner_host}` : ''}
          </span>
        {:else if latest.disposition === 'refused'}
          <span class="warn">refused by the runner — outside the allowlist</span>
        {:else}
          <span class="warn">closed without an answer on its execute step</span>
        {/if}
      </div>
      {#if latest.output !== ''}
        <pre class="ap-output">{latest.output}</pre>
      {/if}
    </div>
  {/if}

  <WriteGate>
    <div class="ap-controls">
      {#if mode.kind === 'pending'}
        <button type="button" class="btn" disabled title="a request for this kind is open; a second would be a twin">
          Approve: publish the tree's row — waiting on the answer above
        </button>
      {:else if mode.kind === 'force'}
        <p class="ap-force-why">
          The verb refused (exit 6): the live row carries what the tree never said, so publishing the tree's row
          would <strong>erase the live {fieldNames.join(', ')}</strong>
          {kind.live_version === null ? '' : ` of v${kind.live_version}`} — the output above names it. To publish anyway,
          type the field name{fieldNames.length === 1 ? '' : 's'} that will be erased:
        </p>
        <div class="ap-force-row">
          <input
            type="text"
            class="ap-confirm"
            placeholder={fieldNames.join(', ')}
            bind:value={typed}
            disabled={refusal !== null || filing}
            aria-label="the field that will be erased"
          />
          <button
            type="button"
            class="btn btn-danger-outline"
            disabled={refusal !== null || filing || !forceConfirmed(typed, fieldNames)}
            title={refusal ?? `files publish-workflow ${kind.kind} --force-tree on ${PUBLISH_HOST}`}
            onclick={() => void file('force')}
          >
            Publish the tree's row anyway (--force-tree), erasing {fieldNames.join(', ')}
          </button>
          <button
            type="button"
            class="btn"
            disabled={refusal !== null || filing}
            title={refusal ?? 'file the plain request again — the tree may have caught up since'}
            onclick={() => void file('plain')}
          >
            Ask the verb again
          </button>
        </div>
      {:else}
        <button
          type="button"
          class="btn"
          disabled={refusal !== null || filing}
          title={refusal ?? `files publish-workflow ${kind.kind} on ${PUBLISH_HOST}; the verb's own refusals run first`}
          onclick={() => void file('plain')}
        >
          Approve: publish the tree's row
        </button>
      {/if}
      {#if refusal}
        <small class="ap-refusal">{refusal}</small>
      {/if}
      {#if error}
        <p class="load-failed ap-error">The request was not filed: {error}</p>
      {/if}
    </div>
  </WriteGate>
</div>

<style>
  .ap-kind { border: 1px solid var(--hairline, #2a3138); padding: 10px 12px; margin-bottom: 10px; }
  .ap-head { display: flex; gap: 12px; align-items: baseline; flex-wrap: wrap; font-size: 13px; }
  .ap-head small { color: var(--static, #7a838c); font-size: 12px; }
  .mono { font-family: var(--font-mono, ui-monospace, monospace); font-variant-numeric: tabular-nums; }
  .ok { color: var(--ok, #4fb98a); }
  .warn { color: var(--warn, #d9a441); }
  .ap-answer { margin-top: 8px; font-size: 12px; }
  .ap-answer-head { display: flex; gap: 10px; align-items: baseline; flex-wrap: wrap; }
  .ap-answer-head small { color: var(--static, #7a838c); }
  .ap-pending { color: var(--static, #7a838c); font-style: italic; }
  /* The whole output, scrolled, never cut: the refusal names the field
     by field and the confirmation is read from it. */
  .ap-output {
    margin: 6px 0 0; padding: 8px; max-height: 240px; overflow: auto;
    font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    white-space: pre-wrap; overflow-wrap: anywhere;
    border: 1px solid var(--hairline, #2a3138); color: var(--fog, #e8ecef);
  }
  .ap-controls { margin-top: 8px; display: flex; flex-direction: column; gap: 6px; }
  .ap-force-why { margin: 0; font-size: 12px; max-width: 90ch; color: var(--warn, #d9a441); }
  .ap-force-row { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; }
  .ap-confirm {
    font-family: var(--font-mono, ui-monospace, monospace); font-size: 12px; padding: 4px 6px;
    background: transparent; color: var(--fog, #e8ecef); border: 1px solid var(--hairline, #2a3138); min-width: 18ch;
  }
  .ap-refusal { color: var(--static, #7a838c); font-size: 11px; }
  .ap-error { color: var(--warn, #d9a441); font-size: 12px; margin: 0; }
</style>
