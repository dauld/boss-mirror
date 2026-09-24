<script lang="ts">
  // Per-account drill-down — list of entries touching this account
  // with an expandable detail view per entry. Used by TrialBalanceTab.

  import Section from '@boss/web-kit/ui/Section.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import { type EntityKind } from '@boss/web-kit/ui/entity-href';
  import {
    loadEntriesForAccount,
    ENTRIES_PER_ACCOUNT_CAP,
    type LedgerEntry,
  } from './ledger';
  import EntryDetail from './EntryDetail.svelte';
  import { listView, okRead, type ReadState } from '../data/readState';

  type Props = {
    accountCode: string;
    accountName: string;
    selectedEntryId: string | null;
    onSelectEntry: (id: string | null) => void;
    factSourceKind: (sourceTable: string) => EntityKind | null;
  };
  let { accountCode, accountName, selectedEntryId, onSelectEntry, factSourceKind }: Props = $props();

  let entries = $state<ReadonlyArray<LedgerEntry>>([]);
  let capped = $state(false);
  let entriesRead = $state<ReadState>(okRead);
  let loading = $state(true);
  /// A failed entries read is checked before the row count, so an
  /// outage cannot paint "No entries for this account." (backlog
  /// 1a2b67c9).
  let entriesView = $derived(
    listView(
      [{ source: `ledger entries for ${accountCode}`, state: entriesRead }],
      entries.length,
    ),
  );

  $effect(() => {
    const code = accountCode;
    let cancelled = false;
    loading = true;
    (async () => {
      const page = await loadEntriesForAccount(code);
      if (!cancelled) {
        entries = page.data;
        capped = page.capped;
        entriesRead = page.read;
        loading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function toggleEntry(id: string): void {
    onSelectEntry(selectedEntryId === id ? null : id);
  }
</script>

<Section title={`Entries touching ${accountCode} — ${accountName}`}>
    {#if capped}
      <div class="entries-cap-note" role="status">
        Showing the most recent <strong>{ENTRIES_PER_ACCOUNT_CAP.toLocaleString()}</strong> entries for this account — older entries exist beyond this window.
      </div>
    {/if}
    {#if loading}
      <p class="empty">Loading entries…</p>
    {:else if entriesView.kind === 'failed'}
      <p class="empty load-failed" role="alert">
        Couldn't load {entriesView.source} — {entriesView.error}
      </p>
    {:else if entriesView.kind === 'empty'}
      <p class="empty">No entries for this account.</p>
    {:else}
      <table class="tb-entries">
        <thead>
          <tr>
            <th>Posted</th>
            <th>Memo</th>
            <th>Fact kind</th>
            <th>Source</th>
            <th class="c">Rule</th>
          </tr>
        </thead>
        <tbody>
          {#each entries as e (e.id)}
            <tr
              class={selectedEntryId === e.id ? 'tb-row selected' : 'tb-row'}
              role="button"
              tabindex="0"
              onclick={() => toggleEntry(e.id)}
              onkeydown={(evt) => {
                if (evt.key === 'Enter' || evt.key === ' ') {
                  evt.preventDefault();
                  toggleEntry(e.id);
                }
              }}
            >
              <td class="mono">{e.posted_on}</td>
              <td>{e.memo ?? ''}</td>
              <td class="mono small">{e.fact_kind}</td>
              <td class="mono small">
                {#if e.fact_source_table && e.fact_source_id}
                  {@const kind = factSourceKind(e.fact_source_table)}
                  {#if kind}
                    <EntityLink {kind} id={e.fact_source_id} />
                  {:else}
                    <span>{e.fact_source_table} · {e.fact_source_id}</span>
                  {/if}
                {:else}
                  <span class="empty">—</span>
                {/if}
              </td>
              <td class="c">v{e.rule_version}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    {#if selectedEntryId}
      <EntryDetail entryId={selectedEntryId} {factSourceKind} />
    {/if}
</Section>

<style>
  .entries-cap-note {
    padding: 8px 12px;
    background: var(--warn-wash);
    border: 1px solid var(--busy);
    border-radius: 6px;
    font-size: 13px;
    color: var(--warn);
    margin: 0 0 8px 0;
  }
</style>
