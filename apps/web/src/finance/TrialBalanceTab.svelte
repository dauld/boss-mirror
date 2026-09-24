<script lang="ts">
  // Trial balance — port of apps/web/src/finance/TrialBalanceTab.tsx.
  //
  // Three nested tables: trial balance (by account), drill-down (entries
  // touching a selected account), entry detail (lines + fact payload).
  // Plus the Periods panel with lock/unlock buttons.

  import Section from '@boss/web-kit/ui/Section.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import { type EntityKind } from '@boss/web-kit/ui/entity-href';
  import {
    centsToDollars,
    dateStamp,
    exportRows,
    printReport,
    type CsvColumn,
  } from './csvExport';
  import { shortId } from '../data/ids';
  import {
    formatUsd,
    lockPeriod,
    loadEntriesForAccount,
    loadEntryDetail,
    loadPeriods,
    loadTrialBalance,
    reverseEntry,
    unlockPeriod,
    type LedgerEntry,
    type LedgerEntryDetail,
    type Period,
    type TrialBalanceResponse,
    type TrialBalanceRow,
  } from './ledger';
  import { session } from '@boss/web-kit/session/session.svelte';
  import { listView, okRead, type ReadState } from '../data/readState';
  import AccountDrillDown from './AccountDrillDown.svelte';

  let asOf = $state('');
  let tb = $state<TrialBalanceResponse | null>(null);
  let tbLoading = $state(true);
  let periods = $state<ReadonlyArray<Period>>([]);
  let periodsRead = $state<ReadState>(okRead);
  let periodsLoading = $state(true);
  /// A failed periods read is checked before the row count, so an
  /// outage cannot paint "No periods yet." (backlog 1a2b67c9).
  let periodsView = $derived(
    listView([{ source: 'ledger periods', state: periodsRead }], periods.length),
  );
  let selectedAccount = $state<string | null>(null);
  let selectedEntryId = $state<string | null>(null);
  let tbTick = $state(0);
  let periodsTick = $state(0);

  let readOnly = $derived(
    session.value.kind === 'ready' && session.value.user.role === 'auditor',
  );

  $effect(() => {
    const a = asOf;
    void tbTick;
    let cancelled = false;
    tbLoading = true;
    (async () => {
      const d = await loadTrialBalance(a || null);
      if (!cancelled) {
        tb = d;
        tbLoading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  $effect(() => {
    void periodsTick;
    let cancelled = false;
    periodsLoading = true;
    (async () => {
      const res = await loadPeriods();
      if (!cancelled) {
        periods = res.periods;
        periodsRead = res.read;
        periodsLoading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function exportCsv(t: TrialBalanceResponse, a: string): void {
    const columns: ReadonlyArray<CsvColumn<TrialBalanceRow>> = [
      { header: 'Account code', value: (r) => r.account_code },
      { header: 'Account name', value: (r) => r.account_name },
      { header: 'Kind', value: (r) => r.kind },
      { header: 'Normal side', value: (r) => r.normal_side },
      { header: 'Debits', value: (r) => centsToDollars(r.debit_total_cents) },
      { header: 'Credits', value: (r) => centsToDollars(r.credit_total_cents) },
      { header: 'Balance', value: (r) => centsToDollars(r.balance_cents) },
      { header: 'Currency', value: (r) => r.currency },
    ];
    const filename = `trial-balance-${dateStamp(a || t.as_of)}.csv`;
    exportRows(filename, t.rows, columns);
  }

  function toggleAccount(code: string): void {
    selectedAccount = selectedAccount === code ? null : code;
    selectedEntryId = null;
  }

  function refreshAll(): void {
    periodsTick += 1;
    tbTick += 1;
  }

  // --- Period row mutations ---

  let periodBusy = $state<Record<string, boolean>>({});
  let periodError = $state<Record<string, string | null>>({});

  async function doLock(p: Period): Promise<void> {
    periodBusy = { ...periodBusy, [p.id]: true };
    periodError = { ...periodError, [p.id]: null };
    try {
      await lockPeriod(p.id, 'operator');
      refreshAll();
    } catch (e) {
      periodError = { ...periodError, [p.id]: String(e) };
    } finally {
      periodBusy = { ...periodBusy, [p.id]: false };
    }
  }

  async function doUnlock(p: Period): Promise<void> {
    const ok = window.confirm(
      `Unlock ${p.starts_on}? Clears the lock + checksum and allows new entries in this period again.`,
    );
    if (!ok) return;
    periodBusy = { ...periodBusy, [p.id]: true };
    periodError = { ...periodError, [p.id]: null };
    try {
      await unlockPeriod(p.id);
      refreshAll();
    } catch (e) {
      periodError = { ...periodError, [p.id]: String(e) };
    } finally {
      periodBusy = { ...periodBusy, [p.id]: false };
    }
  }

  function shortChecksum(p: Period): string {
    return p.locked_checksum
      ? p.locked_checksum.replace('sha256:', '').slice(0, 12)
      : '';
  }

  function factSourceKind(sourceTable: string): EntityKind | null {
    switch (sourceTable) {
      case 'invoices':
        return 'invoice';
      default:
        return null;
    }
  }
</script>

<div class="trial-balance-tab finance-print-area">
  <Section title="Trial balance">
      <div class="tb-controls">
        <label class="tb-asof">
          As of
          <input
            type="date"
            bind:value={asOf}
            onchange={() => {
              selectedAccount = null;
              selectedEntryId = null;
            }}
          />
        </label>
        <button
          type="button"
          class="secondary"
          disabled={!tb}
          onclick={() => tb && exportCsv(tb, asOf)}
          title="Download the visible trial balance as a CSV"
        >
          Download CSV
        </button>
        <button
          type="button"
          class="secondary"
          disabled={!tb}
          onclick={printReport}
          title="Print / save as PDF via the browser's print dialog"
        >
          Print / PDF
        </button>
      </div>
      {#if tbLoading && !tb}
        <p class="empty">Loading trial balance…</p>
      {:else if !tb}
        <p class="empty load-failed" role="alert">Ledger unavailable.</p>
      {:else}
        {@const visibleRows = tb.rows.filter((r) => r.debit_total_cents > 0 || r.credit_total_cents > 0)}
        <table class="tb-table">
          <thead>
            <tr>
              <th class="c">Code</th>
              <th>Name</th>
              <th class="c">Kind</th>
              <th class="r">Debits</th>
              <th class="r">Credits</th>
              <th class="r">Balance</th>
            </tr>
          </thead>
          <tbody>
            {#each visibleRows as r (r.account_code)}
              <tr
                class={selectedAccount === r.account_code ? 'tb-row selected' : 'tb-row'}
                role="button"
                tabindex="0"
                onclick={() => toggleAccount(r.account_code)}
                onkeydown={(e) => {
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    toggleAccount(r.account_code);
                  }
                }}
              >
                <td class="c mono">{r.account_code}</td>
                <td>{r.account_name}</td>
                <td class="c">{r.kind}</td>
                <td class="r mono">
                  {r.debit_total_cents > 0 ? formatUsd(r.debit_total_cents) : ''}
                </td>
                <td class="r mono">
                  {r.credit_total_cents > 0 ? formatUsd(r.credit_total_cents) : ''}
                </td>
                <td class="r mono">{formatUsd(r.balance_cents)}</td>
              </tr>
            {/each}
          </tbody>
          <tfoot>
            <tr>
              <td colspan="3"><strong>Totals</strong></td>
              <td class="r mono"><strong>{formatUsd(tb.total_debits_cents)}</strong></td>
              <td class="r mono"><strong>{formatUsd(tb.total_credits_cents)}</strong></td>
              <td class="r">
                {#if tb.balanced}
                  <span class="tb-balanced">BALANCED</span>
                {:else}
                  <span class="tb-unbalanced">MISMATCH</span>
                {/if}
              </td>
            </tr>
          </tfoot>
        </table>
      {/if}
  </Section>

  {#if selectedAccount}
    {@const accountName = tb?.rows.find((r) => r.account_code === selectedAccount)?.account_name ?? ''}
    {#key selectedAccount}
      <AccountDrillDown
        accountCode={selectedAccount}
        {accountName}
        {selectedEntryId}
        onSelectEntry={(id) => (selectedEntryId = id)}
        {factSourceKind}
      />
    {/key}
  {/if}

  <Section title="Periods">
      <div class="tb-controls">
        <button type="button" class="secondary" onclick={refreshAll}>Refresh</button>
      </div>
      {#if periodsLoading}
        <p class="empty">Loading periods…</p>
      {:else if periodsView.kind === 'failed'}
        <p class="empty load-failed" role="alert">
          Couldn't load {periodsView.source} — {periodsView.error}
        </p>
      {:else if periodsView.kind === 'empty'}
        <p class="empty">No periods yet.</p>
      {:else}
        <table class="tb-periods">
          <thead>
            <tr>
              <th>Period</th>
              <th class="c">Status</th>
              <th class="r">Entries</th>
              <th class="r">Debits</th>
              <th class="r">Credits</th>
              <th>Locked by</th>
              <th>Checksum</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each periods as p (p.id)}
              <tr>
                <td class="mono">{p.starts_on}</td>
                <td class="c">
                  <span class={p.status === 'locked' ? 'pill pill-locked' : 'pill pill-open'}>
                    {p.status}
                  </span>
                </td>
                <td class="r mono">{p.entry_count}</td>
                <td class="r mono">{formatUsd(p.total_debits)}</td>
                <td class="r mono">{formatUsd(p.total_credits)}</td>
                <td>{p.locked_by ?? '—'}</td>
                <td class="mono small" title={p.locked_checksum ?? ''}>
                  {shortChecksum(p) || '—'}
                </td>
                <td class="r">
                  {#if readOnly}
                    <span class="text-muted small">—</span>
                  {:else if p.status === 'open'}
                    <button type="button" disabled={periodBusy[p.id]} onclick={() => doLock(p)}>
                      Lock
                    </button>
                  {:else}
                    <button
                      type="button"
                      class="secondary"
                      disabled={periodBusy[p.id]}
                      onclick={() => doUnlock(p)}
                    >
                      Unlock
                    </button>
                  {/if}
                  {#if periodError[p.id]}
                    <div class="error small">{periodError[p.id]}</div>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
  </Section>
</div>

