// Monetary types + formatters. Mirrors `boss-core::money` on the
// Rust side — wire shape is `{ amount_cents: number, currency: string }`,
// no decimals, no locale assumptions.
//
// Every place in the app that displays money should route through
// `formatMoney` so the style is consistent and easy to change once.
// Ad-hoc `$${n.toFixed(2)}` call sites are a smell to fix on sight.
//
// Full convention: docs/architecture-decisions.md §Finance & ledger.

export type Money = Readonly<{
  amount_cents: number;
  currency: string; // ISO-4217 uppercase
}>;

export type MoneyFormatOptions = Readonly<{
  /// `'accounting'` wraps negatives in parens and hides the currency
  /// symbol (the convention for finance tables). `'plain'` uses a
  /// leading sign + currency symbol (the UI default).
  style?: 'plain' | 'accounting';
  /// Show the ISO code after the amount when the currency isn't USD.
  /// Always show it under `'accounting'`. Caller can force it true.
  showCode?: boolean;
  /// How much numeric precision to render.
  ///
  /// - `'cents'` — always show two decimals ("$19,200.00"). Right
  ///   for invoice detail views, line-item edit forms, and any
  ///   surface where the cent value is load-bearing.
  /// - `'whole'` — round to whole units, no decimals ("$19,200").
  ///   Right for tables and at-a-glance tiles where cent precision
  ///   is just visual noise.
  /// - `'auto'` (default) — `'whole'` when the amount is ≥
  ///   `$100.00`, `'cents'` below that. Matches the convention
  ///   that "small" amounts (employee deductions, fees, etc.) want
  ///   precision while large totals (invoices, balances, salaries)
  ///   don't.
  /// - `'compact'` — abbreviate by magnitude for stat tiles and
  ///   at-a-glance surfaces ("$1.3M", "$46K", "$499"). Tiers on
  ///   MAJOR units, so amounts below a thousand render whole
  ///   instead of collapsing to "$0K".
  precision?: 'cents' | 'whole' | 'auto' | 'compact';
}>;

const CURRENCY_SYMBOL: Readonly<Record<string, string>> = {
  USD: '$',
  CAD: 'CA$',
  EUR: '€',
  GBP: '£',
  JPY: '¥',
};

/// Zero-decimal currencies (JPY, KRW, CLP, ...) store the amount
/// as whole units already — no `/100` conversion on display. Kept
/// narrow; extend as new currencies land.
const ZERO_DECIMAL: ReadonlySet<string> = new Set(['JPY', 'KRW', 'CLP']);

/// `'auto'` precision flips to whole-units at this threshold.
/// 100 dollars / euros / etc. — anything bigger, lose the cents.
const AUTO_PRECISION_THRESHOLD_CENTS = 10_000;

/// Format a `Money` value for display.
///
/// - `formatMoney({ amount_cents: 12345, currency: 'USD' })` → `"$123"` (auto: ≥$100 rounds)
/// - `formatMoney({ amount_cents: 4250, currency: 'USD' })` → `"$42.50"` (auto: <$100 keeps cents)
/// - `formatMoney({ amount_cents: 12345, currency: 'USD' }, { precision: 'cents' })` → `"$123.45"`
/// - `formatMoney({ amount_cents: 19200000, currency: 'USD' }, { precision: 'whole' })` → `"$192,000"`
/// - `formatMoney({ amount_cents: -12345, currency: 'USD' }, { style: 'accounting' })` → `"(123.45)"` (accounting always keeps cents)
/// - `formatMoney({ amount_cents: 1000, currency: 'EUR' })` → `"€10.00 EUR"`
/// - `formatMoney({ amount_cents: 1234, currency: 'JPY' })` → `"¥1,234"`
export function formatMoney(
  m: Money,
  opts: MoneyFormatOptions = {},
): string {
  const { style = 'plain', showCode } = opts;
  const currency = m.currency.toUpperCase();
  const symbol = CURRENCY_SYMBOL[currency] ?? '';
  const isZeroDecimal = ZERO_DECIMAL.has(currency);

  // Accounting style is the auditor convention — keep cents always.
  // Other styles default to 'auto' (whole at ≥ $100, cents below).
  const requested = opts.precision ?? (style === 'accounting' ? 'cents' : 'auto');
  const showMinor =
    !isZeroDecimal &&
    (requested === 'cents' ||
      (requested === 'auto' &&
        Math.abs(m.amount_cents) < AUTO_PRECISION_THRESHOLD_CENTS));

  const negative = m.amount_cents < 0;
  const magnitude = Math.abs(m.amount_cents);
  // Whole-unit precision rounds half-to-even via Math.round; the
  // direction matters very little for display + table totals
  // already aggregate from the cent payloads.
  const major = isZeroDecimal
    ? magnitude
    : showMinor
      ? Math.floor(magnitude / 100)
      : Math.round(magnitude / 100);
  const minor = isZeroDecimal || !showMinor ? 0 : magnitude % 100;

  // COMPACT tiers on MAJOR units — dollars, or whole yen for a
  // zero-decimal currency — never on the raw cents. The two hand-rolled
  // versions this replaces both tiered on cents, and AccountPage's
  // divided by 100_000 unconditionally, so every amount under $500
  // rendered as "$0K" on a glance tile whose whole job is the number.
  const compact = (): string => {
    const units = isZeroDecimal ? magnitude : magnitude / 100;
    if (units >= 1_000_000) return `${(units / 1_000_000).toFixed(1)}M`;
    if (units >= 1_000) return `${Math.round(units / 1_000).toLocaleString('en-US')}K`;
    return Math.round(units).toLocaleString('en-US');
  };

  const majorStr = major.toLocaleString('en-US');
  const amountStr =
    requested === 'compact'
      ? compact()
      : isZeroDecimal || !showMinor
        ? majorStr
        : `${majorStr}.${minor.toString().padStart(2, '0')}`;

  const showCodeFinal = showCode ?? (style === 'accounting' || currency !== 'USD');
  const codeSuffix = showCodeFinal ? ` ${currency}` : '';

  if (style === 'accounting') {
    // Accounting style: parens for negatives, no leading sign, always
    // show the ISO code for disambiguation in tables.
    if (negative) return `(${amountStr})${codeSuffix}`;
    return `${amountStr}${codeSuffix}`;
  }

  const sign = negative ? '-' : '';
  return `${sign}${symbol}${amountStr}${codeSuffix}`;
}

/// Additive zero of the given currency, useful as a `reduce` seed.
export function zeroMoney(currency = 'USD'): Money {
  return { amount_cents: 0, currency };
}

/// Sum a list of `Money` values. Throws if currencies diverge —
/// that's a domain bug, not a silent conversion.
export function sumMoney(items: ReadonlyArray<Money>): Money {
  if (items.length === 0) return zeroMoney();
  const currency = items[0]!.currency;
  let amount_cents = 0;
  for (const m of items) {
    if (m.currency !== currency) {
      throw new Error(
        `sumMoney: currency mismatch — ${currency} vs ${m.currency}`,
      );
    }
    amount_cents += m.amount_cents;
  }
  return { amount_cents, currency };
}

/// Construct from dollars + cents. Rejects out-of-range cents so
/// `$1.234` can't silently round into the wire.
export function money(
  major: number,
  minor = 0,
  currency = 'USD',
): Money {
  if (!Number.isInteger(major) || !Number.isInteger(minor)) {
    throw new Error(`money: major and minor must be integers, got ${major}.${minor}`);
  }
  if (minor < 0 || minor >= 100) {
    throw new Error(`money: minor must be in [0,100), got ${minor}`);
  }
  const sign = major < 0 ? -1 : 1;
  return { amount_cents: major * 100 + sign * minor, currency };
}
