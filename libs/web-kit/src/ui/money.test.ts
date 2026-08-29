// Unit tests for the money formatter. Run via `bun test`.

import { describe, expect, test } from 'bun:test';
import { formatMoney } from './money';

describe('formatMoney precision', () => {
  test("auto: amounts >= $100 round to whole units", () => {
    expect(formatMoney({ amount_cents: 19_200_00, currency: 'USD' })).toBe('$19,200');
    expect(formatMoney({ amount_cents: 12_345, currency: 'USD' })).toBe('$123');
    expect(formatMoney({ amount_cents: 10_000, currency: 'USD' })).toBe('$100');
  });

  test('auto: amounts under $100 keep cents', () => {
    expect(formatMoney({ amount_cents: 9_999, currency: 'USD' })).toBe('$99.99');
    expect(formatMoney({ amount_cents: 4_250, currency: 'USD' })).toBe('$42.50');
    expect(formatMoney({ amount_cents: 0, currency: 'USD' })).toBe('$0.00');
  });

  test("'cents' precision always shows two decimals", () => {
    expect(formatMoney(
      { amount_cents: 19_200_00, currency: 'USD' },
      { precision: 'cents' },
    )).toBe('$19,200.00');
    expect(formatMoney(
      { amount_cents: 12_345, currency: 'USD' },
      { precision: 'cents' },
    )).toBe('$123.45');
  });

  test("'whole' precision strips decimals at any magnitude", () => {
    expect(formatMoney(
      { amount_cents: 19_200_00, currency: 'USD' },
      { precision: 'whole' },
    )).toBe('$19,200');
    expect(formatMoney(
      { amount_cents: 4_250, currency: 'USD' },
      { precision: 'whole' },
    )).toBe('$43');
    expect(formatMoney(
      { amount_cents: 99, currency: 'USD' },
      { precision: 'whole' },
    )).toBe('$1');
  });

  test('accounting style still keeps cents under auto', () => {
    expect(formatMoney(
      { amount_cents: 19_200_00, currency: 'USD' },
      { style: 'accounting' },
    )).toBe('19,200.00 USD');
    expect(formatMoney(
      { amount_cents: -19_200_00, currency: 'USD' },
      { style: 'accounting' },
    )).toBe('(19,200.00) USD');
  });

  test('accounting respects an explicit whole precision request', () => {
    expect(formatMoney(
      { amount_cents: 19_200_00, currency: 'USD' },
      { style: 'accounting', precision: 'whole' },
    )).toBe('19,200 USD');
  });

  test('zero-decimal currency (JPY) is unaffected by precision', () => {
    expect(formatMoney({ amount_cents: 1_234, currency: 'JPY' })).toBe('¥1,234 JPY');
    expect(formatMoney(
      { amount_cents: 1_234, currency: 'JPY' },
      { precision: 'cents' },
    )).toBe('¥1,234 JPY');
  });

  test('negatives round consistently with positives', () => {
    expect(formatMoney({ amount_cents: -19_200_00, currency: 'USD' })).toBe('-$19,200');
    expect(formatMoney({ amount_cents: -4_250, currency: 'USD' })).toBe('-$42.50');
  });
});

// `'compact'` is the stat-tile precision: the glance surfaces care about
// the order of magnitude, not the cents. Two hand-rolled versions of it
// existed before this — ExecPage's tiered `fmtUsd` and AccountPage's
// `kFmt`, which divided by 100_000 unconditionally and therefore
// rendered every amount under $500 as the useless "$0K".
describe('formatMoney compact precision', () => {
  test('millions carry one decimal', () => {
    expect(formatMoney(
      { amount_cents: 1_250_000_00, currency: 'USD' },
      { precision: 'compact' },
    )).toBe('$1.3M');
    expect(formatMoney(
      { amount_cents: 1_000_000_00, currency: 'USD' },
      { precision: 'compact' },
    )).toBe('$1.0M');
  });

  test('thousands are whole', () => {
    expect(formatMoney(
      { amount_cents: 45_600_00, currency: 'USD' },
      { precision: 'compact' },
    )).toBe('$46K');
    expect(formatMoney(
      { amount_cents: 1_000_00, currency: 'USD' },
      { precision: 'compact' },
    )).toBe('$1K');
  });

  test('below a thousand renders whole units, NOT $0K', () => {
    // The kFmt bug this replaces: $499 became "$0K".
    expect(formatMoney(
      { amount_cents: 499_00, currency: 'USD' },
      { precision: 'compact' },
    )).toBe('$499');
    expect(formatMoney(
      { amount_cents: 0, currency: 'USD' },
      { precision: 'compact' },
    )).toBe('$0');
  });

  test('negatives keep the sign at every tier', () => {
    expect(formatMoney(
      { amount_cents: -1_250_000_00, currency: 'USD' },
      { precision: 'compact' },
    )).toBe('-$1.3M');
    expect(formatMoney(
      { amount_cents: -45_600_00, currency: 'USD' },
      { precision: 'compact' },
    )).toBe('-$46K');
  });

  test('accounting style parenthesises a compact negative', () => {
    expect(formatMoney(
      { amount_cents: -45_600_00, currency: 'USD' },
      { style: 'accounting', precision: 'compact' },
    )).toBe('(46K) USD');
  });

  test('zero-decimal currency compacts on its own units', () => {
    // JPY stores whole yen, so 1_250_000 IS ¥1.25M — no /100 first.
    expect(formatMoney(
      { amount_cents: 1_250_000, currency: 'JPY' },
      { precision: 'compact' },
    )).toBe('¥1.3M JPY');
  });
});
