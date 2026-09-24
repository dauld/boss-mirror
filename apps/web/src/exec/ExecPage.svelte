<script lang="ts">
  // Executive overview — port of apps/web/src/exec/ExecPage.tsx.
  //
  // Inlines its panels because each one is small and they share the
  // dashboard-card layout. Keeping them together here trades a bit of
  // file length for a much shorter import graph. The launch-calendar
  // panel and the people read that named its owners retired with the
  // second example tenant (design 2ea444f5, backlog a8991c86): no
  // Workflow on the live instance or in the example tenant we keep
  // produces a launch date, so the panel only ever said nothing was
  // launching.

  import Link from '@boss/web-kit/ui/Link.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { formatMoney } from '@boss/web-kit/ui/money';
  import { href } from '../router';
  import { appNow } from '@boss/web-kit/sim-clock';
  import {
    loadCommerceSummary,
    type CommerceSummary,
  } from '../finance/api';
  import { getLabel } from '@boss/web-kit/session/manifest.svelte';

  // --- Shared formatting ---------------------------------------------------

  function fmtUsd(cents: number): string {
    return formatMoney({ amount_cents: cents, currency: 'USD' }, { precision: 'compact' });
  }
  function pluralize(n: number, word: string): string {
    return n === 1 ? `${n} ${word}` : `${n} ${word}s`;
  }
  // $derived so the displayed date updates as the sim clock
  // advances. Pre-fix this was a plain function called from the
  // template (`{todayStr()}`); Svelte 5 sometimes doesn't track
  // the cross-module $state read through a function call, so
  // the date appeared frozen even as simClock.value updated via
  // SSE. $derived registers the dep explicitly at the component
  // scope and re-runs on every simClock update.
  const todayStr = $derived(
    appNow().toLocaleDateString('en-US', {
      weekday: 'long',
      month: 'long',
      day: 'numeric',
      year: 'numeric',
    }),
  );

  let summary = $state<CommerceSummary | null>(null);
  let summaryLoading = $state(true);

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const s = await loadCommerceSummary();
        if (!cancelled) {
          summary = s;
          summaryLoading = false;
        }
      } catch {
        if (!cancelled) summaryLoading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  // --- Mix panel data ------------------------------------------------------

  // Revenue-category labels — shared canonical lookup with
  // finance/types.ts's `revenueCategoryLabel()`. Each tenant
  // overrides individual codes in tenant.toml's [labels] block
  // under `finance.revenue_category.<code>`; otherwise we fall
  // back to a humanized version of the code. The exec dashboard
  // doesn't need its own per-code dictionary — the canonical
  // label is the same for finance + exec views.
  function mixLabel(category: string): string {
    // Tenant override key: e.g. `finance.revenue_category.wholesale`.
    // Tenants override individual categories in tenant.toml's
    // [labels] block to flavor their exec report.
    // Humanized fallback: `event-package` → `Event package`,
    // `taproom` → `Taproom`. Matches finance/types.ts's
    // humanizeCategoryCode behavior.
    const humanized = category
      ? category.replace(/-/g, ' ').replace(/^./, (c) => c.toUpperCase())
      : '—';
    return getLabel(`finance.revenue_category.${category}`, humanized);
  }

  let mixRows = $derived.by(() => {
    if (!summary) return [];
    const total = summary.total_revenue_ttm_cents;
    return [...summary.revenue_ttm]
      .sort((a, b) => b.revenue_cents - a.revenue_cents)
      .map((r) => ({
        cat: r.category,
        amount: r.revenue_cents,
        share: total > 0 ? r.revenue_cents / total : 0,
      }));
  });

  // --- Jobs panel data -----------------------------------------------------

  type JobSummaryRow = { kind: string; count: number };
  let jobSummary = $state<{ total: number; by_kind: JobSummaryRow[] } | null>(null);
  let jobsLoading = $state(true);
  let kindLabels = $state<Map<string, string>>(new Map());

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/jobs/summary?status=open');
        if (r.ok) {
          const data = (await r.json()) as {
            counts: Record<string, number>;
            total: number;
          };
          const by_kind = Object.entries(data.counts ?? {})
            .map(([kind, count]) => ({ kind, count }))
            .sort((a, b) => b.count - a.count);
          if (!cancelled) {
            jobSummary = { total: data.total ?? 0, by_kind };
          }
        }
      } catch {
        // ignore
      }
      try {
        const r = await fetch('/api/workflows');
        if (r.ok) {
          const kinds = (await r.json()) as Array<{ kind: string; label: string }>;
          const m = new Map<string, string>();
          for (const k of kinds) m.set(k.kind, k.label);
          if (!cancelled) kindLabels = m;
        }
      } catch {
        // ignore
      }
      if (!cancelled) jobsLoading = false;
    })();
    return () => {
      cancelled = true;
    };
  });

  function kindLabel(kind: string): string {
    return kindLabels.get(kind) ?? kind;
  }

  let jobTop = $derived(jobSummary ? jobSummary.by_kind.slice(0, 5) : []);
  let jobMaxCount = $derived(Math.max(...jobTop.map((r) => r.count), 1));

  // --- Finished-goods inventory panel ------------------------------------
  // Replaces the legacy "Tech utilization" panel which keyed on service-
  // technician scheduling — not relevant to brewery executives. The
  // brewery's "what do we have to sell?" question reads /api/products,
  // which carries total_on_hand rolled up across locations.

  type ProductRow = {
    sku: string;
    name: string;
    product_kind?: string;
    total_on_hand?: number | null;
    metadata?: Record<string, unknown> | null;
  };
  let products = $state<ProductRow[]>([]);
  let productsLoading = $state(true);
  /// Non-null when the panel's load failed — rendered instead of the
  /// empty state (packet 3fba9c35, the false-empty sweep).
  let productsFailed = $state<string | null>(null);

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/products');
        if (r.ok) {
          const list = (await r.json()) as ProductRow[];
          if (!cancelled) {
            products = list;
            productsFailed = null;
          }
        } else {
          if (!cancelled) productsFailed = `HTTP ${r.status}`;
        }
      } catch (e) {
        if (!cancelled) productsFailed = e instanceof Error ? e.message : String(e);
      }
      if (!cancelled) productsLoading = false;
    })();
    return () => {
      cancelled = true;
    };
  });

  let topProducts = $derived(
    [...products]
      .filter((p) => (p.total_on_hand ?? 0) > 0)
      .sort((a, b) => (b.total_on_hand ?? 0) - (a.total_on_hand ?? 0))
      .slice(0, 8),
  );

  let totalOnHand = $derived(
    products.reduce((sum, p) => sum + (p.total_on_hand ?? 0), 0),
  );

  // --- Cash + receivables panel ------------------------------------------
  // Replaces the legacy "Churn watchlist" panel (account-team CSM signal).
  // For a brewery exec the day-one financial question is "what's in the
  // bank and what do customers owe us?" — both come out of the GL
  // projection at /api/ledger/balance-sheet. Cash + AR show the brewery's
  // working-capital position at a glance; deeper detail lives at /finance.

  type BalanceSheetLine = {
    account_code: string;
    account_name: string;
    amount_cents: number;
  };
  type BalanceSheet = {
    as_of: string;
    assets: BalanceSheetLine[];
    total_assets_cents: number;
    liabilities?: BalanceSheetLine[];
  };

  let balanceSheet = $state<BalanceSheet | null>(null);
  let balanceSheetLoading = $state(true);

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/ledger/balance-sheet');
        if (r.ok && !cancelled) {
          balanceSheet = (await r.json()) as BalanceSheet;
        }
      } catch {
        // ignore — panel renders empty-state
      }
      if (!cancelled) balanceSheetLoading = false;
    })();
    return () => {
      cancelled = true;
    };
  });

  let cashLine = $derived(
    balanceSheet?.assets.find((l) => l.account_code === '1000') ?? null,
  );
  let arLine = $derived(
    balanceSheet?.assets.find((l) => l.account_code === '1100') ?? null,
  );
  let cashInTransitLine = $derived(
    balanceSheet?.assets.find((l) => l.account_code === '1010') ?? null,
  );
  let apLine = $derived(
    balanceSheet?.liabilities?.find((l) => l.account_code === '2100') ?? null,
  );
</script>

<div class="exec theme-exec">
  <header class="exec-header">
    <div>
      <div class="exec-eyebrow">BOSS — Executive Overview</div>
      <h1 class="exec-title">{todayStr}</h1>
    </div>
    <div class="exec-subtitle">
      What changed, and what deserves your attention
    </div>
  </header>

  <div class="exec-grid">
    <section class="exec-card">
      <h2>Revenue mix — trailing 12 months</h2>
      {#if summaryLoading && !summary}
        <p class="empty">Loading revenue mix…</p>
      {:else if !summary}
        <!-- A failed summary read, on the shared failure marker (sweep
             c3e4edcc); an empty trailing year is the line below, which
             this one used to share. -->
        <p class="empty load-failed" role="alert">Revenue mix unavailable — the finance summary did not answer.</p>
      {:else if summary.revenue_ttm.length === 0}
        <p class="empty">No revenue in the trailing 12 months.</p>
      {:else}
        <div class="mix">
          {#each mixRows as r (r.cat)}
            <div class="mix-row">
              <div class="mix-label">{mixLabel(r.cat)}</div>
              <div class="mix-bar">
                <div class="mix-fill" style={`width:${r.share * 100}%`}></div>
              </div>
              <div class="mix-share">{(r.share * 100).toFixed(0)}%</div>
              <div class="mix-amount">{fmtUsd(r.amount)}</div>
            </div>
          {/each}
        </div>
      {/if}
    </section>

    <section class="exec-card exec-card-wide">
      <h2>Active jobs</h2>
      {#if jobsLoading}
        <div class="stat-row"><div class="stat"><div class="stat-label">Loading jobs...</div></div></div>
      {:else if !jobSummary}
        <div class="stat-row"><div class="stat"><div class="stat-label">Jobs data unavailable</div></div></div>
      {:else}
        <div class="stat-row">
          <div class="stat">
            <div class="stat-label">Open jobs</div>
            <div class="stat-value">{jobSummary.total.toLocaleString()}</div>
          </div>
          <div class="stat">
            <div class="stat-label">Job kinds active</div>
            <div class="stat-value">{jobSummary.by_kind.length}</div>
          </div>
        </div>
        <div class="bar-list">
          {#each jobTop as r (r.kind)}
            <div class="bar-row">
              <span class="bar-label">{kindLabel(r.kind)}</span>
              <div class="bar-track">
                <div
                  class="bar-fill"
                  style={`width:${(r.count / jobMaxCount) * 100}%`}
                ></div>
              </div>
              <span class="bar-count">{r.count}</span>
            </div>
          {/each}
        </div>
        <div style="margin-top:12px; text-align:right">
          <Link to={href('/ux/jobs')}>
            View all jobs →
          </Link>
        </div>
      {/if}
    </section>

    <section class="exec-card exec-card-wide">
      <h2>Finished goods on hand</h2>
      {#if productsLoading && products.length === 0}
        <p class="empty">Loading inventory…</p>
      {:else if productsFailed}
        <p class="empty load-failed" role="alert">
          Couldn't load inventory — {productsFailed}
        </p>
      {:else if topProducts.length === 0}
        <p class="empty">No finished products in inventory.</p>
      {:else}
        <div class="stat-row">
          <div class="stat">
            <div class="stat-label">SKUs in inventory</div>
            <div class="stat-value">{products.filter((p) => (p.total_on_hand ?? 0) > 0).length}</div>
          </div>
          <div class="stat">
            <div class="stat-label">Total units on hand</div>
            <div class="stat-value">{totalOnHand.toLocaleString()}</div>
          </div>
        </div>
        <table class="data-table">
          <thead>
            <tr>
              <th>SKU</th>
              <th>Product</th>
              <th class="num">On hand</th>
            </tr>
          </thead>
          <tbody>
            {#each topProducts as p (p.sku)}
              <tr>
                <td class="mono" style="font-size:12px">{p.sku}</td>
                <td>{p.name}</td>
                <td class="num">{(p.total_on_hand ?? 0).toLocaleString()}</td>
              </tr>
            {/each}
          </tbody>
        </table>
        <div style="margin-top:8px; text-align:right">
          <Link to={href('/ux/products')}>View full catalog →</Link>
        </div>
      {/if}
    </section>

    <section class="exec-card exec-card-wide">
      <h2>Cash &amp; receivables</h2>
      {#if balanceSheetLoading && !balanceSheet}
        <p class="empty">Loading balance-sheet snapshot…</p>
      {:else if !balanceSheet}
        <p class="empty load-failed" role="alert">Balance sheet unavailable — the ledger did not answer.</p>
      {:else}
        <div style="margin-bottom:12px; font-size:13px; color:var(--static)">
          As of {balanceSheet.as_of}
        </div>
        <div class="stat-row">
          {#if cashLine}
            <div class="stat">
              <div class="stat-label">Cash</div>
              <div class="stat-value">{fmtUsd(cashLine.amount_cents)}</div>
            </div>
          {/if}
          {#if cashInTransitLine && cashInTransitLine.amount_cents !== 0}
            <div class="stat">
              <div class="stat-label">In transit</div>
              <div class="stat-value">{fmtUsd(cashInTransitLine.amount_cents)}</div>
            </div>
          {/if}
          {#if arLine}
            <div class="stat">
              <div class="stat-label">Accounts receivable</div>
              <div class="stat-value">{fmtUsd(arLine.amount_cents)}</div>
            </div>
          {/if}
          {#if apLine}
            <div class="stat">
              <div class="stat-label">Accounts payable</div>
              <div class="stat-value">{fmtUsd(apLine.amount_cents)}</div>
            </div>
          {/if}
        </div>
        <div style="margin-top:8px; text-align:right">
          <Link to={href('/ux/finance')}>Open finance dashboard →</Link>
        </div>
      {/if}
    </section>
  </div>

  <footer class="exec-footer">
    Executive overview — each card links to the deeper view.
  </footer>
</div>
