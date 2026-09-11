<script lang="ts">
  // Invoices tab — filterable list. Port of InvoicesTab sub-component
  // from apps/web/src/finance/FinancePage.tsx.

  import FilterGroup from '@boss/web-kit/ui/FilterGroup.svelte';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import SearchInput from '@boss/web-kit/ui/SearchInput.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import OverflowBanner from '@boss/web-kit/ui/OverflowBanner.svelte';
  import SortHeader from '@boss/web-kit/ui/SortHeader.svelte';
  import { createSortState } from '@boss/web-kit/ui/sort-state.svelte';
  import InvoiceStatusChip from './InvoiceStatusChip.svelte';
  import {
    PAYMENT_METHOD_LABEL,
    type Invoice,
    type InvoiceStatus,
    type PaymentMethod,
  } from './types';
  import type { Account } from '../accounts/types';
  import { formatMoney } from '@boss/web-kit/ui/money';

  type Props = {
    invoices: ReadonlyArray<Invoice>;
    /// Non-null when the invoice load failed — rendered instead of
    /// the empty state, so an outage never reads as "no invoices"
    /// (packet 3fba9c35).
    loadError?: string | null;
    totalCount: number;
  };
  let { invoices, loadError = null, totalCount }: Props = $props();

  type StatusFilter = InvoiceStatus | 'all' | 'unpaid';
  type MethodFilter = PaymentMethod | 'all';

  const METHOD_FILTERS: ReadonlyArray<MethodFilter> = ['all', 'ach', 'wire', 'check', 'card'];

  let statusFilter = $state<StatusFilter>('all');
  let methodFilter = $state<MethodFilter>('all');
  let query = $state('');
  let accounts = $state<Account[]>([]);

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/people/accounts');
        if (!r.ok) return;
        const body = await r.json();
        if (!cancelled) {
          accounts = Array.isArray(body) ? body : (body.data ?? []);
        }
      } catch {
        // Ignore — invoices still render without friendly account names.
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let accountById = $derived.by(() => {
    const m = new Map<string, Account>();
    for (const p of accounts) m.set(p.id, p);
    return m;
  });

  let truncated = $derived(totalCount > invoices.length);
  // "Unpaid" = genuinely awaiting collection (outstanding + past-due).
  // Written-off invoices are uncollectable, not unpaid — they get their own
  // bucket so the operator sees real receivables vs historical write-offs.
  let unpaid = $derived(
    invoices.filter((i) => i.status !== 'paid' && i.status !== 'written-off'),
  );
  let pastDue = $derived(invoices.filter((i) => i.status === 'past-due'));
  let writtenOff = $derived(invoices.filter((i) => i.status === 'written-off'));

  let methodCounts = $derived.by(() => {
    const counts: Record<PaymentMethod | 'all', number> = {
      all: invoices.length,
      ach: 0,
      wire: 0,
      check: 0,
      card: 0,
    };
    for (const i of invoices) {
      if (i.payment_method) counts[i.payment_method] += 1;
    }
    return counts;
  });

  let visible = $derived(
    invoices.filter((i) => {
      if (
        statusFilter === 'unpaid' &&
        (i.status === 'paid' || i.status === 'written-off')
      )
        return false;
      if (
        statusFilter !== 'all' &&
        statusFilter !== 'unpaid' &&
        i.status !== statusFilter
      )
        return false;
      if (methodFilter !== 'all' && i.payment_method !== methodFilter) return false;
      if (query) {
        const q = query.toLowerCase();
        const account = accountById.get(i.account_id);
        const lineText = i.line_items
          .map((l) => `${l.revenue_category} ${l.description}`)
          .join(' ');
        if (!`${i.id} ${account?.name ?? ''} ${lineText}`.toLowerCase().includes(q)) {
          return false;
        }
      }
      return true;
    }),
  );

  // 3,491 seeded wholesale orders made fixed-order lists useless
  // (CAR-4). Issued-newest-first is the landing order; every column
  // is clickable. Money/date/count columns default descending.
  type SortKey =
    | 'id'
    | 'status'
    | 'account'
    | 'lines'
    | 'amount'
    | 'tax'
    | 'method'
    | 'issued'
    | 'due'
    | 'paid';
  const DESC_FIRST: ReadonlyArray<SortKey> = ['lines', 'amount', 'tax', 'issued', 'due', 'paid'];
  const sort = createSortState<SortKey>({ key: 'issued', dir: 'desc' }, (k) =>
    DESC_FIRST.includes(k) ? 'desc' : 'asc',
  );
  let visibleSorted = $derived(
    sort.sorted(visible, {
      id: (i) => i.id,
      status: (i) => i.status,
      account: (i) => accountById.get(i.account_id)?.name ?? i.account_id,
      lines: (i) => i.line_items.length,
      amount: (i) => i.amount_cents,
      tax: (i) => i.tax_cents ?? 0,
      method: (i) => i.payment_method,
      issued: (i) => i.issued_on,
      due: (i) => i.due_on,
      paid: (i) => i.paid_on,
    }),
  );
</script>

<div class="catalog-layout">
  <aside class="catalog-filters">
    <FilterGroup label="Search">
        <SearchInput bind:value={query} placeholder="Invoice, account…" />
    </FilterGroup>
    <FilterGroup label="Status">
        <FilterButton active={statusFilter === 'all'} onclick={() => (statusFilter = 'all')}>
          All ({invoices.length})
        </FilterButton>
        <FilterButton active={statusFilter === 'unpaid'} onclick={() => (statusFilter = 'unpaid')}>
          Unpaid ({unpaid.length})
        </FilterButton>
        <FilterButton active={statusFilter === 'past-due'} onclick={() => (statusFilter = 'past-due')}>
          Past due ({pastDue.length})
        </FilterButton>
        <FilterButton active={statusFilter === 'paid'} onclick={() => (statusFilter = 'paid')}>
          Paid ({invoices.filter((i) => i.status === 'paid').length})
        </FilterButton>
        {#if writtenOff.length > 0}
          <FilterButton
            active={statusFilter === 'written-off'}
            onclick={() => (statusFilter = 'written-off')}
          >
            Written off ({writtenOff.length})
          </FilterButton>
        {/if}
    </FilterGroup>
    <FilterGroup label="Method">
        {#each METHOD_FILTERS as m (m)}
          <FilterButton active={methodFilter === m} onclick={() => (methodFilter = m)}>
              {m === 'all' ? 'All' : PAYMENT_METHOD_LABEL[m]} ({methodCounts[m]})
          </FilterButton>
        {/each}
    </FilterGroup>
  </aside>

  <section class="list-section">
    {#if truncated}
      <OverflowBanner
        showing={invoices.length}
        total={totalCount}
        noun="invoices"
        hint="Use search or status filters to narrow the list."
      />
    {/if}
    {#if loadError}
      <p class="empty load-failed" role="alert">
        Couldn't load invoices — {loadError}
      </p>
    {:else if visible.length === 0}
      <p class="empty">No invoices match those filters.</p>
    {:else}
      <table class="data-table data-table-striped">
        <thead>
          <tr>
            <SortHeader {sort} key="id">Invoice</SortHeader>
            <SortHeader {sort} key="status">Status</SortHeader>
            <SortHeader {sort} key="account">Account</SortHeader>
            <SortHeader {sort} key="lines" num={true}>Lines</SortHeader>
            <SortHeader {sort} key="amount" num={true}>Amount</SortHeader>
            <SortHeader {sort} key="tax" num={true}>Tax</SortHeader>
            <SortHeader {sort} key="method">Method</SortHeader>
            <SortHeader {sort} key="issued">Issued</SortHeader>
            <SortHeader {sort} key="due">Due</SortHeader>
            <SortHeader {sort} key="paid">Paid</SortHeader>
          </tr>
        </thead>
        <tbody>
          {#each visibleSorted as i (i.id)}
            {@const account = accountById.get(i.account_id)}
            {@const taxCents = i.tax_cents ?? 0}
            <tr>
              <td class="mono"><EntityLink kind="invoice" id={i.id} /></td>
              <td><InvoiceStatusChip status={i.status} /></td>
              <td>
                <EntityLink
                  kind="account"
                  id={i.account_id}
                  label={account?.name}
                  mono={!account}
                />
              </td>
              <td class="num">{i.line_items.length}</td>
              <td class="num">{formatMoney({ amount_cents: i.amount_cents, currency: i.currency })}</td>
              <td class="num" style={taxCents > 0 ? '' : 'color:#a8a29e'}>
                {#if taxCents > 0}
                  {formatMoney({ amount_cents: taxCents, currency: i.currency })}
                  {#if i.tax_jurisdiction}
                    <span class="mono" style="margin-left:4px; font-size:10px; color:#78716c">
                      {i.tax_jurisdiction.replace(/^US-/, '')}
                    </span>
                  {/if}
                {:else}
                  —
                {/if}
              </td>
              <td>
                {#if i.payment_method}
                  {PAYMENT_METHOD_LABEL[i.payment_method]}
                {:else}
                  <span style="color:#a8a29e">—</span>
                {/if}
              </td>
              <td>{i.issued_on}</td>
              <td>{i.due_on}</td>
              <td>{i.paid_on ?? '—'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </section>
</div>
