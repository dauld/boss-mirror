// Finance domain types. Port of apps/web/src/finance/types.ts.

import { getLabel } from '@boss/web-kit/session/manifest.svelte';

export type InvoiceStatus = 'paid' | 'outstanding' | 'past-due' | 'written-off';

// Open string — tenants extend categories as data, not as code
// changes. The server-side type (`boss-commerce`'s
// `RevenueCategory` newtype) matches this shape. Human-readable
// labels come from `revenueCategoryLabel(code)` below, which
// routes through the tenant manifest's `[labels]` block before
// falling back to a humanized version of the code.
export type RevenueCategory = string;

export type InvoiceLineItem = {
  id: string;
  invoice_id: string;
  revenue_category: RevenueCategory;
  amount_cents: number;
  currency: string;
  description: string;
  ref_id: string | null;
};

export type PaymentMethod = 'ach' | 'wire' | 'check' | 'card';

export type Invoice = {
  id: string;
  account_id: string;
  issued_on: string;
  due_on: string;
  paid_on: string | null;
  status: InvoiceStatus;
  amount_cents: number;
  currency: string;
  tax_cents?: number;
  tax_jurisdiction?: string | null;
  payment_method?: PaymentMethod | null;
  line_items: ReadonlyArray<InvoiceLineItem>;
};

// Tenant-aware label resolution. Each tenant declares its own
// category vocabulary in `tenant.toml`'s `[labels]` block under
// keys like `finance.revenue_category.<code>`; the lookup here
// falls back to a humanized version of the code so unrecognized
// values still render legibly. Use `revenueCategoryLabel("wholesale")`
// instead of the prior `REVENUE_CATEGORY_LABEL[code]` pattern.
export function revenueCategoryLabel(code: RevenueCategory): string {
  return getLabel(`finance.revenue_category.${code}`, humanizeCategoryCode(code));
}

// Fallback humanizer: `event-package` → `Event package`,
// `taproom` → `Taproom`. Used when the tenant manifest carries no
// label for the code.
function humanizeCategoryCode(code: string): string {
  if (!code) return '—';
  const spaced = code.replace(/-/g, ' ');
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

// The statuses no longer owed: paid, and written off — the write-off
// credits 1100 A/R, so the receivable is gone though no cash came in.
// Every other status is still owed. The server's one definition is
// `InvoiceStatus::NOT_OWED` in boss-commerce, which open-AR and the
// summary's AR aging read; this is its copy on the far side of the
// wire, held equal to it by boss-commerce's
// `the_spa_not_owed_list_is_the_servers` test (backlog 926d64a3).
export const NOT_OWED: ReadonlyArray<InvoiceStatus> = ['paid', 'written-off'];

/** True while the invoice is still a receivable. */
export function isOwed(status: InvoiceStatus): boolean {
  return !NOT_OWED.includes(status);
}

export const INVOICE_STATUS_LABEL: Record<InvoiceStatus, string> = {
  paid: 'Paid',
  outstanding: 'Outstanding',
  'past-due': 'Past due',
  'written-off': 'Written off',
};

export const PAYMENT_METHOD_LABEL: Record<PaymentMethod, string> = {
  ach: 'ACH',
  wire: 'Wire',
  check: 'Check',
  card: 'Card',
};
