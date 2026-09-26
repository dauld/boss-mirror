<script lang="ts">
  // Ledger entry detail — lines + fact payload. Used by the trial-
  // balance drill-down.

  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import { type EntityKind } from '@boss/web-kit/ui/entity-href';
  import {
    formatUsd,
    loadEntryDetail,
    reverseEntry,
    type LedgerEntryDetail,
  } from './ledger';
  import { shortId } from '../data/ids';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { href } from '../router';
  import { entrySearch } from './financeQuery';
  import { session } from '@boss/web-kit/session/session.svelte';

  type Props = {
    entryId: string;
    factSourceKind: (sourceTable: string) => EntityKind | null;
  };
  let { entryId, factSourceKind }: Props = $props();

  let entry = $state<LedgerEntryDetail | null>(null);
  let loading = $state(true);

  $effect(() => {
    const id = entryId;
    let cancelled = false;
    loading = true;
    (async () => {
      const d = await loadEntryDetail(id);
      if (!cancelled) {
        entry = d;
        loading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  // Reverse-entry state
  type ReverseState =
    | { kind: 'idle' }
    | { kind: 'posting' }
    | { kind: 'posted'; reversalId: string }
    | { kind: 'error'; message: string };
  let reverseState = $state<ReverseState>({ kind: 'idle' });

  let readOnly = $derived(
    session.value.kind === 'ready' && session.value.user.role === 'auditor',
  );

  async function doReverse(e: LedgerEntryDetail): Promise<void> {
    if (
      !window.confirm(
        `Post a reversing entry for ${shortId(e.id)}…?\n\nEvery debit becomes a credit and vice versa, dated today.`,
      )
    )
      return;
    reverseState = { kind: 'posting' };
    try {
      const resp = await reverseEntry(e);
      reverseState = { kind: 'posted', reversalId: resp.entry_id };
    } catch (err) {
      reverseState = { kind: 'error', message: String(err) };
    }
  }
</script>

{#if loading && !entry}
  <p class="empty">Loading entry…</p>
{:else if !entry}
  <p class="empty load-failed" role="alert">Entry unavailable.</p>
{:else}
  {@const e = entry}
  <div class="tb-entry-detail">
    <h4>
      Entry {shortId(e.id)}… &middot; posted {e.posted_on}
      <span class="ruleset-badge">RuleSet v{e.rule_version}</span>
      {#if !readOnly}
        {#if reverseState.kind === 'posted'}
          <span class="tb-reverse-result">
            Reversal posted: <Link
              to={href(`/ux/finance${entrySearch(reverseState.reversalId)}`)}
              className="mono">{shortId(reverseState.reversalId)}</Link>
          </span>
        {:else}
          <button
            class="tb-reverse-btn"
            onclick={() => doReverse(e)}
            disabled={reverseState.kind === 'posting'}
            title="Post a reversing journal entry dated today"
          >
            {reverseState.kind === 'posting' ? 'Posting…' : 'Reverse this entry'}
          </button>
          {#if reverseState.kind === 'error'}
            <span class="tb-reverse-error">{reverseState.message}</span>
          {/if}
        {/if}
      {/if}
    </h4>
    <table class="tb-entry-lines">
      <thead>
        <tr>
          <th class="c">Code</th>
          <th>Account</th>
          <th class="r">Debit</th>
          <th class="r">Credit</th>
        </tr>
      </thead>
      <tbody>
        {#each e.lines as l, i (i)}
          <tr>
            <td class="c mono">{l.account_code}</td>
            <td>{l.account_name}</td>
            <td class="r mono">{l.debit_cents > 0 ? formatUsd(l.debit_cents) : ''}</td>
            <td class="r mono">{l.credit_cents > 0 ? formatUsd(l.credit_cents) : ''}</td>
          </tr>
        {/each}
      </tbody>
    </table>
    <details class="tb-fact-payload">
      <summary>
        Fact: <code>{e.fact_kind}</code>
        {#if e.fact_source_table && e.fact_source_id}
          {@const kind = factSourceKind(e.fact_source_table)}
          {' '}&middot;{' '}
          {#if kind}
            <EntityLink {kind} id={e.fact_source_id} />
          {:else}
            <span>{e.fact_source_table} · {e.fact_source_id}</span>
          {/if}
        {/if}
      </summary>
      <pre>{JSON.stringify(e.fact_payload, null, 2)}</pre>
    </details>
  </div>
{/if}
