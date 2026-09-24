<script lang="ts">
  // Manual journal entry form — port of
  // apps/web/src/finance/NewJournalEntryPage.tsx.

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import {
    createManualEntry,
    formatUsd,
    loadAccounts,
    type Account,
  } from './ledger';
  import { href, navigate } from '../router';
  import { appToday } from '@boss/web-kit/sim-clock';

  type LineDraft = {
    account_code: string;
    side: 'debit' | 'credit';
    amount_dollars: string;
    memo: string;
  };

  const NEW_LINE: LineDraft = {
    account_code: '',
    side: 'debit',
    amount_dollars: '',
    memo: '',
  };

  function isoToday(): string {
    return appToday();
  }

  function parseDollarsToCents(s: string): number {
    const n = parseFloat(s);
    if (!Number.isFinite(n) || n <= 0) return 0;
    return Math.round(n * 100);
  }

  let accounts = $state<Account[]>([]);
  let loading = $state(true);
  let postedOn = $state(isoToday());
  let memo = $state('');
  let createdBy = $state('');
  let lines = $state<LineDraft[]>([
    { ...NEW_LINE, side: 'debit' },
    { ...NEW_LINE, side: 'credit' },
  ]);
  let saving = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      const rows = await loadAccounts();
      if (!cancelled) {
        accounts = rows;
        loading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let activeAccounts = $derived(accounts.filter((a) => a.is_active));

  let totals = $derived.by(() => {
    let d = 0;
    let c = 0;
    for (const l of lines) {
      const cents = parseDollarsToCents(l.amount_dollars);
      if (l.side === 'debit') d += cents;
      else c += cents;
    }
    return { totalDebits: d, totalCredits: c };
  });

  let balanced = $derived(
    totals.totalDebits > 0 && totals.totalDebits === totals.totalCredits,
  );
  let allLinesValid = $derived(
    lines.every(
      (l) => l.account_code !== '' && parseDollarsToCents(l.amount_dollars) > 0,
    ),
  );
  let canSubmit = $derived(
    !saving && balanced && lines.length >= 2 && allLinesValid,
  );

  function updateLine(i: number, patch: Partial<LineDraft>): void {
    lines = lines.map((l, idx) => (idx === i ? { ...l, ...patch } : l));
  }
  function addLine(side: 'debit' | 'credit'): void {
    lines = [...lines, { ...NEW_LINE, side }];
  }
  function removeLine(i: number): void {
    if (lines.length > 2) {
      lines = lines.filter((_, idx) => idx !== i);
    }
  }

  async function submit(): Promise<void> {
    if (!canSubmit) return;
    saving = true;
    error = null;
    try {
      const result = await createManualEntry({
        posted_on: postedOn,
        memo: memo.trim() || null,
        created_by: createdBy.trim() || null,
        lines: lines.map((l) => {
          const cents = parseDollarsToCents(l.amount_dollars);
          return {
            account_code: l.account_code,
            debit_cents: l.side === 'debit' ? cents : 0,
            credit_cents: l.side === 'credit' ? cents : 0,
            memo: l.memo.trim() || null,
          };
        }),
      });
      navigate(href(`/ux/finance?entry=${encodeURIComponent(result.entry_id)}`));
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      saving = false;
    }
  }
</script>

<div class="theme-exec" style="padding:0 32px 32px">
  <Breadcrumb to={href('/ux/finance')}>
    ← Finance
  </Breadcrumb>
  <PageHeader
    eyebrow="Finance"
    title="New journal entry"
    subtitle="Post an adjusting or reversing entry directly to the GL"
  />

  <div style="max-width:900px">
    <Section title="Entry details">
        <div class="ni-field-row">
          <div class="ni-field">
            <label for="mje-posted">Posted on</label>
            <input id="mje-posted" type="date" bind:value={postedOn} class="ni-input" />
          </div>
          <div class="ni-field" style="flex:1">
            <label for="mje-memo">Memo</label>
            <input
              id="mje-memo"
              type="text"
              bind:value={memo}
              placeholder="Q1 accrual, depreciation, reclassification…"
              class="ni-input"
            />
          </div>
          <div class="ni-field">
            <label for="mje-by">Posted by</label>
            <input
              id="mje-by"
              type="text"
              bind:value={createdBy}
              placeholder="admin"
              class="ni-input"
            />
          </div>
        </div>
    </Section>

    <Section title={`Lines (${lines.length})`}>
        {#if loading}
          <p class="empty">Loading chart of accounts…</p>
        {:else}
          <table class="ni-lines-table">
            <thead>
              <tr>
                <th>Account</th>
                <th>Side</th>
                <th style="text-align:right">Amount</th>
                <th>Memo</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {#each lines as l, i (i)}
                <tr>
                  <td>
                    <select
                      value={l.account_code}
                      onchange={(e) => updateLine(i, { account_code: (e.target as HTMLSelectElement).value })}
                      class="ni-input"
                    >
                      <option value="">— select —</option>
                      {#each activeAccounts as a (a.code)}
                        <option value={a.code}>{a.code} · {a.name}</option>
                      {/each}
                    </select>
                  </td>
                  <td>
                    <select
                      value={l.side}
                      onchange={(e) => updateLine(i, { side: (e.target as HTMLSelectElement).value as 'debit' | 'credit' })}
                      class="ni-input"
                    >
                      <option value="debit">Debit</option>
                      <option value="credit">Credit</option>
                    </select>
                  </td>
                  <td style="text-align:right">
                    <input
                      type="number"
                      min="0"
                      step="0.01"
                      value={l.amount_dollars}
                      oninput={(e) => updateLine(i, { amount_dollars: (e.target as HTMLInputElement).value })}
                      class="ni-input"
                      style="text-align:right; width:120px"
                    />
                  </td>
                  <td>
                    <input
                      type="text"
                      value={l.memo}
                      oninput={(e) => updateLine(i, { memo: (e.target as HTMLInputElement).value })}
                      placeholder="Optional"
                      class="ni-input"
                    />
                  </td>
                  <td>
                    {#if lines.length > 2}
                      <button
                        type="button"
                        onclick={() => removeLine(i)}
                        class="hr-done-btn"
                        style="background:var(--err-wash); color:var(--err)"
                      >
                        Remove
                      </button>
                    {/if}
                  </td>
                </tr>
              {/each}
            </tbody>
            <tfoot>
              <tr style="border-top:1px solid var(--hairline); font-weight:600">
                <td colspan="2" style="text-align:right">Totals:</td>
                <td style="text-align:right">
                  {formatUsd(totals.totalDebits)} · {formatUsd(totals.totalCredits)}
                </td>
                <td colspan="2">
                  {#if balanced}
                    <span style="color:var(--ok)">Balanced</span>
                  {:else}
                    <span style="color:var(--err)">
                      Off by {formatUsd(Math.abs(totals.totalDebits - totals.totalCredits))}
                    </span>
                  {/if}
                </td>
              </tr>
            </tfoot>
          </table>
        {/if}
        <div style="display:flex; gap:8px; margin-top:12px">
          <button type="button" onclick={() => addLine('debit')} class="hr-done-btn">+ Debit line</button>
          <button type="button" onclick={() => addLine('credit')} class="hr-done-btn">+ Credit line</button>
        </div>
    </Section>

    {#if error}
      <div
        role="alert"
        style="margin:12px 0; padding:10px 14px; border:1px solid var(--troubled); background:var(--err-wash); border-radius:6px; color:var(--err)"
      >
        {error}
      </div>
    {/if}

    <div style="display:flex; gap:8px; margin-top:16px">
      <button
        type="button"
        onclick={submit}
        disabled={!canSubmit}
        class="fin-new-invoice"
        style={canSubmit ? 'opacity:1' : 'opacity:0.5'}
      >
        {saving ? 'Posting…' : 'Post entry'}
      </button>
      <button type="button" onclick={() => navigate(href('/ux/finance'))} class="hr-done-btn">
        Cancel
      </button>
    </div>
  </div>
</div>
