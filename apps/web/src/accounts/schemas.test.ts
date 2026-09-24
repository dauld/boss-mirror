import { describe, expect, test } from 'bun:test';
import { AccountSchema, RiskScoreListSchema } from './schemas';

// The crash this schema exists to stop: WatchlistPage cast the payload
// and read `.length` off the result, so a response without `accounts`
// made `scores` undefined and the page died with
// "Cannot read properties of undefined (reading 'length')"
// under the route-smoke suite's adversarial mock (feedback 2fe1c8c1).

const factors = {
  days_since_last_invoice: 40,
  open_ticket_count: 2,
  has_active_contract: true,
  days_since_last_note: null,
};
const row = {
  account_id: 'acct-1',
  account_name: 'Algedonic Ales',
  score: 72,
  top_factor: 'days_since_last_invoice',
  factors,
};
const ok = { accounts: [row] };

describe('RiskScoreListSchema', () => {
  test('accepts a well-formed payload', () => {
    const r = RiskScoreListSchema.safeParse(ok);
    expect(r.success).toBe(true);
  });

  test('REFUSES a payload with no accounts key — the crash', () => {
    expect(RiskScoreListSchema.safeParse({}).success).toBe(false);
    expect(RiskScoreListSchema.safeParse({ data: [] }).success).toBe(false);
  });

  test('refuses accounts that is not an array', () => {
    expect(RiskScoreListSchema.safeParse({ accounts: null }).success).toBe(false);
    expect(RiskScoreListSchema.safeParse({ accounts: 'nope' }).success).toBe(false);
  });

  /** A score the table sorts on must be a NUMBER; a string sorts wrong
   *  and silently, which is worse than refusing. */
  test('refuses a non-numeric score', () => {
    const bad = { accounts: [{ ...row, score: 'high' }] };
    expect(RiskScoreListSchema.safeParse(bad).success).toBe(false);
  });

  /** Backlog 4b981df2 (page audit 08b0c4f8 GAP 10): this test ACCEPTED
   *  a row without `factors`, while WatchlistPage dereferences
   *  `s.factors.*` in its sort and its table with no guard — so such a
   *  row parsed clean and then threw a TypeError in render, past the
   *  try that sets the error state. The server always sends all four
   *  keys (boss-accounts `RiskFactors`, and a prediction without
   *  factors is a 500 there), so a row missing one is a wrong shape and
   *  is REFUSED here, where the page turns a refusal into its error
   *  state. */
  test('REFUSES a row without factors — the page reads them unguarded', () => {
    const noFactors = Object.fromEntries(Object.entries(row).filter(([k]) => k !== 'factors'));
    expect(RiskScoreListSchema.safeParse({ accounts: [noFactors] }).success).toBe(false);
    const nullFactors = { accounts: [{ ...row, factors: null }] };
    expect(RiskScoreListSchema.safeParse(nullFactors).success).toBe(false);
  });

  test('refuses a factors bag missing a key the page reads', () => {
    for (const key of Object.keys(factors)) {
      const missing = Object.fromEntries(Object.entries(factors).filter(([k]) => k !== key));
      const bad = { accounts: [{ ...row, factors: missing }] };
      expect(RiskScoreListSchema.safeParse(bad).success).toBe(false);
    }
  });

  test('refuses a ticket count that is not a number — it sorts the table', () => {
    const bad = { accounts: [{ ...row, factors: { ...factors, open_ticket_count: '2' } }] };
    expect(RiskScoreListSchema.safeParse(bad).success).toBe(false);
  });

  /** A backend ADDING a signal must not become a hard parse failure:
   *  only the keys the page reads are required. */
  test('tolerates an extended factors bag', () => {
    const extra = { accounts: [{ ...row, factors: { ...factors, brand_new_signal: 1 } }] };
    expect(RiskScoreListSchema.safeParse(extra).success).toBe(true);
  });

  test('an empty account list is valid — zero at-risk accounts is a real answer', () => {
    expect(RiskScoreListSchema.safeParse({ accounts: [] }).success).toBe(true);
  });
});

// Backlog d2c9e79f: the schema pinned `tier` to platinum/gold/silver,
// citing a DB CHECK that 22-accounts.sql does not have — `tier` is
// plain TEXT, and a tier is an (account, tier) Class row a tenant adds
// without a deploy. The account detail validates with this schema, so
// an account on a tenant-added tier refused to load.
describe('AccountSchema tier', () => {
  const account = (tier: unknown) => ({
    id: 'acct-1', name: 'One', director: null, city: null, state: null,
    tier, customer_since: null, territory_rep_id: null,
  });

  test('accepts a tier the seeded trio does not name — the registry decides, not the schema', () => {
    expect(AccountSchema.safeParse(account('bronze')).success).toBe(true);
  });

  test('accepts an untiered account and the seeded tiers', () => {
    for (const t of [null, 'platinum', 'gold', 'silver']) {
      expect(AccountSchema.safeParse(account(t)).success).toBe(true);
    }
  });

  test('still refuses a tier that is not a string', () => {
    expect(AccountSchema.safeParse(account(3)).success).toBe(false);
  });
});
