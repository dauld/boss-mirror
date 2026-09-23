<script lang="ts">
  // Ad-hoc invoice form — port of apps/web/src/finance/NewInvoicePage.tsx.

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import type { Account } from '../accounts/types';
  import { fetchAccountsPage } from '../accounts/api';
  import {
    INVOICE_STATUS_LABEL,
    revenueCategoryLabel,
    type InvoiceStatus,
    type RevenueCategory,
  } from './types';
  import { fetchValidated, z } from '../data/parseResponse';
  import { href, navigate } from '../router';
  import { appNow, appToday } from '@boss/web-kit/sim-clock';

  const RevenueCategoryRowSchema = z.object({
    code: z.string(),
    label: z.string(),
  });
  const RevenueCategoryListSchema = z.array(RevenueCategoryRowSchema);

  type LineDraft = {
    revenue_category: RevenueCategory;
    amount_dollars: string;
    description: string;
  };

  const DEFAULT_LINE: LineDraft = {
    revenue_category: 'service',
    amount_dollars: '',
    description: '',
  };

  function isoToday(): string {
    return appToday();
  }
  function isoPlusDays(days: number): string {
    return new Date(appNow().getTime() + days * 86_400_000).toISOString().slice(0, 10);
  }
  function newInvoiceId(): string {
    return `INV-ADHOC-${Date.now().toString(36).toUpperCase()}`;
  }

  let accounts = $state<Account[]>([]);
  let accountId = $state('');
  let accountQuery = $state('');
  let issuedOn = $state(isoToday());
  let dueOn = $state(isoPlusDays(30));
  let status = $state<InvoiceStatus>('outstanding');
  let lines = $state<LineDraft[]>([{ ...DEFAULT_LINE }]);
  let saving = $state(false);
  let error = $state<string | null>(null);

  /// Non-null when the account list failed to load. Without it the
  /// picker rendered one blank option and Create stayed disabled with
  /// no explanation — an outage disguised as an empty roster (packet
  /// 3fba9c35, the false-empty sweep).
  let accountsFailed = $state<string | null>(null);
  // The directory is a bounded page (2d1d298e): past the cap, the
  // picker cannot offer every account, and says so.
  let accountsTotal = $state(0);
  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetchAccountsPage();
        if (r.kind === 'failed') {
          if (!cancelled) accountsFailed = r.error;
          return;
        }
        if (!cancelled) {
          accounts = [...r.page.data];
          accountsTotal = r.page.total;
          accountsFailed = null;
        }
      } catch (e) {
        if (!cancelled) accountsFailed = e instanceof Error ? e.message : String(e);
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let accountOptions = $derived.by(() => {
    const q = accountQuery.trim().toLowerCase();
    const base = [...accounts].sort((a, b) =>
      (a.name ?? a.id).localeCompare(b.name ?? b.id),
    );
    if (!q) return base.slice(0, 20);
    return base
      .filter(
        (p) =>
          (p.name ?? '').toLowerCase().includes(q) ||
          p.id.toLowerCase().includes(q) ||
          (p.city ?? '').toLowerCase().includes(q),
      )
      .slice(0, 20);
  });

  let totalDollars = $derived(
    lines.reduce((s, l) => {
      const n = parseFloat(l.amount_dollars);
      return s + (Number.isFinite(n) ? n : 0);
    }, 0),
  );

  let canSubmit = $derived(
    !saving &&
      accountId !== '' &&
      issuedOn !== '' &&
      dueOn !== '' &&
      totalDollars > 0 &&
      lines.every((l) => parseFloat(l.amount_dollars) > 0),
  );

  let selectedAccount = $derived(accounts.find((p) => p.id === accountId));

  // The ad-hoc invoice form's category dropdown sources its
  // options from `GET /api/finance/revenue-categories` (returns
  // [{code, label}] derived from tenant.toml's [labels] block).
  // Empty list = tenant hasn't named any categories; the dropdown
  // falls back to a tenant-neutral `uncategorized` row plus
  // free-text via the description field. Adding a new category is
  // a tenant.toml edit — no SPA change.
  let categoryRows = $state<ReadonlyArray<{ code: RevenueCategory; label: string }>>([
    { code: 'uncategorized', label: 'Uncategorized' },
  ]);

  $effect(() => {
    let cancelled = false;
    (async () => {
      const result = await fetchValidated(
        '/api/finance/revenue-categories',
        RevenueCategoryListSchema,
      );
      if (cancelled || result.kind !== 'ok' || result.data.length === 0) return;
      categoryRows = result.data;
      // First load may arrive after the user has interacted with the
      // form — re-seed any existing draft line's category to the first
      // server-returned code if it's still the placeholder default.
      lines = lines.map((l) =>
        l.revenue_category === 'service' && !result.data.some((r) => r.code === 'service')
          ? { ...l, revenue_category: result.data[0]!.code }
          : l,
      );
    })();
    return () => {
      cancelled = true;
    };
  });

  const STATUS_KEYS = Object.keys(INVOICE_STATUS_LABEL) as InvoiceStatus[];

  function updateLine(i: number, patch: Partial<LineDraft>) {
    lines = lines.map((l, idx) => (idx === i ? { ...l, ...patch } : l));
  }
  function addLine() {
    lines = [...lines, { ...DEFAULT_LINE }];
  }
  function removeLine(i: number) {
    if (lines.length > 1) {
      lines = lines.filter((_, idx) => idx !== i);
    }
  }

  async function submit(): Promise<void> {
    if (!canSubmit) return;
    saving = true;
    error = null;
    try {
      const id = newInvoiceId();
      const totalCents = Math.round(totalDollars * 100);
      const payload = {
        id,
        account_id: accountId,
        issued_on: issuedOn,
        due_on: dueOn,
        paid_on: null,
        status,
        amount_cents: totalCents,
        currency: 'USD',
        line_items: lines.map((l, i) => ({
          id: `${id}-L${i + 1}`,
          invoice_id: id,
          revenue_category: l.revenue_category,
          amount_cents: Math.round(parseFloat(l.amount_dollars) * 100),
          currency: 'USD',
          description: l.description || revenueCategoryLabel(l.revenue_category),
          ref_id: null,
        })),
      };
      const resp = await fetch('/api/commerce/invoices/create', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
      });
      if (!resp.ok) {
        throw new Error(`create failed: ${resp.status} ${await resp.text()}`);
      }
      navigate(entityHref('invoice', id));
    } catch (e) {
      error = String(e);
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
    title="New invoice"
    subtitle="Ad-hoc billing for corrections or work outside the Sale Job path"
  />

  <div style="max-width:780px">
    <Section title="Recipient">
        <div class="ni-field">
          <label for="ni-account-search">Account</label>
          <input
            id="ni-account-search"
            type="text"
            bind:value={accountQuery}
            placeholder="Search by name, id, city…"
            class="ni-input"
          />
          <select
            bind:value={accountId}
            class="ni-input"
            size={Math.min(accountOptions.length + 1, 8)}
          >
            <option value="">— select account —</option>
            {#each accountOptions as p (p.id)}
              <option value={p.id}>
                {p.name} · {p.city}, {p.state} ({p.id})
              </option>
            {/each}
          </select>
          {#if accountsFailed}
            <div class="ni-hint load-failed" role="alert">
              Couldn't load accounts — {accountsFailed}
            </div>
          {:else if accountsTotal > accounts.length}
            <div class="ni-hint" role="status">
              Showing the first {accounts.length} of {accountsTotal} accounts; an account
              past them is not in this list.
            </div>
          {/if}
          {#if selectedAccount}
            <div class="ni-hint">
              Billing to <strong>{selectedAccount.name}</strong> ·
              {selectedAccount.city}, {selectedAccount.state}
            </div>
          {/if}
        </div>
    </Section>

    <Section title="Dates + status">
        <div class="ni-field-row">
          <div class="ni-field">
            <label for="ni-issued">Issued on</label>
            <input id="ni-issued" type="date" bind:value={issuedOn} class="ni-input" />
          </div>
          <div class="ni-field">
            <label for="ni-due">Due on</label>
            <input id="ni-due" type="date" bind:value={dueOn} class="ni-input" />
          </div>
          <div class="ni-field">
            <label for="ni-status">Status</label>
            <select id="ni-status" bind:value={status} class="ni-input">
              {#each STATUS_KEYS as s (s)}
                <option value={s}>{INVOICE_STATUS_LABEL[s]}</option>
              {/each}
            </select>
          </div>
        </div>
    </Section>

    <Section title={`Line items (${lines.length})`}>
        <table class="ni-lines-table">
          <thead>
            <tr>
              <th>Category</th>
              <th>Description</th>
              <th class="num">Amount (USD)</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each lines as l, i (i)}
              <tr>
                <td>
                  <select
                    value={l.revenue_category}
                    onchange={(e) => updateLine(i, { revenue_category: (e.target as HTMLSelectElement).value as RevenueCategory })}
                    class="ni-input"
                  >
                    {#each categoryRows as row (row.code)}
                      <option value={row.code}>{row.label}</option>
                    {/each}
                  </select>
                </td>
                <td>
                  <input
                    type="text"
                    value={l.description}
                    oninput={(e) => updateLine(i, { description: (e.target as HTMLInputElement).value })}
                    placeholder={revenueCategoryLabel(l.revenue_category)}
                    class="ni-input"
                  />
                </td>
                <td class="num">
                  <input
                    type="number"
                    min="0"
                    step="1"
                    value={l.amount_dollars}
                    oninput={(e) => updateLine(i, { amount_dollars: (e.target as HTMLInputElement).value })}
                    class="ni-input ni-num"
                  />
                </td>
                <td>
                  {#if lines.length > 1}
                    <button
                      type="button"
                      class="ni-btn ni-btn-remove"
                      onclick={() => removeLine(i)}
                    >
                      Remove
                    </button>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
          <tfoot>
            <tr>
              <td colspan="2">
                <button type="button" class="ni-btn" onclick={addLine}>+ Add line</button>
              </td>
              <td class="num">
                <strong>${totalDollars.toLocaleString()}</strong>
              </td>
              <td></td>
            </tr>
          </tfoot>
        </table>
    </Section>

    {#if error}<div class="error" style="margin-bottom:12px">{error}</div>{/if}

    <div class="ni-actions">
      <button
        type="button"
        class="ni-btn ni-btn-primary"
        onclick={submit}
        disabled={!canSubmit}
      >
        {saving ? 'Creating…' : 'Create invoice'}
      </button>
      <button
        type="button"
        class="ni-btn"
        onclick={() => navigate(href('/ux/finance'))}
        disabled={saving}
      >
        Cancel
      </button>
    </div>
  </div>
</div>
